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
//!   - [`CallSink`]    — the full-body event + log fan-out (TOOL_CALL_EVENT /
//!     `mcp_calls.jsonl`). This is the ⑥/① pipe of the two-pipe rule; the
//!     LLM-side summary is the [`LlmSource`](crate::agent::llm_source)'s job,
//!     NOT this one's.
//!
//! Scope note: only `fs__`-prefixed MCP tools route through here. Query /
//! scratchpad tools are ④.b (memory) concerns and get routed when ④.b is
//! wired; for now they return a clear error rather than silently passing.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::agent::mcp::{is_mcp_tool, make_call_event, McpClient, ToolCallEvent, MCP_TOOL_PREFIX};
use crate::agent::memory::{execute_recall_detail, is_recall_detail, MemoryStore};
use crate::agent::message::AgentMessage;
use crate::agent::react::{ActionExecutor, ExecResult};
use crate::agent::recall::{execute_query_tool, is_query_tool};
use crate::agent::scratchpad::{apply_update, is_scratchpad_tool, Scratchpad};
use crate::agent::fs_tools::{is_file_tool, FileTools};
use crate::agent::lsp::{execute_lsp, is_lsp_tool};
use crate::agent::lsp_pool::LspPool;
use crate::agent::security::{
    classify_level, decide, match_rules, Decision, PermissionRule, RuleVerdict, SecurityCtx,
};
use crate::agent::shell::{is_bash_tool, BashTool};
use crate::agent::tools::SecurityLevel;

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
    fn record(&self, event: ToolCallEvent) -> impl std::future::Future<Output = ()> + Send;
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
    async fn record(&self, _event: ToolCallEvent) {}
}

/// Real network backend: forwards to a live [`McpClient`]. `None` when the
/// filesystem server didn't start this session — fs calls then fail cleanly
/// instead of panicking.
pub struct McpClientBackend {
    pub client: Option<McpClient>,
}

impl ToolBackend for McpClientBackend {
    async fn call(&self, tool: &str, args: &str) -> Result<String, String> {
        match &self.client {
            Some(c) => c.call_tool(tool, args).await.map_err(|e| e.to_string()),
            None => Err(format!(
                "MCP server not available this session — '{tool}' can't run"
            )),
        }
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

    /// Run the approval gate for `tool` as this agent. The unified ④.d security
    /// check (in [`CompositeExecutor`]) calls this on an `Ask` decision so every
    /// Call tool shares one approval channel (`APPROVAL_REQUEST_EVENT`).
    pub async fn approve(&self, tool: &str, args: &str) -> ApprovalOutcome {
        self.approver.decide(&self.agent, tool, args).await
    }

    /// Emit a full-fidelity tool-call event through the sink (→ UI / ⑥). step 5:
    /// the [`CompositeExecutor`] calls this for native/internal Call tools so they
    /// surface in the UI the same way MCP calls do (two-pipe ⑥ side, no LLM cost).
    pub async fn emit(&self, tool: &str, args: &str, body: &str, success: bool) {
        let mut event = make_call_event(&self.agent, tool, args, body, now_ms());
        event.success = success;
        self.sink.record(event).await;
    }

    /// Backend call ONLY — no approval, no event. The unified ④.d security check
    /// + the shared event emission live in [`CompositeExecutor`] now (step 5
    /// cleanup: MCP isn't special). For a non-MCP name this errors cleanly.
    pub async fn call_backend(&self, tool: &str, args: &str) -> ExecResult {
        if !crate::agent::mcp::is_mcp_tool(tool) {
            return ExecResult {
                raw: format!("ERROR: '{tool}' is not a known tool"),
                success: false,
            };
        }
        let body = match self.backend.call(tool, args).await {
            Ok(b) => b,
            Err(e) => format!("ERROR: tool '{tool}' failed: {e}"),
        };
        let success = classify_success(&body);
        ExecResult { raw: body, success }
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
// CompositeExecutor — routes a ToolCall to the right handler so the live
// runtime keeps recall / scratchpad working alongside MCP (CLAUDE.md #2b).
//
// Routing:
//   - fs__*                          → McpExecutor (the ④.d chokepoint)
//   - recall_topic / search_topic    → execute_query_tool (read-only, on a
//                                       turn-start history snapshot)
//   - update_scratchpad              → apply_update + persist
//
// Scope note: query / scratchpad are really ④.b (memory) concerns living here
// transitionally. When ④.b lands they migrate out and this shrinks back to a
// thin MCP wrapper. The history snapshot is read-only and taken at turn start —
// fine because recall targets *closed* topics, which don't change mid-turn.
// ---------------------------------------------------------------------------

pub struct CompositeExecutor<B, A, S> {
    mcp: McpExecutor<B, A, S>,
    history: Vec<AgentMessage>,
    scratchpad: Scratchpad,
    scratchpad_path: PathBuf,
    memory: MemoryStore,
    bash: BashTool,
    files: FileTools,
    /// ④.d 安全检查：角色策略 + 工作区间根（算危险级 + 决定放行/问人/拒）。
    security_level: SecurityLevel,
    workspace: PathBuf,
    /// specifier 规则（CLAUDE.md ④.d）：级别策略之前的短路，Deny 优先 / Allow 免问。
    permission_rules: Vec<PermissionRule>,
    /// ④ LSP 会话级 server 池（共享 Arc）。LSP 工具查询经它取/起语言服务器。
    lsp_pool: LspPool,
}

impl<B, A, S> CompositeExecutor<B, A, S> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mcp: McpExecutor<B, A, S>,
        history: Vec<AgentMessage>,
        scratchpad: Scratchpad,
        scratchpad_path: PathBuf,
        memory: MemoryStore,
        bash: BashTool,
        files: FileTools,
        security_level: SecurityLevel,
        workspace: PathBuf,
        permission_rules: Vec<PermissionRule>,
        lsp_pool: LspPool,
    ) -> Self {
        Self {
            mcp,
            history,
            scratchpad,
            scratchpad_path,
            memory,
            bash,
            files,
            security_level,
            workspace,
            permission_rules,
            lsp_pool,
        }
    }

    /// Reclaim the (possibly mutated) scratchpad when the turn ends, so the
    /// runtime can carry it into the next turn.
    pub fn into_scratchpad(self) -> Scratchpad {
        self.scratchpad
    }
}

impl<B, A, S> ActionExecutor for CompositeExecutor<B, A, S>
where
    B: ToolBackend + Send + Sync,
    A: Approver + Send + Sync,
    S: CallSink + Send + Sync,
{
    async fn execute(&mut self, tool: &str, args: &str) -> ExecResult {
        // ④.d 安全检查（step 4 + step 5 收口）：危险族——文件改/删 + Bash + MCP——
        // 过**同一道**关卡（MCP 不再特殊：自带审批已退役，走这里）。内部记忆/查询恒
        // L0、不过门。被拒也发一条事件，UI 能看到"被拦的尝试"。
        if is_file_tool(tool) || is_bash_tool(tool) || is_mcp_tool(tool) {
            let ctx = SecurityCtx {
                workspace: self.workspace.clone(),
            };
            // specifier 规则先短路（Deny 优先 / Allow 免问）；都不命中才落级别策略。
            let rule = match_rules(tool, args, &self.permission_rules);
            let decision = match rule {
                Some(RuleVerdict::Deny) => Decision::Deny,
                Some(RuleVerdict::Allow) => Decision::Allow,
                None => decide(classify_level(tool, args, &ctx), self.security_level),
            };
            match decision {
                Decision::Deny => {
                    let reason = if matches!(rule, Some(RuleVerdict::Deny)) {
                        format!("'{tool}' matches a deny rule")
                    } else {
                        format!(
                            "'{tool}' writes/deletes outside the workspace (L3 red line — never allowed)"
                        )
                    };
                    let res = ExecResult {
                        raw: format!("DENIED: {reason}"),
                        success: false,
                    };
                    self.mcp.emit(tool, args, &res.raw, res.success).await;
                    return res;
                }
                Decision::Ask => {
                    if let ApprovalOutcome::Rejected(reason) = self.mcp.approve(tool, args).await {
                        let res = ExecResult {
                            raw: format!("DENIED: {reason}"),
                            success: false,
                        };
                        self.mcp.emit(tool, args, &res.raw, res.success).await;
                        return res;
                    }
                }
                Decision::Allow => {}
            }
        }

        // Dispatch to the native/internal backend.
        let res = if is_file_tool(tool) {
            // ④ 原生文件工具（step 2）：改/删经路径 confinement 锁工作区间；读可越界。
            self.files.dispatch(tool, args)
        } else if is_bash_tool(tool) {
            // ④ Bash 族（step 3）：无沙箱，cwd=工作区间；前台/后台 + 登记表。
            self.bash.dispatch(tool, args).await
        } else if is_lsp_tool(tool) {
            // ④ LSP（代码语义，L0）：经会话级 `LspPool` 取/起语言服务器；无 server
            // 时降级到 Grep/Bash 引导（P4+P5）。
            execute_lsp(&self.lsp_pool, args).await
        } else if is_recall_detail(tool) {
            // ④.b: pull a step's full detail from this agent's memory.
            let raw = execute_recall_detail(&self.memory, args);
            let success = !raw.trim_start().starts_with("ERROR:");
            ExecResult { raw, success }
        } else if is_query_tool(tool) {
            match execute_query_tool(tool, args, &self.history) {
                Ok(raw) => ExecResult { raw, success: true },
                Err(e) => ExecResult {
                    raw: format!("ERROR: query '{tool}' failed: {e}"),
                    success: false,
                },
            }
        } else if is_scratchpad_tool(tool) {
            // scratchpad
            match apply_update(&mut self.scratchpad, args) {
                Ok(confirmation) => {
                    // In-memory update is the source of truth; a save failure
                    // is logged but doesn't fail the tool (mirrors old runtime).
                    if let Err(e) = self.scratchpad.save(&self.scratchpad_path) {
                        tracing::error!(
                            target: "aidock::agent",
                            path = %self.scratchpad_path.display(),
                            error = %e,
                            "scratchpad updated in memory but save FAILED"
                        );
                    }
                    ExecResult {
                        raw: confirmation,
                        success: true,
                    }
                }
                Err(e) => ExecResult {
                    raw: format!("ERROR: scratchpad update failed: {e}"),
                    success: false,
                },
            }
        } else {
            // MCP（fs__ / mcp__）或未知名 —— 后端直调；审批 + 事件已在上面统一处理。
            self.mcp.call_backend(tool, args).await
        };

        // step 5（双通道 ⑥/UI 侧）：每个 Call 工具都发一条统一的工具调用事件
        // （全量 body，无 LLM token 成本）——UI 因此能看到文件/Bash/recall 的调用卡。
        self.mcp.emit(tool, args, &res.raw, res.success).await;
        res
    }
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
        events: Mutex<Vec<ToolCallEvent>>,
    }
    impl CallSink for RecordingSink {
        async fn record(&self, event: ToolCallEvent) {
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

    // --- CompositeExecutor routing ----------------------------------------

    use std::sync::atomic::AtomicU32;
    static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

    fn temp_scratchpad_path() -> PathBuf {
        let n = TMP_SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("aidock-test-sp-{}-{n}.json", std::process::id()))
    }

    fn composite() -> CompositeExecutor<FakeBackend, AllowAll, RecordingSink> {
        let mcp = McpExecutor::new(
            "frontend_dev",
            FakeBackend::ok("[FILE] index.html"),
            AllowAll,
            RecordingSink::default(),
        );
        let mem = MemoryStore::new(std::env::temp_dir().join(format!(
            "aidock-comp-mem-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::SeqCst)
        )));
        CompositeExecutor::new(
            mcp,
            Vec::new(),
            Scratchpad::default(),
            temp_scratchpad_path(),
            mem,
            BashTool::new(
                std::env::temp_dir(),
                false,
                crate::agent::shell::BashRegistry::new(),
            ),
            FileTools::new(std::env::temp_dir()),
            SecurityLevel::Standard,
            std::env::temp_dir(),
            Vec::new(),
            LspPool::new(std::env::temp_dir()),
        )
    }

    #[tokio::test]
    async fn composite_routes_fs_to_mcp() {
        let mut ex = composite();
        let res = ex.execute("fs__list_directory", r#"{"path":"."}"#).await;
        assert!(res.success);
        assert_eq!(res.raw, "[FILE] index.html");
    }

    #[tokio::test]
    async fn composite_routes_query_tool() {
        let mut ex = composite();
        // Empty history: recall returns a (non-error) "nothing found"-style body.
        let res = ex.execute("recall_topic", r#"{"topic_id":"t-1"}"#).await;
        assert!(res.success, "query should succeed even on empty history: {}", res.raw);
    }

    #[tokio::test]
    async fn composite_routes_scratchpad_and_mutates() {
        let mut ex = composite();
        let res = ex
            .execute("update_scratchpad", r#"{"current_focus":"build login"}"#)
            .await;
        assert!(res.success);
        let pad = ex.into_scratchpad();
        assert_eq!(pad.current_focus, "build login");
    }

    #[tokio::test]
    async fn composite_security_check_denies_out_of_workspace_edit() {
        // ④.d: classify_level → L3 → Deny, before FileTools ever runs.
        let mut ex = composite();
        let res = ex
            .execute(
                "Edit",
                r#"{"file_path":"/etc/passwd","old_string":"a","new_string":"b"}"#,
            )
            .await;
        assert!(!res.success);
        assert!(res.raw.starts_with("DENIED:"), "{}", res.raw);
        assert!(res.raw.contains("L3"));
    }

    #[tokio::test]
    async fn composite_unified_gate_gates_mcp_too() {
        // step 5 收口：MCP 工具(fs__)现在过 CompositeExecutor 的统一安全检查
        // （L2→问人），而不是 McpExecutor 自带审批。审批拒绝 → 后端不被触达。
        let mcp = McpExecutor::new(
            "PM",
            FakeBackend::ok("should not run"),
            FixedApprover(ApprovalOutcome::Rejected("user said no".into())),
            RecordingSink::default(),
        );
        let mem = MemoryStore::new(std::env::temp_dir().join(format!(
            "aidock-comp-mem-rej-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::SeqCst)
        )));
        let mut ex = CompositeExecutor::new(
            mcp,
            Vec::new(),
            Scratchpad::default(),
            temp_scratchpad_path(),
            mem,
            BashTool::new(
                std::env::temp_dir(),
                false,
                crate::agent::shell::BashRegistry::new(),
            ),
            FileTools::new(std::env::temp_dir()),
            SecurityLevel::Standard,
            std::env::temp_dir(),
            Vec::new(),
            LspPool::new(std::env::temp_dir()),
        );
        let res = ex
            .execute("fs__write_file", r#"{"path":"x","content":"y"}"#)
            .await;
        assert!(!res.success);
        assert!(res.raw.starts_with("DENIED:"), "{}", res.raw);
        assert!(
            !ex.mcp.backend.called.load(Ordering::SeqCst),
            "unified gate must block the backend on reject"
        );
    }

    #[tokio::test]
    async fn composite_permission_rule_denies_even_l0_read() {
        // A deny specifier rule short-circuits before the level→policy decision,
        // so it blocks even an otherwise-L0 Read (the secrets denylist).
        let mcp = McpExecutor::new(
            "frontend_dev",
            FakeBackend::ok("secret"),
            AllowAll,
            RecordingSink::default(),
        );
        let mem = MemoryStore::new(std::env::temp_dir().join(format!(
            "aidock-comp-mem-rule-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::SeqCst)
        )));
        let mut ex = CompositeExecutor::new(
            mcp,
            Vec::new(),
            Scratchpad::default(),
            temp_scratchpad_path(),
            mem,
            BashTool::new(
                std::env::temp_dir(),
                false,
                crate::agent::shell::BashRegistry::new(),
            ),
            FileTools::new(std::env::temp_dir()),
            SecurityLevel::Standard,
            std::env::temp_dir(),
            vec![PermissionRule::deny("Read", "*/.ssh/*")],
            LspPool::new(std::env::temp_dir()),
        );
        let res = ex
            .execute("Read", r#"{"file_path":"/home/u/.ssh/id_rsa"}"#)
            .await;
        assert!(!res.success);
        assert!(res.raw.starts_with("DENIED:"), "{}", res.raw);
        assert!(res.raw.contains("deny rule"));
    }

    #[tokio::test]
    async fn composite_routes_recall_detail_to_memory() {
        let mut ex = composite();
        // Seed a detail in this executor's memory, then recall it by id.
        ex.memory.write_detail("topic-001", 0, "用 JWT", "full detail body").unwrap();
        let res = ex.execute("recall_detail", r#"{"id":"topic-001#1"}"#).await;
        assert!(res.success);
        assert!(res.raw.contains("full detail body"));
    }
}
