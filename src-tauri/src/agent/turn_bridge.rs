//! ③ ↔ ④.a boundary: translate the loop's ④-neutral outward actions into
//! typed `AgentMessageKind` values the dispatcher understands.
//!
//! This is the symmetric counterpart to `llm_source::translate` (which goes
//! `ChatResponse → Action`). Keeping it here — not in `react.rs` — is what lets
//! ④.a stay free of any `AgentMessageKind` import (CLAUDE.md ④.a「选 A」). The
//! runtime calls these when draining a finished/suspended turn into messages.

use crate::agent::message::AgentMessageKind;
use crate::agent::react::Outbound;

/// A non-suspending outward message → its typed kind.
pub fn outbound_to_kind(out: Outbound) -> AgentMessageKind {
    match out {
        Outbound::Broadcast { content } => AgentMessageKind::Broadcast { content },
        Outbound::Answer { reply_to, content } => AgentMessageKind::Answer { reply_to, content },
    }
}

/// A `Suspended` turn's question → an ASK_AGENT kind. The runtime dispatches
/// this (capturing its message id for WAITING_ANSWER) before parking the turn.
pub fn ask_to_kind(
    to: String,
    content: String,
    expected_format: Option<String>,
) -> AgentMessageKind {
    AgentMessageKind::AskAgent {
        to,
        content,
        expected_format,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_maps_through() {
        match outbound_to_kind(Outbound::Broadcast {
            content: "team, kicking off".into(),
        }) {
            AgentMessageKind::Broadcast { content } => assert_eq!(content, "team, kicking off"),
            other => panic!("expected Broadcast, got {other:?}"),
        }
    }

    #[test]
    fn answer_keeps_reply_to() {
        match outbound_to_kind(Outbound::Answer {
            reply_to: "m-42".into(),
            content: "use JWT".into(),
        }) {
            AgentMessageKind::Answer { reply_to, content } => {
                assert_eq!(reply_to, "m-42");
                assert_eq!(content, "use JWT");
            }
            other => panic!("expected Answer, got {other:?}"),
        }
    }

    #[test]
    fn ask_carries_all_fields() {
        match ask_to_kind(
            "backend_dev".into(),
            "which auth?".into(),
            Some("one line".into()),
        ) {
            AgentMessageKind::AskAgent {
                to,
                content,
                expected_format,
            } => {
                assert_eq!(to, "backend_dev");
                assert_eq!(content, "which auth?");
                assert_eq!(expected_format.as_deref(), Some("one line"));
            }
            other => panic!("expected AskAgent, got {other:?}"),
        }
    }
}
