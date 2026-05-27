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

use crate::agent::message::MessageId;

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
}
