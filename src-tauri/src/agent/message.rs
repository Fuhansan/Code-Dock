//! Typed messages flowing through the multi-agent group chat.
//!
//! Per AIDOCK_DESIGN.md §4.1, every message an Agent emits is one of a small
//! set of structured kinds — not free text. The kind drives routing
//! (`ASK_AGENT` forces a response from a specific recipient, `BROADCAST` is
//! pickup-optional) and the state machine (`WORK_START` toggles WORKING,
//! `DONE` releases it).
//!
//! `UserInput` is the only kind originating from outside the agent system —
//! the human user, who is "god" in the design and can address anyone.
//!
//! All variants are flat — no nested enum payloads — so the JSONL stream in
//! Sprint 3 stays trivially greppable.

use serde::{Deserialize, Serialize};

pub type MessageId = String;
pub type TopicId = String;
pub type AgentId = String;

/// One line in the chat history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: MessageId,
    /// Sender's role id, or `"user"` for `UserInput` kinds.
    pub sender: AgentId,
    /// Topic this message belongs to. Every message belongs to exactly one topic.
    pub topic_id: TopicId,
    /// Unix milliseconds.
    pub timestamp: i64,
    pub kind: AgentMessageKind,
    /// Iff this message opens a *new* topic, the human-readable title the
    /// sender declared (via the tool's `new_topic_title` arg). The dispatcher
    /// picks this up on first sighting of `topic_id` and stamps it on the
    /// `Topic`. Subsequent messages in the same topic leave this `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opens_topic_title: Option<String>,
}

impl AgentMessage {
    /// Builder helper that defaults `opens_topic_title` to `None`. Keeps test
    /// and runtime construction sites compact.
    pub fn new(
        id: impl Into<MessageId>,
        sender: impl Into<AgentId>,
        topic_id: impl Into<TopicId>,
        timestamp: i64,
        kind: AgentMessageKind,
    ) -> Self {
        Self {
            id: id.into(),
            sender: sender.into(),
            topic_id: topic_id.into(),
            timestamp,
            kind,
            opens_topic_title: None,
        }
    }
}

/// The discriminated message payload. Tag-on-the-wire is `type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AgentMessageKind {
    /// Speak to the whole team. Recipients self-decide whether to respond.
    Broadcast { content: String },

    /// Direct a question at one teammate; they MUST answer.
    AskAgent {
        to: AgentId,
        content: String,
        /// Optional hint about the expected response shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_format: Option<String>,
    },

    /// Reply to a specific `ASK_AGENT`. Closes that question.
    Answer {
        reply_to: MessageId,
        content: String,
    },

    /// Announce that the sender has started a task; flips them to WORKING.
    WorkStart {
        /// Short, human-readable description of what they're about to do.
        task: String,
    },

    /// In-progress status; doesn't change state, just informational.
    Progress {
        task: String,
        /// 0..=100. The serde wire keeps it as u8 — saturating is the
        /// caller's problem.
        percent: u8,
        #[serde(default)]
        note: String,
    },

    /// Sender finished. Releases them from WORKING.
    Done {
        /// One-line summary of what was produced.
        summary: String,
        /// Optional artifact id this Done refers to (Sprint 3+).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_id: Option<String>,
    },

    /// Topic-closing summary (the "door plate" for future recall).
    Summary {
        /// The topic being closed. Must equal the enclosing `AgentMessage.topic_id`.
        topic_id: TopicId,
        summary: String,
        #[serde(default)]
        key_decisions: Vec<String>,
    },

    /// Human user spoke. `sender` should be `"user"`.
    UserInput { content: String },
}

impl AgentMessageKind {
    /// True if this kind hard-routes to one recipient who must respond.
    pub fn is_directed(&self) -> bool {
        matches!(self, AgentMessageKind::AskAgent { .. })
    }

    /// Recipient role id, if any. None means broadcast / no specific target.
    pub fn directed_to(&self) -> Option<&AgentId> {
        match self {
            AgentMessageKind::AskAgent { to, .. } => Some(to),
            _ => None,
        }
    }

    /// Returns the variant tag as a static string — handy for logging.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Broadcast { .. } => "BROADCAST",
            Self::AskAgent { .. } => "ASK_AGENT",
            Self::Answer { .. } => "ANSWER",
            Self::WorkStart { .. } => "WORK_START",
            Self::Progress { .. } => "PROGRESS",
            Self::Done { .. } => "DONE",
            Self::Summary { .. } => "SUMMARY",
            Self::UserInput { .. } => "USER_INPUT",
        }
    }

    /// Compact, human-readable rendering of the payload for prompts and
    /// recall output. Variant-aware: ASK_AGENT prefixes the recipient,
    /// ANSWER shows the reply_to, SUMMARY appends decisions, etc.
    pub fn payload_preview(&self) -> String {
        match self {
            Self::Broadcast { content } => content.clone(),
            Self::AskAgent { to, content, .. } => format!("(→ {}) {}", to, content),
            Self::Answer { reply_to, content } => format!("(re {}) {}", reply_to, content),
            Self::WorkStart { task } => task.clone(),
            Self::Progress { task, percent, note } => {
                if note.is_empty() {
                    format!("{} — {}%", task, percent)
                } else {
                    format!("{} — {}% — {}", task, percent, note)
                }
            }
            Self::Done { summary, artifact_id } => match artifact_id {
                Some(id) => format!("{} (artifact: {})", summary, id),
                None => summary.clone(),
            },
            Self::Summary {
                summary,
                key_decisions,
                ..
            } => {
                if key_decisions.is_empty() {
                    summary.clone()
                } else {
                    format!("{} | decisions: {}", summary, key_decisions.join("; "))
                }
            }
            Self::UserInput { content } => content.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_agent_routes_to_recipient() {
        let kind = AgentMessageKind::AskAgent {
            to: "frontend_dev".into(),
            content: "Got time for an API question?".into(),
            expected_format: None,
        };
        assert!(kind.is_directed());
        assert_eq!(kind.directed_to().map(String::as_str), Some("frontend_dev"));
    }

    #[test]
    fn broadcast_is_not_directed() {
        let kind = AgentMessageKind::Broadcast {
            content: "Heads up team".into(),
        };
        assert!(!kind.is_directed());
        assert_eq!(kind.directed_to(), None);
    }

    #[test]
    fn wire_format_uses_screaming_snake_case() {
        let m = AgentMessage::new(
            "m-1",
            "PM",
            "t-1",
            0,
            AgentMessageKind::AskAgent {
                to: "frontend_dev".into(),
                content: "x".into(),
                expected_format: None,
            },
        );
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["kind"]["type"], "ASK_AGENT");
        assert_eq!(v["kind"]["to"], "frontend_dev");
        // opens_topic_title omitted when None
        assert!(v.get("opens_topic_title").is_none());
    }

    #[test]
    fn opens_topic_title_round_trips() {
        let mut m = AgentMessage::new(
            "m-1",
            "PM",
            "t-2",
            0,
            AgentMessageKind::Broadcast {
                content: "kick off".into(),
            },
        );
        m.opens_topic_title = Some("Frontend stack pick".into());
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["opens_topic_title"], "Frontend stack pick");
        let back: AgentMessage = serde_json::from_value(v).unwrap();
        assert_eq!(back.opens_topic_title.as_deref(), Some("Frontend stack pick"));
    }

    #[test]
    fn tag_is_screaming_snake() {
        assert_eq!(
            AgentMessageKind::WorkStart { task: "x".into() }.tag(),
            "WORK_START"
        );
        assert_eq!(
            AgentMessageKind::UserInput { content: "hi".into() }.tag(),
            "USER_INPUT"
        );
    }
}
