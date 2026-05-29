//! ④.a — the real [`ActionExecutor`]: the single chokepoint every
//! side-effecting `ToolCall` passes through.
//!
//! This is the **④.d hook** (CLAUDE.md ④.a「④.d 插座」): today it executes
//! directly; later a dry-run / temp-workspace wrap slots in HERE, leaving the
//! loop untouched.
//!
//! Three seams keep it testable without a live MCP server, and each maps onto
//! an existing runtime concern:
//!   - [`ToolBackend`] — the network call (`McpClient::call_tool`)
//!   - [`Approver`]    — the approval gate (`runtime::gate_mcp_call`)
//!   - [`CallSink`]    — the full-body event + log fan-out (MCP_CALL_EVENT /
//!     `mcp_calls.jsonl`). This is the ⑥/① pipe of the two-pipe rule; the
//!     LLM-side summary is the [`LlmSource`](crate::agent::llm_source)'s job,
//!     NOT this one's.
//!
//! Scope note: only `fs__`-prefixed MCP tools route through here. Query /
//! scratchpad tools are ④.b (memory) concerns and get routed when ④.b is
//! wired; for now they return a clear error rather than silently passing.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::agent::mcp::{make_call_event, McpCallEvent, McpClient, MCP_TOOL_PREFIX};
use crate::agent::react::{ActionExecutor, ExecResult};

/// Outcome of the approval gate for one tool call. Mirrors the private enum in
/// `runtime.rs`; lifted here so the executor owns its own seam type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Proceed,
    Rejected(String),
}

/// The network seam: forward a tool call to wherever it actually runs.
pub trait ToolBackend {
    fn call(
        &self,
        tool: &str,
        args: &str,
    ) -> impl std::future::Future<Output = Result<String, String>> + Send;
}

/// The approval seam: decide whether a call may proceed. Read-only tools and
/// session-approved tools return `Proceed` without prompting; the real impl
/// (wired in the runtime) drives the user dialog.
pub trait Approver {
    fn decide(
        &self,
        agent: &str,
        tool: &str,
        args: &str,
    ) -> impl std::future::Future<Output = ApprovalOutcome> + Send;
}

/// The fan-out seam: record the full-fidelity call event (→ UI via Tauri, →
/// `mcp_calls.jsonl` on disk). Full body, no token cost. Async because the
/// JSONL writer is.
pub trait CallSink {
    fn record(&self, event: McpCallEvent) -> impl std::future::Future<Output = ()> + Send;
}

// ---------------------------------------------------------------------------
// Headless defaults + the real network adapter.
// ---------------------------------------------------------------------------

/// Approver that lets everything through. Headless / test default — the
/// runtime swaps in the real user-prompting gate.
pub struct AllowAll;

impl Approver for AllowAll {
    async fn decide(&self, _agent: &str, _tool: &str, _args: &str) -> ApprovalOutcome {
        ApprovalOutcome::Proceed
    }
}

/// Sink that drops every event. Headless default.
pub struct NullCallSink;

impl CallSink for NullCallSink {
    async fn record(&self, _event: McpCallEvent) {}
}

/// Real network backend: forwards to a live [`McpClient`].
pub struct McpClientBackend {
    pub client: McpClient,
}

impl ToolBackend for McpClientBackend {
    async fn call(&self, tool: &str, args: &str) -> Result<String, String> {
        self.client.call_tool(tool, args).await.map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// The executor.
// ---------------------------------------------------------------------------

pub struct McpExecutor<B, A, S> {
    backend: B,
    approver: A,
    sink: S,
    agent: String,
}

impl<B, A, S> McpExecutor<B, A, S>
where
    B: ToolBackend + Send + Sync,
    A: Approver + Send + Sync,
    S: CallSink + Send + Sync,
{
    pub fn new(agent: impl Into<String>, backend: B, approver: A, sink: S) -> Self {
        Self {
            backend,
            approver,
            sink,
            agent: agent.into(),
        }
    }
}

impl<B, A, S> ActionExecutor for McpExecutor<B, A, S>
where
    B: ToolBackend + Send + Sync,
    A: Approver + Send + Sync,
    S: CallSink + Send + Sync,
{
    async fn execute(&mut self, tool: &str, args: &str) -> ExecResult {
        if !tool.starts_with(MCP_TOOL_PREFIX) {
            // Query / scratchpad tools belong to ④.b and aren't wired yet.
            return ExecResult {
                raw: format!(
                    "ERROR: '{tool}' is not an MCP tool; only {MCP_TOOL_PREFIX}* tools route through the executor for now"
                ),
                success: false,
            };
        }

        let body = match self.approver.decide(&self.agent, tool, args).await {
            ApprovalOutcome::Rejected(reason) => format!("DENIED: {reason}"),
            ApprovalOutcome::Proceed => match self.backend.call(tool, args).await {
                Ok(b) => b,
                Err(e) => format!("ERROR: MCP tool '{tool}' failed: {e}"),
            },
        };

        let success = classify_success(&body);

        // Full body → UI/⑥. We override the event's success flag so DENIED
        // (which make_call_event would otherwise read as success, since it
        // only keys on "ERROR:") shows correctly as a non-success call.
        let mut event = make_call_event(&self.agent, tool, args, &body, now_ms());
        event.success = success;
        self.sink.record(event).await;

        ExecResult { raw: body, success }
    }
}

/// A tool result counts as success unless it's an error or a denial. Drives
/// the loop's ④.c judge + the consecutive-failure backstop.
fn classify_success(body: &str) -> bool {
    let t = body.trim_start();
    !(t.starts_with("ERROR:") || t.starts_with("DENIED:"))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests — gate routing, success classification, event fan-out, all without a
// live MCP server.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    struct FakeBackend {
        result: Result<String, String>,
        called: AtomicBool,
    }

    impl FakeBackend {
        fn ok(body: &str) -> Self {
            Self {
                result: Ok(body.to_string()),
                called: AtomicBool::new(false),
            }
        }
        fn err(msg: &str) -> Self {
            Self {
                result: Err(msg.to_string()),
                called: AtomicBool::new(false),
            }
        }
    }

    impl ToolBackend for FakeBackend {
        async fn call(&self, _tool: &str, _args: &str) -> Result<String, String> {
            self.called.store(true, Ordering::SeqCst);
            self.result.clone()
        }
    }

    struct FixedApprover(ApprovalOutcome);
    impl Approver for FixedApprover {
        async fn decide(&self, _a: &str, _t: &str, _g: &str) -> ApprovalOutcome {
            self.0.clone()
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<McpCallEvent>>,
    }
    impl CallSink for RecordingSink {
        async fn record(&self, event: McpCallEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn success_call_returns_body_and_emits_success_event() {
        let mut ex = McpExecutor::new(
            "frontend_dev",
            FakeBackend::ok("[FILE] index.html"),
            AllowAll,
            RecordingSink::default(),
        );
        let res = ex.execute("fs__list_directory", r#"{"path":"."}"#).await;

        assert!(res.success);
        assert_eq!(res.raw, "[FILE] index.html");
        let evs = ex.sink.events.lock().unwrap();
        assert_eq!(evs.len(), 1);
        assert!(evs[0].success);
        assert_eq!(evs[0].result, "[FILE] index.html"); // full body in the event
    }

    #[tokio::test]
    async fn backend_error_is_failure() {
        let mut ex = McpExecutor::new(
            "backend_dev",
            FakeBackend::err("connection reset"),
            AllowAll,
            RecordingSink::default(),
        );
        let res = ex.execute("fs__read_file", r#"{"path":"a"}"#).await;

        assert!(!res.success);
        assert!(res.raw.starts_with("ERROR:"));
        assert!(!ex.sink.events.lock().unwrap()[0].success);
    }

    #[tokio::test]
    async fn rejected_call_never_hits_the_backend() {
        let backend = FakeBackend::ok("should not run");
        let mut ex = McpExecutor::new(
            "PM",
            backend,
            FixedApprover(ApprovalOutcome::Rejected("user said no".into())),
            RecordingSink::default(),
        );
        let res = ex.execute("fs__write_file", r#"{"path":"x","content":"y"}"#).await;

        assert!(!res.success);
        assert!(res.raw.starts_with("DENIED:"));
        assert!(!ex.backend.called.load(Ordering::SeqCst), "backend must not run on reject");
        // Denial is still surfaced to the UI as a (non-success) call.
        assert!(!ex.sink.events.lock().unwrap()[0].success);
    }

    #[tokio::test]
    async fn non_mcp_tool_is_rejected_without_side_effects() {
        let backend = FakeBackend::ok("nope");
        let mut ex = McpExecutor::new("PM", backend, AllowAll, RecordingSink::default());
        let res = ex.execute("recall_topic", r#"{"q":"x"}"#).await;

        assert!(!res.success);
        assert!(res.raw.contains("not an MCP tool"));
        assert!(!ex.backend.called.load(Ordering::SeqCst));
        assert!(ex.sink.events.lock().unwrap().is_empty(), "no event for a non-call");
    }
}
