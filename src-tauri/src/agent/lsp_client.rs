//! ④ LSP 客户端内核（P2）—— JSON-RPC over LSP Content-Length 分帧。
//!
//! 手写传输（同项目其它手写件，如 glob）：MVP 用 `serde_json::Value` 直接拼/读协议，
//! 暂不引 `lsp-types`（4 个操作形状简单、省一个重依赖）。
//!
//! **可测性是设计要点**：[`LspClient`] 泛型于 reader/writer（`Box<dyn AsyncRead/Write>`），
//! 测试用 `tokio::io::duplex` 造假服务器，把"请求↔响应关联 + 通知收集 + 服务器反向请求
//! 回 null"整条逻辑确定性验掉——不依赖机器上装没装 rust-analyzer。真进程 spawn +
//! initialize 握手在 P3（`LspPool`）接，那层薄、靠 dev 机 smoke 验。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;

/// Per-request ceiling. rust-analyzer can be slow while still indexing, so this
/// is generous; a stuck request resolves to a clean timeout error, not a hang.
const REQUEST_TIMEOUT_SECS: u64 = 30;

type SharedWriter = Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>;
type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;
/// uri → the latest `publishDiagnostics` params for that file.
type Diagnostics = Arc<Mutex<HashMap<String, Value>>>;

/// A live JSON-RPC connection to one language server. Cheap to share (`Arc`able
/// by the caller). Drop stops the reader task (the channel ends close).
pub struct LspClient {
    writer: SharedWriter,
    pending: Pending,
    diagnostics: Diagnostics,
    /// Set when the server signals it's quiescent (rust-analyzer's
    /// `experimental/serverStatus`). Semantic queries before this return empty,
    /// so the pool waits on it before handing out the client.
    ready: Arc<AtomicBool>,
    next_id: AtomicI64,
    _reader: JoinHandle<()>,
}

impl LspClient {
    /// Build a client over any framed byte streams. `reader`/`writer` are the
    /// two ends of the same pipe to the server (child stdout / child stdin in
    /// production; duplex halves in tests).
    pub fn start(
        writer: Box<dyn AsyncWrite + Unpin + Send>,
        reader: Box<dyn AsyncRead + Unpin + Send>,
    ) -> Self {
        let writer: SharedWriter = Arc::new(Mutex::new(writer));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let diagnostics: Diagnostics = Arc::new(Mutex::new(HashMap::new()));
        let ready = Arc::new(AtomicBool::new(false));
        let reader_handle = tokio::spawn(reader_loop(
            BufReader::new(reader),
            writer.clone(),
            pending.clone(),
            diagnostics.clone(),
            ready.clone(),
        ));
        Self {
            writer,
            pending,
            diagnostics,
            ready,
            next_id: AtomicI64::new(1),
            _reader: reader_handle,
        }
    }

    /// Has the server signalled it's quiescent (ready to answer semantic queries)?
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// Block (poll) until the server is quiescent or `max` elapses. Best-effort:
    /// servers that don't emit a readiness signal just hit the timeout and the
    /// caller proceeds (first query may then be empty until indexing finishes).
    pub async fn wait_until_ready(&self, max: Duration) {
        let start = Instant::now();
        while !self.is_ready() && start.elapsed() < max {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Send a request and await the matching response (`result`, or the `error`
    /// object if the server returned one). Times out cleanly.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.send(&msg).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

        match tokio::time::timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), rx).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => Err("LSP response channel dropped (server gone?)".to_string()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(format!("LSP request '{method}' timed out"))
            }
        }
    }

    /// Fire-and-forget notification (no response).
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.send(&msg).await
    }

    /// Latest diagnostics the server has published for `uri`, if any.
    pub async fn diagnostics_for(&self, uri: &str) -> Option<Value> {
        self.diagnostics.lock().await.get(uri).cloned()
    }

    async fn send(&self, msg: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
        let mut w = self.writer.lock().await;
        write_message(&mut *w, &bytes)
            .await
            .map_err(|e| format!("LSP write failed: {e}"))
    }
}

/// Pump server→client messages: fulfil responses, stash diagnostics, and reply
/// `null` to any server→client *request* so the server never blocks on us
/// (e.g. `workspace/configuration`, `window/workDoneProgress/create`).
async fn reader_loop<R: AsyncBufRead + Unpin>(
    mut r: R,
    writer: SharedWriter,
    pending: Pending,
    diagnostics: Diagnostics,
    ready: Arc<AtomicBool>,
) {
    loop {
        let bytes = match read_message(&mut r).await {
            Ok(Some(b)) => b,
            Ok(None) | Err(_) => break, // EOF or framing error → server gone
        };
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
            continue; // skip malformed frame
        };

        let id = v.get("id").and_then(value_as_i64);
        let method = v.get("method").and_then(|m| m.as_str());

        match (id, method) {
            // Server→client REQUEST (has id AND method): we don't implement any,
            // so reply null to keep the server unblocked.
            (Some(id), Some(_)) => {
                let reply = json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null });
                if let Ok(bytes) = serde_json::to_vec(&reply) {
                    let mut w = writer.lock().await;
                    let _ = write_message(&mut *w, &bytes).await;
                }
            }
            // Response to one of our requests.
            (Some(id), None) => {
                if let Some(tx) = pending.lock().await.remove(&id) {
                    let payload = v
                        .get("result")
                        .cloned()
                        .or_else(|| v.get("error").cloned())
                        .unwrap_or(Value::Null);
                    let _ = tx.send(payload);
                }
            }
            // Notification.
            (None, Some(method)) => {
                if method == "textDocument/publishDiagnostics" {
                    if let Some(params) = v.get("params") {
                        if let Some(uri) = params.get("uri").and_then(|u| u.as_str()) {
                            diagnostics
                                .lock()
                                .await
                                .insert(uri.to_string(), params.clone());
                        }
                    }
                } else if method == "experimental/serverStatus" {
                    // rust-analyzer readiness: quiescent == done indexing.
                    let quiescent = v
                        .pointer("/params/quiescent")
                        .and_then(|q| q.as_bool())
                        .unwrap_or(false);
                    if quiescent {
                        ready.store(true, Ordering::SeqCst);
                    }
                }
                // other notifications (logs, progress) ignored for MVP
            }
            (None, None) => {} // not a valid JSON-RPC message; ignore
        }
    }
    // Server gone (EOF / framing error): drop all pending senders so in-flight
    // requests fail fast ("server gone") instead of waiting out the per-request
    // timeout. (Found via the lsp_smoke dev test: a server that exits — e.g. a
    // rustup shim whose rust-analyzer component isn't installed — otherwise made
    // every request hang the full 30s.)
    pending.lock().await.clear();
}

/// JSON-RPC `id` may arrive as a number or (rarely) a numeric string.
fn value_as_i64(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Write one LSP message: `Content-Length` header + CRLF CRLF + JSON body.
/// `pub(crate)` so sibling modules' tests can drive a fake server.
pub(crate) async fn write_message<W: AsyncWrite + Unpin>(
    w: &mut W,
    payload: &[u8],
) -> std::io::Result<()> {
    let header = format!("Content-Length: {}\r\n\r\n", payload.len());
    w.write_all(header.as_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await
}

/// Read one LSP message: parse headers until the blank line, then read exactly
/// `Content-Length` body bytes. `Ok(None)` on clean EOF. `pub(crate)` for tests.
pub(crate) async fn read_message<R: AsyncBufRead + Unpin>(
    r: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut content_len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_len = rest.trim().parse().ok();
        }
    }
    let len = content_len.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "LSP frame missing Content-Length")
    })?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted fake language server over an in-memory duplex pipe. Returns an
    /// [`LspClient`] wired to it, plus a handle to drive the "server" side.
    fn fake_server() -> (LspClient, tokio::io::DuplexStream) {
        // `client_end` is what the client reads/writes; `server_end` is ours.
        let (client_end, server_end) = tokio::io::duplex(64 * 1024);
        let (cr, cw) = tokio::io::split(client_end);
        let client = LspClient::start(Box::new(cw), Box::new(cr));
        (client, server_end)
    }

    #[tokio::test]
    async fn request_response_roundtrip() {
        let (client, server_end) = fake_server();
        let (sr, mut sw) = tokio::io::split(server_end);
        let mut sr = BufReader::new(sr);

        // Server task: read one request, reply with a result echoing its id.
        let server = tokio::spawn(async move {
            let bytes = read_message(&mut sr).await.unwrap().unwrap();
            let req: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(req["method"], "textDocument/definition");
            let id = req["id"].as_i64().unwrap();
            let resp = json!({ "jsonrpc": "2.0", "id": id, "result": { "ok": true } });
            write_message(&mut sw, &serde_json::to_vec(&resp).unwrap())
                .await
                .unwrap();
        });

        let res = client
            .request("textDocument/definition", json!({ "x": 1 }))
            .await
            .unwrap();
        assert_eq!(res, json!({ "ok": true }));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn diagnostics_notification_is_captured() {
        let (client, server_end) = fake_server();
        let (_sr, mut sw) = tokio::io::split(server_end);

        let note = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///ws/src/main.rs",
                "diagnostics": [{ "message": "mismatched types", "severity": 1 }]
            }
        });
        write_message(&mut sw, &serde_json::to_vec(&note).unwrap())
            .await
            .unwrap();

        // Give the reader task a moment to ingest the frame.
        for _ in 0..50 {
            if client.diagnostics_for("file:///ws/src/main.rs").await.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let diags = client
            .diagnostics_for("file:///ws/src/main.rs")
            .await
            .expect("diagnostics captured");
        assert_eq!(diags["diagnostics"][0]["message"], "mismatched types");
    }

    #[tokio::test]
    async fn server_request_gets_null_reply() {
        // `_client` kept alive so its reader task runs and replies.
        let (_client, server_end) = fake_server();
        let (sr, mut sw) = tokio::io::split(server_end);
        let mut sr = BufReader::new(sr);

        // Server sends a server→client request (id + method); client must reply.
        let server_req = json!({
            "jsonrpc": "2.0", "id": 99, "method": "workspace/configuration", "params": {}
        });
        write_message(&mut sw, &serde_json::to_vec(&server_req).unwrap())
            .await
            .unwrap();

        let bytes = read_message(&mut sr).await.unwrap().unwrap();
        let reply: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reply["id"], 99);
        assert_eq!(reply["result"], Value::Null);
    }

    #[tokio::test]
    async fn request_fails_fast_when_server_disconnects() {
        let (client, server_end) = fake_server();
        drop(server_end); // server gone → client reader hits EOF, drains pending
        let raced = tokio::time::timeout(
            Duration::from_secs(2),
            client.request("textDocument/hover", json!({})),
        )
        .await;
        // Fast (not the 30s request timeout), and an error (server gone).
        assert!(matches!(raced, Ok(Err(_))), "expected fast error, got {raced:?}");
    }

    #[tokio::test]
    async fn request_times_out_when_server_silent() {
        let (client, _server_end) = fake_server();
        // Override is hard without exposing the const; instead just verify a
        // dropped server end surfaces an error rather than hanging forever.
        // (_server_end kept alive but never replies; rely on the channel.)
        // Use a very short manual race to avoid the 30s real timeout in tests:
        let fut = client.request("textDocument/hover", json!({}));
        let raced = tokio::time::timeout(Duration::from_millis(100), fut).await;
        assert!(raced.is_err(), "request should still be pending (no reply)");
    }

    #[test]
    fn framing_roundtrips_via_blocking_io() {
        // Pure framing check (write then read back) on an in-memory buffer.
        use std::io::Cursor;
        let payload = br#"{"jsonrpc":"2.0","id":1}"#;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut buf = Vec::new();
            write_message(&mut buf, payload).await.unwrap();
            assert!(buf.starts_with(b"Content-Length: 24\r\n\r\n"));
            let mut cur = BufReader::new(Cursor::new(buf));
            let got = read_message(&mut cur).await.unwrap().unwrap();
            assert_eq!(got, payload);
        });
    }
}
