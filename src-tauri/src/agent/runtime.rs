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

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::agent::context::build_level_0_context;
use crate::agent::dispatcher::DispatcherHandle;
use crate::agent::message::{AgentMessage, AgentMessageKind, TopicId};
use crate::agent::protocol::{message_tools, parse_tool_call, ParsedToolCall};
use crate::agent::role::RoleConfig;
use crate::agent::roles::PM_ID;
use crate::agent::state::AgentState;
use crate::llm::bailian::BailianProvider;
use crate::llm::{ChatRequest, LLMProvider};

/// Spawn one Agent task. The returned `JoinHandle` lets the session
/// orchestrator await graceful shutdown when the inbox closes.
pub fn spawn_agent(
    role: RoleConfig,
    inbox_rx: mpsc::Receiver<AgentMessage>,
    dispatcher: DispatcherHandle,
    api_key: String,
) -> JoinHandle<()> {
    tokio::spawn(run_agent(role, inbox_rx, dispatcher, api_key))
}

async fn run_agent(
    role: RoleConfig,
    mut inbox_rx: mpsc::Receiver<AgentMessage>,
    dispatcher: DispatcherHandle,
    api_key: String,
) {
    tracing::info!(target: "aidock::agent", role = %role.id, "agent task started");
    let provider = BailianProvider::new(api_key);
    let mut state = AgentState::Idle;
    let mut history: Vec<AgentMessage> = Vec::new();

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

        // Build the LLM input via the Level 0 context builder (Sprint 2.5).
        let llm_input = build_level_0_context(&role, &history, &msg);

        let req = ChatRequest {
            model: role.model.primary.clone(),
            messages: llm_input,
            temperature: Some(role.model.temperature),
            max_tokens: Some(role.model.max_tokens),
            tools: message_tools(),
            tool_choice: Some(json!("auto")),
        };

        let resp = match provider.chat_completion(req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    target: "aidock::agent",
                    role = %role.id,
                    error = %e,
                    "LLM call failed; agent stays silent this turn"
                );
                continue;
            }
        };

        if resp.tool_calls.is_empty() {
            // Sprint 1 harness showed Qwen reliably picks a tool, but be
            // defensive — silent skip on the rare free-text response.
            tracing::warn!(
                target: "aidock::agent",
                role = %role.id,
                content_preview = ?resp.content.as_deref().map(|s| &s[..s.len().min(120)]),
                "LLM returned no tool calls; turn skipped"
            );
            continue;
        }

        for tool_call in resp.tool_calls {
            let parsed = match parse_tool_call(&tool_call.function.name, &tool_call.function.arguments) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        target: "aidock::agent",
                        role = %role.id,
                        tool = %tool_call.function.name,
                        error = %e,
                        "could not parse tool call; skipped"
                    );
                    continue;
                }
            };

            // The topic we'll inherit if the LLM didn't pin one in the tool call.
            let outgoing = build_outgoing(&role, parsed, Some(&msg.topic_id));
            // Transition state BEFORE submitting — if the submit fails we
            // still reflect the local commitment correctly.
            apply_outgoing_state_transition(&mut state, &outgoing);
            // Push our own emit into local history so the next think sees
            // what we already said. Without this the LLM thinks unanswered
            // ASKs are still pending and produces duplicate ANSWERs
            // (witnessed in the first multi-agent demo run).
            history.push(outgoing.clone());

            if let Err(e) = dispatcher.submit(outgoing).await {
                tracing::error!(
                    target: "aidock::agent",
                    role = %role.id,
                    error = %e,
                    "dispatcher submit failed; agent stopping"
                );
                return;
            }
        }
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

    AgentMessage {
        id: format!("m-{}", Uuid::new_v4()),
        sender: role.id.clone(),
        topic_id,
        timestamp: now_ms(),
        kind: parsed.kind,
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

/// Build an inbound message representing a user line. Exposed so the Tauri
/// command can craft one without duplicating the wire details.
pub fn user_input_message(content: String, topic_id: TopicId) -> AgentMessage {
    AgentMessage {
        id: format!("m-{}", Uuid::new_v4()),
        sender: "user".to_string(),
        topic_id,
        timestamp: now_ms(),
        kind: AgentMessageKind::UserInput { content },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::roles::{frontend_role, pm_role};

    fn ask(to: &str) -> AgentMessage {
        AgentMessage {
            id: "m-1".into(),
            sender: "PM".into(),
            topic_id: "t-1".into(),
            timestamp: 0,
            kind: AgentMessageKind::AskAgent {
                to: to.into(),
                content: "?".into(),
                expected_format: None,
            },
        }
    }

    fn user(content: &str) -> AgentMessage {
        AgentMessage {
            id: "m-u".into(),
            sender: "user".into(),
            topic_id: "t-1".into(),
            timestamp: 0,
            kind: AgentMessageKind::UserInput { content: content.into() },
        }
    }

    fn broadcast(from: &str) -> AgentMessage {
        AgentMessage {
            id: "m-b".into(),
            sender: from.into(),
            topic_id: "t-1".into(),
            timestamp: 0,
            kind: AgentMessageKind::Broadcast { content: "hi".into() },
        }
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
        AgentMessage {
            id: "m-b".into(),
            sender: from.into(),
            topic_id: "t-1".into(),
            timestamp: 0,
            kind: AgentMessageKind::Broadcast {
                content: content.into(),
            },
        }
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
        let m = AgentMessage {
            id: "m-out".into(),
            sender: "PM".into(),
            topic_id: "t-1".into(),
            timestamp: 100,
            kind: AgentMessageKind::AskAgent {
                to: "frontend_dev".into(),
                content: "?".into(),
                expected_format: None,
            },
        };
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
        let m1 = AgentMessage {
            id: "m1".into(),
            sender: "frontend_dev".into(),
            topic_id: "t-1".into(),
            timestamp: 1,
            kind: AgentMessageKind::WorkStart { task: "build".into() },
        };
        apply_outgoing_state_transition(&mut state, &m1);
        assert!(state.is_working());

        let m2 = AgentMessage {
            id: "m2".into(),
            sender: "frontend_dev".into(),
            topic_id: "t-1".into(),
            timestamp: 2,
            kind: AgentMessageKind::Done { summary: "done".into(), artifact_id: None },
        };
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
}
