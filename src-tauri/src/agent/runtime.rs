//! Per-Agent execution loop.
//!
//! One tokio task per Agent. Each task owns:
//!   - the Agent's `RoleConfig`
//!   - an inbox receiver fed by the dispatcher
//!   - a submit sender shared with all other Agents + the UI
//!   - a local view of message history (what's flowed through this Agent's
//!     inbox so far)
//!   - this Agent's state-machine `AgentState`
//!
//! The runtime never touches another Agent's state. Cross-Agent coordination
//! is purely message-passing through the dispatcher.
//!
//! V0.1 response policy (kept deliberately simple — Sprint 2.6+ relaxes it):
//!
//!   | Incoming                                | This Agent acts? |
//!   |-----------------------------------------|------------------|
//!   | ASK_AGENT to me                         | Yes, must answer |
//!   | ANSWER replying to my pending ASK       | Yes (continues)  |
//!   | UserInput / BROADCAST, this Agent is PM | Yes              |
//!   | Anything else                           | No (FYI only)    |
//!
//! State transitions are driven by what the Agent EMITS, not by what comes
//! in:
//!   - WORK_START      → WORKING
//!   - DONE            → IDLE
//!   - ASK_AGENT       → WAITING_ANSWER
//!   - matching ANSWER incoming → drops WAITING_ANSWER before deciding

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use std::time::Duration;

use tauri::Emitter;
use tokio::sync::oneshot;

use crate::agent::approval::{
    classify, ApprovalRegistry, ApprovalRequest, Decision, LookupResult, PendingApprovals,
    APPROVAL_REQUEST_EVENT, APPROVAL_TIMEOUT_SECS,
};
use crate::agent::context::build_level_0_context;
use crate::agent::dispatcher::DispatcherHandle;
use crate::agent::fs_tools::FileTools;
use crate::agent::lsp_pool::LspPool;
use crate::agent::mcp::{truncate, McpClient, TOOL_CALL_EVENT, MCP_PREVIEW_MAX};
use crate::agent::mcp_log::McpLogHandle;
use crate::agent::message::{AgentMessage, AgentMessageKind, TopicId};
use crate::agent::role::RoleConfig;
use crate::agent::scratchpad::Scratchpad;
use crate::agent::state::AgentState;
use crate::agent::tools::advertised_tools;
use crate::llm::bailian::BailianProvider;
use crate::llm::ChatMessage;

use std::collections::VecDeque;

use crate::agent::llm_source::LlmSource;
use crate::agent::mcp::ToolCallEvent;
use crate::agent::mcp_executor::{
    ApprovalOutcome, Approver, CallSink, CompositeExecutor, McpClientBackend, McpExecutor,
};
use crate::agent::memory::{assemble_bundle, MemoryStore, TopicTracker};
use crate::agent::shell::{BashRegistry, BashTool};
use crate::agent::react::{AlwaysContinue, NullSink, Outbound, PlanLoop, TurnOutcome, TurnState};
use crate::agent::turn_bridge::{ask_to_kind, outbound_to_kind};

// (Sprint-4 auxiliary-loop helpers removed in the #2b cutover: the ReAct
// turn host no longer hand-rolls a per-tool loop or a terminal-kind check —
// the PlanLoop owns turn termination and the CompositeExecutor owns tool
// routing.)

// 工具广告现在由统一注册表驱动（CLAUDE.md ④「工具调用系统」）：
// `tools::advertised_tools(&role.tools, mcp)` 按角色目录逐名过滤注册表 + 过渡 fs__
// 授权。旧的 `build_all_tools` + `mcp_access` 过滤已退役。结构性门控（三层防御
// layer 2）= 不在目录里就 advertise 不出去。

/// Everything the runtime needs at spawn time. Holding this as a struct
/// keeps the spawn_agent signature stable as Sprint 3+ adds more recovered
/// state fields.
pub struct AgentBoot {
    pub role: RoleConfig,
    pub inbox_rx: mpsc::Receiver<AgentMessage>,
    pub dispatcher: DispatcherHandle,
    pub api_key: String,
    /// Messages this agent already saw in prior sessions, in arrival order.
    /// Empty for a fresh session.
    pub initial_history: Vec<AgentMessage>,
    /// State machine snapshot. Typically derived from `initial_history`
    /// via `AgentState::derive_from_history` by the session orchestrator.
    pub initial_state: AgentState,
    /// Loaded scratchpad — `Default` for fresh sessions.
    pub initial_scratchpad: Scratchpad,
    /// Where to persist scratchpad updates. The runtime writes after every
    /// successful `update_scratchpad` so crashes never lose more than the
    /// in-flight LLM call.
    pub scratchpad_path: PathBuf,
    /// ④.b memory root for this agent (`{session}/agents/{id}/memory`). The
    /// turn host persists the plan here as a topic after each turn.
    pub memory_root: PathBuf,
    /// The session workspace dir — native file tools confine writes/deletes to it.
    pub workspace_dir: PathBuf,
    /// Session-scoped LSP server pool (shared Arc across all agents). Cheap clone.
    pub lsp_pool: LspPool,
    /// Shared MCP client handle (Sprint 4). `None` when the filesystem
    /// MCP server failed to start — agents run chat-only in that case.
    pub mcp: Option<McpClient>,
    /// Tauri handle for emitting MCP tool-call events to the chat panel
    /// (Sprint 4.5). `None` in headless dev harnesses; the runtime
    /// silently skips emission when absent.
    pub app_handle: Option<tauri::AppHandle>,
    /// Sprint 4.6: per-session approval state for destructive MCP calls.
    /// Cheap to clone (internal Arc).
    pub approval: ApprovalRegistry,
    /// Sprint 4.6: map of in-flight approval requests, keyed by request id.
    /// The runtime registers a oneshot sender here before emitting the
    /// approval-request event; the `respond_to_approval` Tauri command
    /// fulfils the channel.
    pub pending_approvals: PendingApprovals,
    /// Sprint 4 polish: cheap-clone handle into the mpsc writer task that
    /// appends each MCP call to `mcp_calls.jsonl`. `None` for headless
    /// harnesses that don't need a persistence path.
    pub mcp_log: Option<McpLogHandle>,
}

/// Spawn one Agent task. The returned `JoinHandle` lets the session
/// orchestrator await graceful shutdown when the inbox closes.
pub fn spawn_agent(boot: AgentBoot) -> JoinHandle<()> {
    tokio::spawn(run_agent(boot))
}

/// The concrete ④.a loop the live runtime drives. Spelled out once so the
/// suspended-turn stash can name its type.
type LiveLoop = PlanLoop<
    LlmSource<BailianProvider>,
    CompositeExecutor<McpClientBackend, RealApprover, RealCallSink>,
    AlwaysContinue,
    NullSink,
>;

/// A turn parked mid-flight waiting for an answer (CLAUDE.md ④.a 硬约束 + #2b).
/// Holds the *live* loop — its `LlmSource` conversation, executor, and the
/// scratchpad moved inside — so resume continues exactly where it left off,
/// not a fresh think. Kept in memory for V0.1.
/// > TODO: persist across app restart (落 ⑥) — see CLAUDE.md ③ 接线.
struct SuspendedTurn {
    plan_loop: LiveLoop,
    state: TurnState,
    /// The ASK message id we're parked on; an incoming ANSWER matches it.
    ask_id: String,
    /// Topic to stamp on messages when the turn resumes.
    topic: TopicId,
    /// If this turn was triggered by an ASK_AGENT directed at us, the id of
    /// that ask — so on turn-end we can auto-ANSWER it if the agent forgot to
    /// (otherwise the asker hangs in WAITING_ANSWER forever). Carried across
    /// suspend/resume since the original ask spans the whole turn.
    answering: Option<String>,
}

/// Real approval seam: delegates to [`gate_mcp_call`]. Headless (no UI) proceeds
/// without prompting, mirroring the pre-cutover behavior.
struct RealApprover {
    registry: ApprovalRegistry,
    pending: PendingApprovals,
    app_handle: Option<tauri::AppHandle>,
}

impl Approver for RealApprover {
    async fn decide(&self, agent: &str, tool: &str, args: &str) -> ApprovalOutcome {
        if self.app_handle.is_none() {
            return ApprovalOutcome::Proceed;
        }
        gate_mcp_call(
            agent,
            tool,
            args,
            &self.registry,
            &self.pending,
            self.app_handle.as_ref(),
        )
        .await
    }
}

/// Real fan-out seam: the full-fidelity MCP call event → UI (Tauri) + ⑥
/// (`mcp_calls.jsonl`). The ⑥/① pipe of the two-pipe rule.
struct RealCallSink {
    app_handle: Option<tauri::AppHandle>,
    mcp_log: Option<McpLogHandle>,
}

impl CallSink for RealCallSink {
    async fn record(&self, event: ToolCallEvent) {
        if let Some(handle) = &self.app_handle {
            if let Err(e) = handle.emit(TOOL_CALL_EVENT, &event) {
                tracing::warn!(
                    target: "aidock::agent",
                    error = %e,
                    "failed to emit MCP call event"
                );
            }
        }
        if let Some(log) = &self.mcp_log {
            log.record(event).await;
        }
    }
}

async fn run_agent(boot: AgentBoot) {
    let AgentBoot {
        role,
        mut inbox_rx,
        dispatcher,
        api_key,
        initial_history,
        initial_state,
        initial_scratchpad,
        scratchpad_path,
        memory_root,
        workspace_dir,
        lsp_pool,
        mcp,
        app_handle,
        approval,
        pending_approvals,
        mcp_log,
    } = boot;

    // Tools: 统一注册表按角色目录过滤广告（set_plan / recall_detail / shell / fs__
    // 全在注册表里，由 `role.tools` 决定可见——CLAUDE.md ④「工具调用系统」）。
    let tools = advertised_tools(&role.tools, mcp.as_ref());
    // Bash 是否启用 = 目录里有没有它（PM 无）。step 4 起底线改走安全检查。
    let bash_enabled = role.tools.iter().any(|t| t == crate::agent::shell::TOOL_BASH);
    // 后台进程登记表：per-agent、跨回合存活（cloned 进每回合的 BashTool）。
    let bash_registry = BashRegistry::new();

    tracing::info!(
        target: "aidock::agent",
        role = %role.id,
        recovered_msgs = initial_history.len(),
        state = initial_state.tag(),
        security_level = ?role.security_level,
        advertised_tools = tools.len(),
        "agent task started (ReAct turn host)"
    );

    // One provider per agent; cloned (cheap — shared pool) into each turn.
    let provider = BailianProvider::new(api_key);
    let mut state = initial_state;
    let mut history: Vec<AgentMessage> = initial_history;
    // `Some` while idle/between turns; `None` while a turn owns it (active or
    // parked). One-turn-at-a-time guarantees no concurrent access.
    let mut scratchpad: Option<Scratchpad> = Some(initial_scratchpad);
    let mut suspended: Option<SuspendedTurn> = None;
    let mut deferred: VecDeque<AgentMessage> = VecDeque::new();

    // ④.b memory: persist the plan as a topic after each turn. Read-side
    // (feeding the bundle back into context) is a later slice; for now this is
    // write-only — the plan survives across turns/restarts on disk.
    let mem_store = MemoryStore::new(memory_root);
    let mut topics = TopicTracker::default();
    topics.recover(&mem_store);

    loop {
        // Drain a deferred trigger when idle & nothing parked; else block on
        // the inbox. Deferred messages are already in `history`.
        let (msg, from_inbox) =
            if suspended.is_none() && state.is_idle() && !deferred.is_empty() {
                (deferred.pop_front().unwrap(), false)
            } else {
                match inbox_rx.recv().await {
                    Some(m) => (m, true),
                    None => break,
                }
            };
        if from_inbox {
            history.push(msg.clone());
        }

        tracing::debug!(
            target: "aidock::agent",
            role = %role.id,
            in_kind = msg.kind.tag(),
            from = %msg.sender,
            state = state.tag(),
            parked = suspended.is_some(),
            "inbox received"
        );

        let suspended_ask = suspended.as_ref().map(|s| s.ask_id.as_str());
        match classify_incoming(&role, &state, &msg, suspended_ask) {
            Disposition::Ignore => continue,
            Disposition::Defer => {
                // One-turn-at-a-time: hold the trigger until the parked turn ends.
                deferred.push_back(msg);
                continue;
            }
            Disposition::Resume => {
                let parked = suspended.take().expect("Resume implies a parked turn");
                let answer = match &msg.kind {
                    AgentMessageKind::Answer { content, .. } => content.clone(),
                    _ => String::new(),
                };
                state = AgentState::Idle; // drop WAITING_ANSWER before resuming
                let mut plan_loop = parked.plan_loop;
                let (outcome, turnstate) = plan_loop.resume(parked.state, answer).await;
                handle_outcome(
                    &role, &dispatcher, &mut state, &mut history, &mut scratchpad,
                    &mut suspended, &mem_store, &mut topics, plan_loop, outcome, turnstate,
                    parked.topic, parked.answering,
                )
                .await;
            }
            Disposition::StartTurn => {
                let topic = msg.topic_id.clone();
                // If a teammate's ASK_AGENT triggered this turn, remember its id
                // so we auto-ANSWER on turn-end (lest the asker hang forever).
                let answering = match &msg.kind {
                    AgentMessageKind::AskAgent { to, .. } if to == &role.id => Some(msg.id.clone()),
                    _ => None,
                };
                let pad = scratchpad.take().expect("scratchpad present when idle");
                // Build context BEFORE moving the scratchpad into the executor.
                let mut context = build_level_0_context(&role, &history, &msg, &pad);
                // ④.b read-side: feed the open topic's plan back as memory so
                // the agent continues its plan instead of re-deriving it. (No
                // step details yet — guardian/detail-capture is a later slice.)
                if let Some(tid) = topics.current() {
                    if let Some(plan) = mem_store.read_topic_plan(tid) {
                        let bundle = assemble_bundle(&mem_store, tid, &plan);
                        if !bundle.is_empty() {
                            context.push(ChatMessage::System { content: bundle });
                        }
                    }
                }
                let source = LlmSource::new(
                    provider.clone(),
                    role.model.primary.clone(),
                    role.model.temperature,
                    role.model.max_tokens,
                    tools.clone(),
                    context,
                );
                let mcp_exec = McpExecutor::new(
                    role.id.clone(),
                    McpClientBackend { client: mcp.clone() },
                    RealApprover {
                        registry: approval.clone(),
                        pending: pending_approvals.clone(),
                        app_handle: app_handle.clone(),
                    },
                    RealCallSink {
                        app_handle: app_handle.clone(),
                        mcp_log: mcp_log.clone(),
                    },
                );
                let composite = CompositeExecutor::new(
                    mcp_exec,
                    history.clone(),
                    pad,
                    scratchpad_path.clone(),
                    mem_store.clone(),
                    BashTool::new(workspace_dir.clone(), bash_enabled, bash_registry.clone()),
                    FileTools::new(workspace_dir.clone()),
                    role.security_level,
                    workspace_dir.clone(),
                    role.permission_rules.clone(),
                    lsp_pool.clone(),
                );
                let mut plan_loop = PlanLoop::new(source, composite, AlwaysContinue, NullSink);
                let (outcome, turnstate) = plan_loop.run_turn().await;
                handle_outcome(
                    &role, &dispatcher, &mut state, &mut history, &mut scratchpad,
                    &mut suspended, &mem_store, &mut topics, plan_loop, outcome, turnstate,
                    topic, answering,
                )
                .await;
            }
        }
    }

    tracing::info!(target: "aidock::agent", role = %role.id, "agent task ended");
}

/// Apply a finished/suspended turn's effects to the dispatcher + state machine.
/// This is the ③ side: it drains the turn's outward messages, emits the
/// terminal/ASK message, transitions `AgentState`, and either reclaims the
/// scratchpad (terminal) or parks the live loop (suspended).
#[allow(clippy::too_many_arguments)]
async fn handle_outcome(
    role: &RoleConfig,
    dispatcher: &DispatcherHandle,
    state: &mut AgentState,
    history: &mut Vec<AgentMessage>,
    scratchpad: &mut Option<Scratchpad>,
    suspended: &mut Option<SuspendedTurn>,
    mem_store: &MemoryStore,
    topics: &mut TopicTracker,
    plan_loop: LiveLoop,
    outcome: TurnOutcome,
    mut turnstate: TurnState,
    topic: TopicId,
    // If this turn was triggered by an ASK directed at us, that ask's id —
    // auto-ANSWERed on turn-end if the agent didn't answer it itself.
    answering: Option<String>,
) {
    // ④.b: persist the turn's plan into its 大主题 (mint/update, close if all
    // steps done). A turn with no plan (agent didn't call set_plan — e.g. a
    // trivial one-line answer) has nothing to persist.
    match turnstate.plan.as_ref() {
        Some(plan) => match topics.record(mem_store, plan, now_ms()) {
            Ok(tid) => tracing::info!(
                target: "aidock::agent", role = %role.id, topic = %tid,
                steps = plan.steps.len(), "memory: persisted plan to topic"
            ),
            Err(e) => tracing::warn!(
                target: "aidock::agent", role = %role.id, error = %e, "memory: persist plan FAILED"
            ),
        },
        None => tracing::info!(
            target: "aidock::agent", role = %role.id,
            "memory: turn had no plan (agent didn't call set_plan) — nothing to persist"
        ),
    }

    // Drain outward messages (broadcasts / answers) the turn accumulated.
    // Track whether the agent itself answered the ASK that triggered this turn.
    let mut answered_ask = false;
    for out in turnstate.outgoing.drain(..) {
        if let Outbound::Answer { reply_to, .. } = &out {
            if answering.as_deref() == Some(reply_to.as_str()) {
                answered_ask = true;
            }
        }
        dispatch_kind(role, outbound_to_kind(out), &topic, dispatcher, state, history).await;
    }

    // If a teammate's ASK triggered this turn and the agent ended it WITHOUT
    // answering, auto-ANSWER so the asker unblocks. The bug this fixes: an
    // agent did the work then emitted DONE instead of ANSWER → the asker hung
    // in WAITING_ANSWER forever, and the user's next message got deferred
    // behind it. `should_auto_answer` is true only when we have an unanswered
    // triggering ask; a *suspended* turn carries `answering` forward instead.
    let pending_answer_id = if answered_ask { None } else { answering.clone() };

    match outcome {
        TurnOutcome::Done { summary } => {
            if let Some(ask_id) = &pending_answer_id {
                dispatch_kind(
                    role,
                    AgentMessageKind::Answer { reply_to: ask_id.clone(), content: summary.clone() },
                    &topic, dispatcher, state, history,
                )
                .await;
            }
            dispatch_kind(
                role,
                AgentMessageKind::Done { summary, artifact_id: None },
                &topic,
                dispatcher,
                state,
                history,
            )
            .await; // transitions state → Idle
            *scratchpad = Some(plan_loop.executor.into_scratchpad());
        }
        TurnOutcome::Escalated { reason } => {
            if let Some(ask_id) = &pending_answer_id {
                dispatch_kind(
                    role,
                    AgentMessageKind::Answer { reply_to: ask_id.clone(), content: format!("（未能完成）{reason}") },
                    &topic, dispatcher, state, history,
                )
                .await;
            }
            // No human Esc in multi-agent; tell the team we're stuck.
            dispatch_kind(
                role,
                AgentMessageKind::Broadcast {
                    content: format!("⚠️ 我卡住了，需要帮助：{reason}"),
                },
                &topic,
                dispatcher,
                state,
                history,
            )
            .await;
            *state = AgentState::Idle;
            *scratchpad = Some(plan_loop.executor.into_scratchpad());
        }
        TurnOutcome::Suspended { to, content, expected_format } => {
            let ask_id = dispatch_kind(
                role,
                ask_to_kind(to, content, expected_format),
                &topic,
                dispatcher,
                state,
                history,
            )
            .await; // transitions state → WaitingAnswer{for_message: ask_id}
            // Park the live loop (scratchpad rides inside it) until the answer.
            // Carry `answering` so the original ASK still gets answered when
            // this turn eventually ends.
            *suspended = Some(SuspendedTurn {
                plan_loop,
                state: turnstate,
                ask_id,
                topic,
                answering,
            });
        }
    }
}

/// Build, state-transition, record, and dispatch one outgoing message.
/// Returns the assigned message id (used to key WAITING_ANSWER on an ASK).
async fn dispatch_kind(
    role: &RoleConfig,
    kind: AgentMessageKind,
    topic: &TopicId,
    dispatcher: &DispatcherHandle,
    state: &mut AgentState,
    history: &mut Vec<AgentMessage>,
) -> String {
    let outgoing = AgentMessage {
        id: format!("m-{}", Uuid::new_v4()),
        sender: role.id.clone(),
        topic_id: topic.clone(),
        timestamp: now_ms(),
        kind,
        opens_topic_title: None,
    };
    apply_outgoing_state_transition(state, &outgoing);
    let id = outgoing.id.clone();
    // Own emits go into local history so the next turn's context sees them.
    history.push(outgoing.clone());
    if let Err(e) = dispatcher.submit(outgoing).await {
        tracing::error!(
            target: "aidock::agent",
            role = %role.id,
            error = %e,
            "dispatcher submit failed"
        );
    }
    id
}

// ---------- response decision ----------

fn should_respond(role: &RoleConfig, state: &AgentState, msg: &AgentMessage) -> bool {
    match &msg.kind {
        // Directed ask — always answer if I'm the target. State doesn't gate
        // this; even WORKING is preempted by a direct question. The model
        // implements "I'm busy" by replying short, not by ignoring.
        AgentMessageKind::AskAgent { to, .. } => to == &role.id,

        // ANSWER is handled separately via `resolved_my_pending` in the loop —
        // if it matched the agent's pending ASK we already trigger a think.
        // Other ANSWERs (overheard between teammates) are FYI only.
        AgentMessageKind::Answer { .. } => false,

        // User input — only PM picks up. V0.1 keeps the human-facing surface
        // single-threaded through PM to avoid race conditions on the customer.
        AgentMessageKind::UserInput { .. } => is_coordinator(role),

        // BROADCAST is pickup-optional per design §4.1. V0.1 makeshift
        // relevance filter (Sprint 2.6 replaces this):
        //   - PM picks up everything (coordinator)
        //   - Other agents pick up if the broadcast names their role id
        //   - WORKING agents stay focused — don't get distracted by broadcasts
        AgentMessageKind::Broadcast { content } => {
            if is_coordinator(role) {
                return true;
            }
            if !state.is_idle() {
                return false;
            }
            broadcast_mentions_me(&role.id, content)
        }

        // FYI kinds — never trigger a think.
        AgentMessageKind::WorkStart { .. }
        | AgentMessageKind::Progress { .. }
        | AgentMessageKind::Done { .. }
        | AgentMessageKind::Summary { .. } => false,
    }
}

/// Cheap substring check — fine for V0.1 role ids that are reasonably
/// distinctive ("frontend_dev", "backend_dev"). Sprint 2.6's relevance
/// filter does this more carefully, but this gets the demo unblocked.
fn broadcast_mentions_me(role_id: &str, content: &str) -> bool {
    content.contains(role_id)
}

/// What to do with an incoming message given whether a turn is currently
/// parked. The new ④.a turn host (CLAUDE.md #2b) layers two rules on top of
/// `should_respond`:
///   - **resume**: an ANSWER matching the parked ASK reopens that turn
///   - **one-turn-at-a-time**: while a turn is parked, any *other* message
///     that would normally trigger a think is deferred, not run concurrently
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// The ANSWER we were waiting on — resume the suspended turn.
    Resume,
    /// No turn in flight and policy says respond — start a fresh turn.
    StartTurn,
    /// A turn is parked; this would-trigger message waits its turn.
    Defer,
    /// Not for us right now.
    Ignore,
}

/// Decide an incoming message's disposition. `suspended_ask_id` is `Some` iff a
/// turn is currently parked waiting for the answer to that ASK message id.
fn classify_incoming(
    role: &RoleConfig,
    state: &AgentState,
    msg: &AgentMessage,
    suspended_ask_id: Option<&str>,
) -> Disposition {
    if let Some(ask_id) = suspended_ask_id {
        if let AgentMessageKind::Answer { reply_to, .. } = &msg.kind {
            if reply_to == ask_id {
                return Disposition::Resume;
            }
        }
        // One-turn-at-a-time: a would-trigger message waits; the rest are FYI.
        if should_respond(role, state, msg) {
            Disposition::Defer
        } else {
            Disposition::Ignore
        }
    } else if should_respond(role, state, msg) {
        Disposition::StartTurn
    } else {
        Disposition::Ignore
    }
}

/// The coordinator = the activated front-desk role (V0.1 = PM). Config-driven
/// now (`RoleConfig.is_coordinator`) instead of a hardcoded id, so a future
/// workshop can pick a different coordinator without touching routing.
fn is_coordinator(role: &RoleConfig) -> bool {
    role.is_coordinator
}

// ---------- assembling and tracking outgoing messages ----------
//
// (`build_outgoing` removed in the #2b cutover — `dispatch_kind` builds the
// AgentMessage directly from a kind + the turn's topic. Topic-fallback /
// new_topic_title logic returns when topic management is revisited.)

fn apply_outgoing_state_transition(state: &mut AgentState, msg: &AgentMessage) {
    match &msg.kind {
        AgentMessageKind::WorkStart { task } => {
            *state = AgentState::Working {
                task: task.clone(),
                since: msg.timestamp,
            };
        }
        AgentMessageKind::Done { .. } => {
            *state = AgentState::Idle;
        }
        AgentMessageKind::AskAgent { .. } => {
            *state = AgentState::WaitingAnswer {
                for_message: msg.id.clone(),
                since: msg.timestamp,
            };
        }
        AgentMessageKind::Summary { .. } => {
            // Closing a topic doesn't change my own work state.
        }
        // Broadcast / Answer / Progress / UserInput don't move our state.
        _ => {}
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Run one tool through the approval gate. Returns immediately for
/// read-only / pre-approved tools; awaits a user response (via oneshot
/// channel) for unknowns, with a hard timeout.
async fn gate_mcp_call(
    agent: &str,
    tool: &str,
    raw_args: &str,
    registry: &ApprovalRegistry,
    pending: &PendingApprovals,
    app_handle: Option<&tauri::AppHandle>,
) -> ApprovalOutcome {
    match registry.lookup(agent, tool).await {
        LookupResult::NoGate | LookupResult::Approved => return ApprovalOutcome::Proceed,
        LookupResult::Rejected => {
            return ApprovalOutcome::Rejected(format!(
                "user previously rejected '{tool}' in this session"
            ));
        }
        LookupResult::AskUser => {}
    }

    let id = format!("appr-{}", Uuid::new_v4());
    let (tx, rx) = oneshot::channel::<Decision>();
    pending.lock().await.insert(id.clone(), tx);

    let req = ApprovalRequest {
        id: id.clone(),
        agent: agent.to_string(),
        tool: tool.to_string(),
        args_preview: truncate(raw_args, MCP_PREVIEW_MAX),
        timestamp: now_ms(),
        danger: classify(tool),
    };

    if let Some(handle) = app_handle {
        if let Err(e) = handle.emit(APPROVAL_REQUEST_EVENT, &req) {
            // Couldn't reach the UI — drop the pending entry and reject.
            pending.lock().await.remove(&id);
            tracing::error!(
                target: "aidock::agent",
                error = %e,
                agent,
                tool,
                "approval request emit failed; auto-rejecting"
            );
            return ApprovalOutcome::Rejected(format!(
                "couldn't reach UI to ask permission: {e}"
            ));
        }
    } else {
        // Defensive — gate_mcp_call shouldn't be called without a
        // handle in headless mode, but if it is, auto-reject so we
        // never block forever.
        pending.lock().await.remove(&id);
        return ApprovalOutcome::Rejected(
            "no UI available to request approval".to_string(),
        );
    }

    tracing::info!(
        target: "aidock::agent",
        agent,
        tool,
        request_id = %id,
        "awaiting user approval"
    );

    match tokio::time::timeout(Duration::from_secs(APPROVAL_TIMEOUT_SECS), rx).await {
        Ok(Ok(decision)) => {
            // A REAL user decision — remember it so AllowSession actually
            // sticks (subsequent calls of this tool then auto-proceed).
            registry.remember(agent, tool, decision).await;
            if decision.allows_execution() {
                ApprovalOutcome::Proceed
            } else {
                ApprovalOutcome::Rejected("user rejected this tool call".to_string())
            }
        }
        Ok(Err(_)) => {
            // Channel dropped — deny THIS call only; do NOT remember (a
            // dropped channel isn't the user saying "no to everything").
            tracing::warn!(
                target: "aidock::agent", agent, tool, request_id = %id,
                "approval channel dropped; denying this call (not remembered)"
            );
            ApprovalOutcome::Rejected("approval channel closed".to_string())
        }
        Err(_) => {
            // Timed out — deny THIS call only; do NOT remember. A missed
            // prompt must not poison the whole session (the old bug: timeout
            // remembered Reject → every later write silently denied).
            pending.lock().await.remove(&id);
            tracing::warn!(
                target: "aidock::agent", agent, tool, request_id = %id,
                timeout_s = APPROVAL_TIMEOUT_SECS,
                "approval timed out; denying this call (not remembered)"
            );
            ApprovalOutcome::Rejected("approval timed out — no response in time".to_string())
        }
    }
}

/// Build an inbound message representing a user line. Exposed so the Tauri
/// command can craft one without duplicating the wire details.
pub fn user_input_message(content: String, topic_id: TopicId) -> AgentMessage {
    AgentMessage::new(
        format!("m-{}", Uuid::new_v4()),
        "user",
        topic_id,
        now_ms(),
        AgentMessageKind::UserInput { content },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::roles::{frontend_role, pm_role};

    fn ask(to: &str) -> AgentMessage {
        AgentMessage::new(
            "m-1",
            "PM",
            "t-1",
            0,
            AgentMessageKind::AskAgent {
                to: to.into(),
                content: "?".into(),
                expected_format: None,
            },
        )
    }

    fn user(content: &str) -> AgentMessage {
        AgentMessage::new(
            "m-u",
            "user",
            "t-1",
            0,
            AgentMessageKind::UserInput {
                content: content.into(),
            },
        )
    }

    #[allow(dead_code)]
    fn broadcast(from: &str) -> AgentMessage {
        AgentMessage::new(
            "m-b",
            from,
            "t-1",
            0,
            AgentMessageKind::Broadcast {
                content: "hi".into(),
            },
        )
    }

    #[test]
    fn pm_responds_to_user_input() {
        assert!(should_respond(&pm_role(), &AgentState::Idle, &user("hi")));
    }

    #[test]
    fn frontend_ignores_user_input() {
        // Without ASK_AGENT direct, non-PM Agents stay quiet on UserInput.
        assert!(!should_respond(&frontend_role(), &AgentState::Idle, &user("hi")));
    }

    #[test]
    fn frontend_responds_to_direct_ask() {
        assert!(should_respond(
            &frontend_role(),
            &AgentState::Idle,
            &ask("frontend_dev")
        ));
    }

    #[test]
    fn frontend_ignores_ask_to_other() {
        assert!(!should_respond(
            &frontend_role(),
            &AgentState::Idle,
            &ask("backend_dev")
        ));
    }

    fn broadcast_with_content(from: &str, content: &str) -> AgentMessage {
        AgentMessage::new(
            "m-b",
            from,
            "t-1",
            0,
            AgentMessageKind::Broadcast {
                content: content.into(),
            },
        )
    }

    #[test]
    fn pm_picks_up_every_broadcast() {
        // PM hears everything, regardless of content.
        assert!(should_respond(
            &pm_role(),
            &AgentState::Idle,
            &broadcast_with_content("frontend_dev", "PM, btw I'm done")
        ));
        assert!(should_respond(
            &pm_role(),
            &AgentState::Idle,
            &broadcast_with_content("backend_dev", "anyone awake?")
        ));
    }

    #[test]
    fn frontend_picks_up_broadcast_mentioning_them() {
        // PM-style delegating BROADCAST that names the developer.
        let msg = broadcast_with_content(
            "PM",
            "分工：frontend_dev 做登录页；backend_dev 做认证 API。",
        );
        assert!(should_respond(&frontend_role(), &AgentState::Idle, &msg));
    }

    #[test]
    fn frontend_ignores_broadcast_not_mentioning_them() {
        let msg = broadcast_with_content("PM", "Heads up team, customer is waiting");
        assert!(!should_respond(&frontend_role(), &AgentState::Idle, &msg));
    }

    #[test]
    fn working_agent_ignores_broadcasts() {
        // Even if the broadcast names me, when I'm WORKING I stay focused.
        let working = AgentState::Working {
            task: "build login".into(),
            since: 0,
        };
        let msg = broadcast_with_content("PM", "frontend_dev — quick reminder, brand colors");
        assert!(!should_respond(&frontend_role(), &working, &msg));
    }

    // ---------- classify_incoming (turn-host disposition, #2b) ----------

    fn answer(reply_to: &str) -> AgentMessage {
        AgentMessage::new(
            "m-ans",
            "backend_dev",
            "t-1",
            0,
            AgentMessageKind::Answer {
                reply_to: reply_to.into(),
                content: "use JWT".into(),
            },
        )
    }

    fn waiting_on(ask_id: &str) -> AgentState {
        AgentState::WaitingAnswer {
            for_message: ask_id.into(),
            since: 0,
        }
    }

    #[test]
    fn suspended_matching_answer_resumes() {
        assert_eq!(
            classify_incoming(&frontend_role(), &waiting_on("ask-99"), &answer("ask-99"), Some("ask-99")),
            Disposition::Resume
        );
    }

    #[test]
    fn suspended_non_matching_answer_ignored() {
        assert_eq!(
            classify_incoming(&frontend_role(), &waiting_on("ask-99"), &answer("ask-other"), Some("ask-99")),
            Disposition::Ignore
        );
    }

    #[test]
    fn suspended_directed_ask_is_deferred_not_run() {
        // One-turn-at-a-time: a fresh directed ASK can't preempt a parked turn.
        assert_eq!(
            classify_incoming(&frontend_role(), &waiting_on("ask-99"), &ask("frontend_dev"), Some("ask-99")),
            Disposition::Defer
        );
    }

    #[test]
    fn idle_directed_ask_starts_a_turn() {
        assert_eq!(
            classify_incoming(&frontend_role(), &AgentState::Idle, &ask("frontend_dev"), None),
            Disposition::StartTurn
        );
    }

    #[test]
    fn idle_irrelevant_ask_ignored() {
        assert_eq!(
            classify_incoming(&frontend_role(), &AgentState::Idle, &ask("backend_dev"), None),
            Disposition::Ignore
        );
    }

    #[test]
    fn outgoing_ask_transitions_to_waiting() {
        let mut state = AgentState::Idle;
        let m = AgentMessage::new(
            "m-out",
            "PM",
            "t-1",
            100,
            AgentMessageKind::AskAgent {
                to: "frontend_dev".into(),
                content: "?".into(),
                expected_format: None,
            },
        );
        apply_outgoing_state_transition(&mut state, &m);
        match state {
            AgentState::WaitingAnswer { for_message, since } => {
                assert_eq!(for_message, "m-out");
                assert_eq!(since, 100);
            }
            other => panic!("expected WaitingAnswer, got {other:?}"),
        }
    }

    #[test]
    fn outgoing_work_start_then_done() {
        let mut state = AgentState::Idle;
        let m1 = AgentMessage::new(
            "m1",
            "frontend_dev",
            "t-1",
            1,
            AgentMessageKind::WorkStart {
                task: "build".into(),
            },
        );
        apply_outgoing_state_transition(&mut state, &m1);
        assert!(state.is_working());

        let m2 = AgentMessage::new(
            "m2",
            "frontend_dev",
            "t-1",
            2,
            AgentMessageKind::Done {
                summary: "done".into(),
                artifact_id: None,
            },
        );
        apply_outgoing_state_transition(&mut state, &m2);
        assert!(state.is_idle());
    }
}
