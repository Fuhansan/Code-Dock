//! Append-only log of MCP tool calls.
//!
//! Sprint 4.5 only emitted MCP call events as a live Tauri event stream;
//! reload landed in an empty chat for the tool-call cards. This module
//! adds a parallel persistence path so the call log survives restart the
//! same way `messages.jsonl` does.
//!
//! Architecture: a single writer task per session reads `ToolCallEvent`s
//! off an mpsc channel and appends each as JSONL. Every agent runtime
//! holds a clone of the channel sender (cheap, `mpsc::Sender` is Clone)
//! and records every call there, in addition to the Tauri event emit
//! for the live UI.
//!
//! Why a dedicated task instead of shared file handle + Mutex:
//! - Append is naturally serialised through the channel, no per-call lock.
//! - Backpressure: if disk gets slow the channel buffers; agents don't
//!   block in their tool-call hot path.
//! - The writer task is the only thing that touches the file, so error
//!   handling is centralised.

use std::path::PathBuf;

use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::agent::mcp::ToolCallEvent;

const CHANNEL_CAPACITY: usize = 256;
pub const MCP_LOG_FILE: &str = "mcp_calls.jsonl";

/// Cheap clone handle agent runtimes use to record one call. Internally
/// just an `mpsc::Sender`.
#[derive(Clone)]
pub struct McpLogHandle {
    tx: mpsc::Sender<ToolCallEvent>,
}

impl McpLogHandle {
    /// Record one event. Returns immediately unless the channel buffer
    /// is full, in which case it yields. Send failure is logged but
    /// otherwise swallowed — we never want a disk hiccup to wedge an
    /// agent task.
    pub async fn record(&self, event: ToolCallEvent) {
        if let Err(e) = self.tx.send(event).await {
            tracing::error!(
                target: "aidock::mcp_log",
                error = %e,
                "could not enqueue MCP call for persistence"
            );
        }
    }
}

/// Open the log at `path` and spawn the writer task. Returns the handle
/// agents will clone, plus the task's `JoinHandle` (the session keeps it
/// so the task isn't dropped immediately).
pub async fn spawn_writer(path: PathBuf) -> std::io::Result<(McpLogHandle, JoinHandle<()>)> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;

    let (tx, mut rx) = mpsc::channel::<ToolCallEvent>(CHANNEL_CAPACITY);
    let task = tokio::spawn(async move {
        tracing::info!(
            target: "aidock::mcp_log",
            path = %path.display(),
            "mcp_calls.jsonl writer started"
        );
        while let Some(event) = rx.recv().await {
            let line = match serde_json::to_string(&event) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        target: "aidock::mcp_log",
                        error = %e,
                        "could not serialise MCP call event; dropping"
                    );
                    continue;
                }
            };
            if let Err(e) = file.write_all(line.as_bytes()).await {
                tracing::error!(target: "aidock::mcp_log", error = %e, "write failed");
                continue;
            }
            if let Err(e) = file.write_all(b"\n").await {
                tracing::error!(target: "aidock::mcp_log", error = %e, "write newline failed");
                continue;
            }
            if let Err(e) = file.flush().await {
                tracing::error!(target: "aidock::mcp_log", error = %e, "flush failed");
            }
        }
        tracing::info!(target: "aidock::mcp_log", "mcp_calls.jsonl writer shutting down");
    });

    Ok((McpLogHandle { tx }, task))
}

/// Read every parseable line of an MCP-call JSONL file. Matches the
/// behaviour of `persistence::read_jsonl_messages` — malformed tail-lines
/// are skipped with a warning so a crash never blocks reload.
pub fn read_mcp_calls(path: &std::path::Path) -> std::io::Result<Vec<ToolCallEvent>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    let mut bad = 0;
    for (idx, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<ToolCallEvent>(line) {
            Ok(e) => out.push(e),
            Err(e) => {
                bad += 1;
                tracing::warn!(
                    target: "aidock::mcp_log",
                    line = idx + 1,
                    error = %e,
                    "skipping malformed MCP-log line"
                );
            }
        }
    }
    if bad > 0 {
        tracing::warn!(
            target: "aidock::mcp_log",
            bad,
            kept = out.len(),
            "mcp_calls.jsonl had malformed lines"
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::mcp::make_call_event;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);
    fn test_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aidock-mcp-log-{}-{}-{}",
            label,
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn round_trip_two_events() {
        let dir = test_dir("rt");
        let path = dir.join(MCP_LOG_FILE);
        let (handle, task) = spawn_writer(path.clone()).await.unwrap();
        let e1 = make_call_event(
            "PM",
            "fs__read_file",
            r#"{"path":"a.md"}"#,
            "hello",
            1,
        );
        let e2 = make_call_event(
            "frontend_dev",
            "fs__write_file",
            r#"{"path":"a.md","content":"x"}"#,
            "Successfully wrote to a.md",
            2,
        );
        handle.record(e1).await;
        handle.record(e2).await;
        drop(handle);
        // Drop closes the sender; writer drains and exits.
        let _ = task.await;

        let back = read_mcp_calls(&path).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].agent, "PM");
        assert_eq!(back[1].agent, "frontend_dev");
        assert!(back[1].success);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_missing_file_returns_empty() {
        let dir = test_dir("missing");
        let path = dir.join(MCP_LOG_FILE);
        let back = read_mcp_calls(&path).unwrap();
        assert!(back.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_line_skipped() {
        let dir = test_dir("malformed");
        let path = dir.join(MCP_LOG_FILE);
        let good = make_call_event("PM", "fs__read_file", r#"{}"#, "ok", 1);
        let good_line = serde_json::to_string(&good).unwrap();
        let content = format!("{good_line}\n{{ not json\n{good_line}\n");
        std::fs::write(&path, content).unwrap();
        let back = read_mcp_calls(&path).unwrap();
        assert_eq!(back.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
