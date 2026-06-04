//! ④ LSP 会话级 server 池（P3）—— 配置驱动语言→server + 懒启动 + initialize 握手。
//!
//! 语言服务器启动要索引整个工程（数秒~分钟），必须**长驻、跨 agent/回合共享**——所以
//! 池是**会话级**的（在 `session.rs` 建、`Arc` 注入各 agent，类比 `McpClient`），一个
//! server 二进制一个实例（`.ts`/`.js` 共用一个 typescript-language-server）。
//!
//! 可测部分（配置查找 / PATH 探测 / path→uri）走单测；真 `spawn` rust-analyzer +
//! initialize 握手那段薄、靠 dev 机 smoke 验（同当年 MCP smoke）。`LspClient` 的协议
//! 逻辑本身已在 `lsp_client.rs` 用 duplex 假服务器测过。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::Mutex;

use crate::agent::lsp_client::LspClient;

/// 一个语言服务器的启动规格（配置驱动；加语言 = 加一行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerSpec {
    /// 二进制名（也是池里的 key——同一 server 多语言共用一个实例）。
    pub command: &'static str,
    pub args: &'static [&'static str],
    /// LSP `languageId`（didOpen 时报给服务器）。
    pub language_id: &'static str,
}

const RUST: ServerSpec = ServerSpec {
    command: "rust-analyzer",
    args: &[],
    language_id: "rust",
};
const TYPESCRIPT: ServerSpec = ServerSpec {
    command: "typescript-language-server",
    args: &["--stdio"],
    language_id: "typescript",
};
const PYRIGHT: ServerSpec = ServerSpec {
    command: "pyright-langserver",
    args: &["--stdio"],
    language_id: "python",
};
const GOPLS: ServerSpec = ServerSpec {
    command: "gopls",
    args: &[],
    language_id: "go",
};

/// Pick the language server for a file by extension. v1 set: Rust / TS+JS /
/// Python / Go. Returns `None` for unconfigured types (→ P5 degradation).
pub fn server_for_path(path: &str) -> Option<ServerSpec> {
    let ext = Path::new(path).extension().and_then(|e| e.to_str())?;
    match ext {
        "rs" => Some(RUST),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(TYPESCRIPT),
        "py" | "pyi" => Some(PYRIGHT),
        "go" => Some(GOPLS),
        _ => None,
    }
}

/// Is `name` an executable on `$PATH`? Cheap pre-check so we can give a clean
/// "install X" message (P5) instead of a spawn error. MVP: checks for a file of
/// that name in a PATH dir (no execute-bit / Windows `.exe` handling yet).
pub fn binary_on_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// `file://` URI for an absolute path. MVP: no percent-encoding (paths with
/// spaces/unicode are a known gap — harden later).
pub fn path_to_uri(p: &Path) -> String {
    format!("file://{}", p.display())
}

struct PooledServer {
    client: Arc<LspClient>,
    /// Kept so the child lives as long as the pool; `kill_on_drop` stops it when
    /// the session ends.
    _child: tokio::process::Child,
}

/// Session-scoped pool of running language servers. `Clone` is cheap (Arc) so it
/// rides into each agent like `BashRegistry`.
#[derive(Clone)]
pub struct LspPool {
    workspace: Arc<PathBuf>,
    servers: Arc<Mutex<HashMap<&'static str, PooledServer>>>,
    /// Serializes first-time starts so concurrent agents don't double-spawn the
    /// same server. Starts are rare (once per server per session).
    start_lock: Arc<Mutex<()>>,
}

impl LspPool {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let ws = workspace.into();
        // Canonicalize so the `rootUri` we send matches the canonicalized file
        // URIs of queries (on macOS /tmp → /private/tmp would otherwise mismatch,
        // and the server treats the file as outside the loaded project → empty
        // results).
        let ws = std::fs::canonicalize(&ws).unwrap_or(ws);
        Self {
            workspace: Arc::new(ws),
            servers: Arc::new(Mutex::new(HashMap::new())),
            start_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Get the running client for `spec`'s server, starting + initializing it on
    /// first use. `Err` carries a user-facing reason (e.g. "rust-analyzer not
    /// installed") for the P5 degradation/install prompt.
    pub async fn get_or_start(&self, spec: ServerSpec) -> Result<Arc<LspClient>, String> {
        // Fast path: already running.
        if let Some(p) = self.servers.lock().await.get(spec.command) {
            return Ok(p.client.clone());
        }
        // Serialize starts; double-check under the guard.
        let _guard = self.start_lock.lock().await;
        if let Some(p) = self.servers.lock().await.get(spec.command) {
            return Ok(p.client.clone());
        }
        if !binary_on_path(spec.command) {
            return Err(format!(
                "{} is not installed (not on PATH) — install it to enable LSP for this language",
                spec.command
            ));
        }
        let (client, child) = spawn_and_initialize(spec, &self.workspace).await?;
        let client = Arc::new(client);
        self.servers.lock().await.insert(
            spec.command,
            PooledServer {
                client: client.clone(),
                _child: child,
            },
        );
        Ok(client)
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
}

/// Spawn the server process, wire it to an [`LspClient`], and run the
/// `initialize`/`initialized` handshake. **Unverified without the real binary —
/// dev-machine smoke test territory**; the protocol layer it builds on is
/// duplex-tested in `lsp_client.rs`.
async fn spawn_and_initialize(
    spec: ServerSpec,
    workspace: &Path,
) -> Result<(LspClient, tokio::process::Child), String> {
    let mut child = tokio::process::Command::new(spec.command)
        .args(spec.args)
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to launch {}: {e}", spec.command))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "no stdin on language server".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "no stdout on language server".to_string())?;

    let client = LspClient::start(Box::new(stdin), Box::new(stdout));

    let init = json!({
        "processId": null,
        "rootUri": path_to_uri(workspace),
        "clientInfo": { "name": "AiDock", "version": "0.1" },
        "capabilities": {
            "textDocument": {
                "definition": { "dynamicRegistration": false },
                "references": { "dynamicRegistration": false },
                "hover": { "contentFormat": ["plaintext", "markdown"] },
                "publishDiagnostics": { "relatedInformation": false },
                "synchronization": { "didOpen": true, "didChange": false }
            },
            "workspace": { "workspaceFolders": false },
            // Opt into rust-analyzer's readiness notification so we know when it
            // has finished indexing (semantic queries before that return empty).
            "experimental": { "serverStatusNotification": true }
        }
    });
    client
        .request("initialize", init)
        .await
        .map_err(|e| format!("{} initialize failed: {e}", spec.command))?;
    client
        .notify("initialized", json!({}))
        .await
        .map_err(|e| format!("{} initialized notify failed: {e}", spec.command))?;

    // Wait for the server to finish loading/indexing before handing it out, so
    // the first query isn't answered empty. Best-effort: servers that don't emit
    // a readiness signal just hit this cap and we proceed (see `wait_until_ready`).
    client.wait_until_ready(Duration::from_secs(30)).await;

    Ok((client, child))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_for_path_maps_extensions() {
        assert_eq!(server_for_path("src/main.rs").map(|s| s.command), Some("rust-analyzer"));
        assert_eq!(
            server_for_path("app/index.tsx").map(|s| s.command),
            Some("typescript-language-server")
        );
        assert_eq!(
            server_for_path("lib/util.js").map(|s| s.command),
            Some("typescript-language-server")
        );
        assert_eq!(server_for_path("api/main.py").map(|s| s.command), Some("pyright-langserver"));
        assert_eq!(server_for_path("cmd/x.go").map(|s| s.command), Some("gopls"));
        assert_eq!(server_for_path("README.md"), None);
        assert_eq!(server_for_path("noext"), None);
    }

    #[test]
    fn ts_and_js_share_one_server_key() {
        // Pool keys on command, so .ts and .js reuse a single instance.
        assert_eq!(
            server_for_path("a.ts").map(|s| s.command),
            server_for_path("b.js").map(|s| s.command)
        );
    }

    #[test]
    fn binary_detection() {
        assert!(!binary_on_path("definitely-not-a-real-binary-xyz-123"));
        #[cfg(unix)]
        assert!(binary_on_path("sh"), "sh should be on PATH on unix");
    }

    #[test]
    fn uri_format() {
        assert_eq!(path_to_uri(Path::new("/ws/src")), "file:///ws/src");
    }

    #[tokio::test]
    async fn missing_binary_yields_install_message() {
        let pool = LspPool::new(std::env::temp_dir());
        let bogus = ServerSpec {
            command: "definitely-not-a-real-binary-xyz-123",
            args: &[],
            language_id: "x",
        };
        match pool.get_or_start(bogus).await {
            Err(e) => assert!(e.contains("not installed"), "{e}"),
            Ok(_) => panic!("expected an install error for a bogus binary"),
        }
    }
}
