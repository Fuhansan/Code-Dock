//! Agent runtime state machine.
//!
//! Per AIDOCK_DESIGN.md §4.2, every Agent is in exactly one of three states.
//! Transitions are driven by message emission, not by external commands:
//!
//! ```text
//!   IDLE ──WORK_START──▶ WORKING
//!     │                     │
//!     ASK_AGENT              DONE
//!     │                     │
//!     ▼                     ▼
//!   WAITING_ANSWER ◀───────  IDLE
//!     │
//!     ANSWER (matching reply_to)
//!     ▼
//!   IDLE
//! ```
//!
//! A WORKING Agent ignores BROADCASTs (too distracting), but a directed
//! ASK_AGENT still preempts. That's the design's "you can interrupt the
//! engineer if you really need to" rule.

use serde::{Deserialize, Serialize};

use crate::agent::message::{AgentMessage, AgentMessageKind, MessageId};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentState {
    Idle,
    Working {
        /// What the Agent reported it's doing. Mirrors the `WORK_START.task`
        /// field that drove the transition.
        task: String,
        /// Unix ms when WORKING began.
        since: i64,
    },
    WaitingAnswer {
        /// The ASK_AGENT message we sent and are waiting on.
        for_message: MessageId,
        since: i64,
    },
}

impl AgentState {
    pub fn is_idle(&self) -> bool {
        matches!(self, AgentState::Idle)
    }

    pub fn is_working(&self) -> bool {
        matches!(self, AgentState::Working { .. })
    }

    pub fn is_waiting(&self) -> bool {
        matches!(self, AgentState::WaitingAnswer { .. })
    }

    /// Variant tag for logging / metrics.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Working { .. } => "WORKING",
            Self::WaitingAnswer { .. } => "WAITING_ANSWER",
        }
    }
}

impl Default for AgentState {
    fn default() -> Self {
        AgentState::Idle
    }
}

impl AgentState {
    /// Reconstruct an Agent's current state by walking its visible message
    /// stream in order. Used at startup after `messages.jsonl` recovery to
    /// re-derive the agent's state machine without needing a separate
    /// state file — the message stream is the source of truth.
    ///
    /// Rules (mirror runtime.rs:apply_outgoing_state_transition + the
    /// ANSWER-resolves-WAITING logic):
    ///   - own WORK_START      → WORKING
    ///   - own DONE            → IDLE
    ///   - own ASK_AGENT       → WAITING_ANSWER for that message
    ///   - incoming ANSWER whose `reply_to` matches the agent's currently-
    ///     pending ASK → IDLE
    ///
    /// Everything else doesn't move the state. SUMMARY isn't a state
    /// change for the emitter.
    pub fn derive_from_history(messages: &[AgentMessage], agent_id: &str) -> Self {
        let mut state = AgentState::Idle;
        for m in messages {
            if m.sender == agent_id {
                match &m.kind {
                    AgentMessageKind::WorkStart { task } => {
                        state = AgentState::Working {
                            task: task.clone(),
                            since: m.timestamp,
                        };
                    }
                    AgentMessageKind::Done { .. } => {
                        state = AgentState::Idle;
                    }
                    AgentMessageKind::AskAgent { .. } => {
                        state = AgentState::WaitingAnswer {
                            for_message: m.id.clone(),
                            since: m.timestamp,
                        };
                    }
                    _ => {}
                }
            } else if let AgentMessageKind::Answer { reply_to, .. } = &m.kind {
                if let AgentState::WaitingAnswer { for_message, .. } = &state {
                    if reply_to == for_message {
                        state = AgentState::Idle;
                    }
                }
            }
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_idle() {
        let s = AgentState::default();
        assert!(s.is_idle());
        assert_eq!(s.tag(), "IDLE");
    }

    #[test]
    fn working_carries_task() {
        let s = AgentState::Working {
            task: "ship login page".into(),
            since: 100,
        };
        assert!(s.is_working());
        assert!(!s.is_idle());
        assert_eq!(s.tag(), "WORKING");
    }

    #[test]
    fn waiting_carries_target_message() {
        let s = AgentState::WaitingAnswer {
            for_message: "m-42".into(),
            since: 500,
        };
        assert!(s.is_waiting());
        assert_eq!(s.tag(), "WAITING_ANSWER");
    }

    #[test]
    fn wire_format_round_trips() {
        let s = AgentState::Working {
            task: "x".into(),
            since: 1,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["kind"], "working");
        let back: AgentState = serde_json::from_value(v).unwrap();
        assert!(back.is_working());
    }

    fn make(id: &str, sender: &str, kind: AgentMessageKind) -> AgentMessage {
        AgentMessage::new(id, sender, "t-1", 100, kind)
    }

    #[test]
    fn derive_empty_history_is_idle() {
        assert!(AgentState::derive_from_history(&[], "PM").is_idle());
    }

    #[test]
    fn derive_work_start_then_done_is_idle() {
        let history = vec![
            make("m1", "frontend_dev", AgentMessageKind::WorkStart { task: "build".into() }),
            make(
                "m2",
                "frontend_dev",
                AgentMessageKind::Done {
                    summary: "shipped".into(),
                    artifact_id: None,
                },
            ),
        ];
        let s = AgentState::derive_from_history(&history, "frontend_dev");
        assert!(s.is_idle());
    }

    #[test]
    fn derive_work_start_no_done_is_working() {
        let history = vec![make(
            "m1",
            "frontend_dev",
            AgentMessageKind::WorkStart { task: "build".into() },
        )];
        let s = AgentState::derive_from_history(&history, "frontend_dev");
        assert!(s.is_working());
    }

    #[test]
    fn derive_unanswered_ask_is_waiting() {
        let history = vec![make(
            "ask-1",
            "PM",
            AgentMessageKind::AskAgent {
                to: "frontend_dev".into(),
                content: "?".into(),
                expected_format: None,
            },
        )];
        let s = AgentState::derive_from_history(&history, "PM");
        match s {
            AgentState::WaitingAnswer { for_message, .. } => assert_eq!(for_message, "ask-1"),
            other => panic!("expected WaitingAnswer, got {other:?}"),
        }
    }

    #[test]
    fn derive_answered_ask_resolves_to_idle() {
        let history = vec![
            make(
                "ask-1",
                "PM",
                AgentMessageKind::AskAgent {
                    to: "frontend_dev".into(),
                    content: "?".into(),
                    expected_format: None,
                },
            ),
            make(
                "ans-1",
                "frontend_dev",
                AgentMessageKind::Answer {
                    reply_to: "ask-1".into(),
                    content: "yes".into(),
                },
            ),
        ];
        let s = AgentState::derive_from_history(&history, "PM");
        assert!(s.is_idle());
    }

    #[test]
    fn derive_unrelated_answer_does_not_resolve() {
        // PM asked m-asked-by-pm. Someone else answers a DIFFERENT ask.
        // PM's WAITING must remain.
        let history = vec![
            make(
                "ask-by-pm",
                "PM",
                AgentMessageKind::AskAgent {
                    to: "frontend_dev".into(),
                    content: "?".into(),
                    expected_format: None,
                },
            ),
            make(
                "ans-other",
                "frontend_dev",
                AgentMessageKind::Answer {
                    reply_to: "different-ask".into(),
                    content: "yes".into(),
                },
            ),
        ];
        let s = AgentState::derive_from_history(&history, "PM");
        assert!(s.is_waiting());
    }
}
