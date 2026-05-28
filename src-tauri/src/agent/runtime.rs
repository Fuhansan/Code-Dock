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

use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use std::time::Duration;

use tauri::Emitter;
use tokio::sync::oneshot;

use crate::agent::approval::{
    classify, ApprovalRegistry, ApprovalRequest, Danger, Decision, LookupResult, PendingApprovals,
    APPROVAL_REQUEST_EVENT, APPROVAL_TIMEOUT_SECS,
};
use crate::agent::role::McpAccess;
use crate::agent::context::build_level_0_context;
use crate::agent::dispatcher::DispatcherHandle;
use crate::agent::mcp::{
    make_call_event, truncate, McpClient, MCP_CALL_EVENT, MCP_PREVIEW_MAX, MCP_TOOL_PREFIX,
};
use crate::agent::message::{AgentMessage, AgentMessageKind, TopicId};
use crate::agent::protocol::{message_tools, parse_tool_call, ParsedToolCall};
use crate::agent::recall::{execute_query_tool, is_query_tool, query_tools};
use crate::agent::role::{McpAccess as RoleMcpAccess, RoleConfig};
use crate::agent::roles::PM_ID;
use crate::agent::scratchpad::{
    apply_update as apply_scratchpad_update, is_scratchpad_tool, scratchpad_tools, Scratchpad,
};
use crate::agent::state::AgentState;
use crate::llm::bailian::BailianProvider;
use crate::llm::{ChatMessage, ChatRequest, LLMProvider, Tool};

/// Cap on how many auxiliary→action loops one agent turn may run. Above
/// this we emit a warning and stop — protects against a model that keeps
/// recalling / scratchpad-updating / MCP-tool-calling without ever
/// committing to an action.
///
/// Bumped from 3 in Sprint 4 after the e2e demo showed a developer
/// realistically uses 4+ MCP calls per turn (list_directory → write_file
/// → list_directory → write_file → DONE). Three was too tight for any
/// non-trivial coding flow.
const MAX_QUERY_LOOP_ITERATIONS: usize = 6;

/// True iff `name` is a Sprint 4 MCP-routed tool (prefix-tagged at
/// projection time in `agent::mcp`).
fn is_mcp_tool(name: &str) -> bool {
    name.starts_with(MCP_TOOL_PREFIX)
}

/// True iff this tool call DOES NOT end the agent's turn. Query, scratchpad
/// and MCP tools all loop back to the LLM so the agent can act with the
/// fetched data / breadcrumb / fs result in mind.
fn is_auxiliary_tool(name: &str) -> bool {
    is_query_tool(name) || is_scratchpad_tool(name) || is_mcp_tool(name)
}

/// True iff this AgentMessageKind concludes the agent's turn. Sprint 4
/// refinement: WORK_START and PROGRESS are "soft actions" — they dispatch
/// a message to the team but the emitter keeps thinking (typically to
/// follow up with fs__write_file calls and a final DONE). Without this
/// rule a developer agent emitting WORK_START would block forever waiting
/// for someone to ASK_AGENT them back.
fn is_terminal_message_kind(kind: &AgentMessageKind) -> bool {
    matches!(
        kind,
        AgentMessageKind::Broadcast { .. }
            | AgentMessageKind::AskAgent { .. }
            | AgentMessageKind::Answer { .. }
            | AgentMessageKind::Done { .. }
            | AgentMessageKind::Summary { .. }
    )
    // NOT terminal: WorkStart, Progress, UserInput (UserInput never emitted by agents).
}

/// All tools advertised on every LLM call: action message tools + query
/// tools (Sprint 2.7) + scratchpad tools (Sprint 2.8) + the slice of MCP
/// filesystem tools the role's `mcp_access` permits (Sprint 4 + the role-
/// scoping refinement after the first GUI run).
///
/// Filtering BEFORE advertising is deliberate: the model literally can't
/// pick what it isn't shown. PM can't accidentally write code when only
/// engineers see `fs__write_file`.
fn build_all_tools(mcp: Option<&McpClient>, access: RoleMcpAccess) -> Vec<Tool> {
    let mut tools = message_tools();
    tools.extend(query_tools());
    tools.extend(scratchpad_tools());

    if matches!(access, RoleMcpAccess::None) {
        return tools;
    }

    if let Some(client) = mcp {
        for t in client.advertised_tools() {
            let allowed = match access {
                RoleMcpAccess::All => true,
                RoleMcpAccess::ReadOnly => matches!(classify(&t.function.name), Danger::ReadOnly),
                RoleMcpAccess::None => unreachable!("None short-circuited above"),
            };
            if allowed {
                tools.push(t.clone());
            }
        }
    }
    tools
}

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
}

/// Spawn one Agent task. The returned `JoinHandle` lets the session
/// orchestrator await graceful shutdown when the inbox closes.
pub fn spawn_agent(boot: AgentBoot) -> JoinHandle<()> {
    tokio::spawn(run_agent(boot))
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
        mcp,
        app_handle,
        approval,
        pending_approvals,
    } = boot;

    // Pre-compute the full tool list once per agent (MCP tools are static
    // across the session — server's tool catalog doesn't change, role's
    // mcp_access doesn't change either).
    let tools = build_all_tools(mcp.as_ref(), role.mcp_access);

    tracing::info!(
        target: "aidock::agent",
        role = %role.id,
        recovered_msgs = initial_history.len(),
        state = initial_state.tag(),
        mcp_access = ?role.mcp_access,
        advertised_tools = tools.len(),
        "agent task started"
    );

    let provider = BailianProvider::new(api_key);
    let mut state = initial_state;
    let mut history: Vec<AgentMessage> = initial_history;
    let mut scratchpad = initial_scratchpad;

    while let Some(msg) = inbox_rx.recv().await {
        tracing::debug!(
            target: "aidock::agent",
            role = %role.id,
            in_kind = msg.kind.tag(),
            from = %msg.sender,
            state = state.tag(),
            "inbox received"
        );

        history.push(msg.clone());

        // Did this message resolve an ASK we sent? Captured BEFORE absorbing
        // so we can both transition state AND let `should_respond` know this
        // is a "you just got the answer you were waiting for, think about
        // next steps" situation.
        let resolved_my_pending = matches!(
            (&msg.kind, &state),
            (
                AgentMessageKind::Answer { reply_to, .. },
                AgentState::WaitingAnswer { for_message, .. },
            ) if reply_to == for_message
        );

        if resolved_my_pending {
            state = AgentState::Idle;
            tracing::debug!(
                target: "aidock::agent",
                role = %role.id,
                "pending ASK resolved by incoming ANSWER"
            );
        }

        // Trigger a think either because:
        //   (a) the message is one our policy responds to, or
        //   (b) it just answered a question we were waiting on.
        let should_think = resolved_my_pending || should_respond(&role, &state, &msg);
        if !should_think {
            continue;
        }

        // Build the Level 0 context (Sprint 2.5) + scratchpad (Sprint 2.8).
        let mut chat_messages = build_level_0_context(&role, &history, &msg, &scratchpad);

        // Multi-turn LLM loop: query tools (Sprint 2.7) fetch data and feed
        // back into the conversation; we stop as soon as the model emits at
        // least one *action* tool call, or after MAX_QUERY_LOOP_ITERATIONS.
        let mut iter = 0usize;
        let emitted_action = loop {
            iter += 1;
            let req = ChatRequest {
                model: role.model.primary.clone(),
                messages: chat_messages.clone(),
                temperature: Some(role.model.temperature),
                max_tokens: Some(role.model.max_tokens),
                tools: tools.clone(),
                tool_choice: Some(json!("auto")),
            };

            let resp = match provider.chat_completion(req).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(
                        target: "aidock::agent",
                        role = %role.id,
                        error = %e,
                        iter,
                        "LLM call failed; agent stays silent this turn"
                    );
                    break false;
                }
            };

            if resp.tool_calls.is_empty() {
                tracing::warn!(
                    target: "aidock::agent",
                    role = %role.id,
                    iter,
                    content_preview = ?resp.content.as_deref().map(|s| &s[..s.len().min(120)]),
                    "LLM returned no tool calls; turn skipped"
                );
                break false;
            }

            // Unified per-tool execution. Every tool call yields a
            // tool-role response that we feed back to the model; the turn
            // ends only when at least one *terminal* message (BROADCAST,
            // ASK_AGENT, ANSWER, DONE, SUMMARY) lands.
            chat_messages.push(ChatMessage::Assistant {
                content: resp.content.clone(),
                tool_calls: resp.tool_calls.clone(),
            });

            let mut turn_terminated = false;
            for tool_call in &resp.tool_calls {
                let name = &tool_call.function.name;
                let args = &tool_call.function.arguments;
                let body = if is_query_tool(name) {
                    execute_query_tool(name, args, &history)
                        .unwrap_or_else(|e| format!("query failed: {e}"))
                } else if is_scratchpad_tool(name) {
                    match apply_scratchpad_update(&mut scratchpad, args) {
                        Ok(confirmation) => {
                            // Sprint 3: persist after every successful update.
                            if let Err(e) = scratchpad.save(&scratchpad_path) {
                                tracing::error!(
                                    target: "aidock::agent",
                                    role = %role.id,
                                    path = %scratchpad_path.display(),
                                    error = %e,
                                    "scratchpad in-memory updated but save FAILED — next restart loses this update"
                                );
                            } else {
                                tracing::info!(
                                    target: "aidock::agent",
                                    role = %role.id,
                                    iter,
                                    "{}", confirmation
                                );
                            }
                            confirmation
                        }
                        Err(e) => format!("scratchpad update failed: {e}"),
                    }
                } else if is_mcp_tool(name) {
                    // Sprint 4.6: gate destructive tools. Skipped entirely
                    // in headless mode (no app_handle = no one to ask).
                    let approval_outcome = if app_handle.is_some() {
                        gate_mcp_call(
                            &role.id,
                            name,
                            args,
                            &approval,
                            &pending_approvals,
                            app_handle.as_ref(),
                        )
                        .await
                    } else {
                        ApprovalOutcome::Proceed
                    };

                    let body = match approval_outcome {
                        ApprovalOutcome::Rejected(reason) => {
                            tracing::info!(
                                target: "aidock::agent",
                                role = %role.id,
                                tool = %name,
                                "MCP call denied by user/policy"
                            );
                            format!("DENIED: {reason}")
                        }
                        ApprovalOutcome::Proceed => match mcp.as_ref() {
                            Some(client) => match client.call_tool(name, args).await {
                                Ok(body) => {
                                    tracing::info!(
                                        target: "aidock::agent",
                                        role = %role.id,
                                        tool = %name,
                                        iter,
                                        body_len = body.len(),
                                        "MCP tool returned"
                                    );
                                    body
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        target: "aidock::agent",
                                        role = %role.id,
                                        tool = %name,
                                        error = %e,
                                        "MCP tool call failed"
                                    );
                                    format!("MCP tool '{name}' failed: {e}")
                                }
                            },
                            None => format!(
                                "MCP tool '{name}' is not available — the filesystem server didn't start this session"
                            ),
                        },
                    };
                    // Sprint 4.5: surface this call to the chat panel
                    // even if it failed — users want to see the attempt,
                    // not just the success.
                    if let Some(handle) = app_handle.as_ref() {
                        let event = make_call_event(&role.id, name, args, &body, now_ms());
                        if let Err(e) = handle.emit(MCP_CALL_EVENT, &event) {
                            tracing::warn!(
                                target: "aidock::agent",
                                role = %role.id,
                                error = %e,
                                "failed to emit MCP call event"
                            );
                        }
                    }
                    body
                } else {
                    // Protocol message tool (BROADCAST, ASK_AGENT, ANSWER,
                    // WORK_START, PROGRESS, DONE, SUMMARY).
                    match parse_tool_call(name, args) {
                        Ok(parsed) => {
                            let outgoing = build_outgoing(&role, parsed, Some(&msg.topic_id));
                            apply_outgoing_state_transition(&mut state, &outgoing);
                            // Sprint 2.4 fix: own emits go into local
                            // history so the next think doesn't duplicate.
                            history.push(outgoing.clone());
                            let kind_for_term_check = outgoing.kind.clone();
                            let tag = outgoing.kind.tag();

                            if let Err(e) = dispatcher.submit(outgoing).await {
                                tracing::error!(
                                    target: "aidock::agent",
                                    role = %role.id,
                                    error = %e,
                                    "dispatcher submit failed; agent stopping"
                                );
                                return;
                            }

                            if is_terminal_message_kind(&kind_for_term_check) {
                                turn_terminated = true;
                            }
                            // For soft-actions (WORK_START / PROGRESS),
                            // tell the model the dispatch happened and
                            // it should continue — typically with the
                            // fs__write_file calls that actually do the
                            // work.
                            if is_terminal_message_kind(&kind_for_term_check) {
                                format!("{} dispatched to the team", tag)
                            } else {
                                format!(
                                    "{} dispatched. Continue with the task — write your files via fs__ tools, then emit DONE when finished.",
                                    tag
                                )
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                target: "aidock::agent",
                                role = %role.id,
                                tool = %name,
                                error = %e,
                                "could not parse tool call; skipped"
                            );
                            format!("could not parse '{name}': {e}")
                        }
                    }
                };
                chat_messages.push(ChatMessage::Tool {
                    content: body,
                    tool_call_id: tool_call.id.clone(),
                });
            }

            if turn_terminated {
                break true;
            }
            if iter >= MAX_QUERY_LOOP_ITERATIONS {
                tracing::warn!(
                    target: "aidock::agent",
                    role = %role.id,
                    "agent loop hit MAX_QUERY_LOOP_ITERATIONS without a terminal action — giving up this turn"
                );
                break false;
            }
            // Otherwise loop: model gets to see all tool results in chat_messages
            // and decide its next move (typically more fs__ writes or DONE).
        };
        let _ = emitted_action;
    }

    tracing::info!(target: "aidock::agent", role = %role.id, "agent task ended");
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
        AgentMessageKind::UserInput { .. } => is_pm(role),

        // BROADCAST is pickup-optional per design §4.1. V0.1 makeshift
        // relevance filter (Sprint 2.6 replaces this):
        //   - PM picks up everything (coordinator)
        //   - Other agents pick up if the broadcast names their role id
        //   - WORKING agents stay focused — don't get distracted by broadcasts
        AgentMessageKind::Broadcast { content } => {
            if is_pm(role) {
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

fn is_pm(role: &RoleConfig) -> bool {
    role.id == PM_ID
}

// ---------- assembling and tracking outgoing messages ----------

fn build_outgoing(role: &RoleConfig, parsed: ParsedToolCall, latest_topic: Option<&TopicId>) -> AgentMessage {
    let topic_id = parsed
        .topic_id
        .or_else(|| latest_topic.cloned())
        .unwrap_or_else(|| format!("t-{}", Uuid::new_v4()));

    // Only carry the title if the agent claimed THIS message opens a topic.
    // Sprint 2.6: dispatcher stamps the title on the Topic on first sighting.
    let opens_topic_title = parsed.new_topic_title;

    AgentMessage {
        id: format!("m-{}", Uuid::new_v4()),
        sender: role.id.clone(),
        topic_id,
        timestamp: now_ms(),
        kind: parsed.kind,
        opens_topic_title,
    }
}

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

/// Result of the approval gate for one MCP tool call.
enum ApprovalOutcome {
    /// Either the tool was read-only, a prior decision approved, or the
    /// user explicitly approved this request.
    Proceed,
    /// The user rejected, a prior decision rejected, or the request
    /// timed out. `reason` is the string surfaced to the LLM as the
    /// tool result body.
    Rejected(String),
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

    let decision = match tokio::time::timeout(
        Duration::from_secs(APPROVAL_TIMEOUT_SECS),
        rx,
    )
    .await
    {
        Ok(Ok(d)) => d,
        Ok(Err(_)) => {
            // Sender dropped without firing — treat as rejection.
            tracing::warn!(
                target: "aidock::agent",
                agent,
                tool,
                request_id = %id,
                "approval sender dropped; auto-rejecting"
            );
            Decision::Reject
        }
        Err(_) => {
            pending.lock().await.remove(&id);
            tracing::warn!(
                target: "aidock::agent",
                agent,
                tool,
                request_id = %id,
                timeout_s = APPROVAL_TIMEOUT_SECS,
                "approval timed out; auto-rejecting"
            );
            Decision::Reject
        }
    };

    registry.remember(agent, tool, decision).await;

    if decision.allows_execution() {
        ApprovalOutcome::Proceed
    } else {
        ApprovalOutcome::Rejected("user rejected this tool call".to_string())
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

    #[test]
    fn outgoing_carries_role_and_topic() {
        let role = pm_role();
        let parsed = ParsedToolCall {
            kind: AgentMessageKind::Broadcast { content: "x".into() },
            topic_id: Some("t-7".into()),
            new_topic_title: None,
        };
        let m = build_outgoing(&role, parsed, None);
        assert_eq!(m.sender, "PM");
        assert_eq!(m.topic_id, "t-7");
        assert!(m.id.starts_with("m-"));
        assert!(m.opens_topic_title.is_none());
    }

    #[test]
    fn outgoing_inherits_latest_topic_when_unset() {
        let role = pm_role();
        let parsed = ParsedToolCall {
            kind: AgentMessageKind::Broadcast { content: "x".into() },
            topic_id: None,
            new_topic_title: None,
        };
        let latest = "t-existing".to_string();
        let m = build_outgoing(&role, parsed, Some(&latest));
        assert_eq!(m.topic_id, "t-existing");
    }

    #[test]
    fn outgoing_carries_new_topic_title() {
        // When the model declares it's opening a new topic, the title must
        // flow through to the AgentMessage so the dispatcher can stamp it
        // on the Topic.
        let role = pm_role();
        let parsed = ParsedToolCall {
            kind: AgentMessageKind::Broadcast {
                content: "kicking off frontend stack discussion".into(),
            },
            topic_id: Some("t-9".into()),
            new_topic_title: Some("Frontend stack pick".into()),
        };
        let m = build_outgoing(&role, parsed, None);
        assert_eq!(m.topic_id, "t-9");
        assert_eq!(
            m.opens_topic_title.as_deref(),
            Some("Frontend stack pick")
        );
    }
}
