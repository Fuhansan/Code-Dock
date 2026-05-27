//! Topics — coarse-grained "conversation segments" that group related messages.
//!
//! Every `AgentMessage` carries a `topic_id`. Topics are how the design's
//! context system stays cheap: closed topics collapse to (title + summary +
//! key_decisions) in Level 0 context, and Agents pull the full message
//! stream on demand via `recall_topic` (Sprint 2.7).
//!
//! Users don't see "topic" as a word in the UI (design §10.2.2) — only as
//! divider lines + a one-liner SUMMARY between conversation segments.

use serde::{Deserialize, Serialize};

use crate::agent::message::{AgentId, TopicId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TopicStatus {
    /// Topic is open — new messages can still join.
    Active,
    /// `SUMMARY` has been emitted; no new messages.
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    pub id: TopicId,
    /// Short, human-readable title. Filled by the Agent that opened the topic.
    pub title: String,
    pub status: TopicStatus,
    /// Roles that have spoken at least once in this topic.
    pub participants: Vec<AgentId>,
    /// Unix ms when the topic opened.
    pub started_at: i64,
    /// Unix ms when SUMMARY landed. None while Active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<i64>,
    /// One-paragraph summary produced at close. Empty while Active.
    #[serde(default)]
    pub summary: String,
    /// Concrete decisions made during this topic, extracted at close.
    #[serde(default)]
    pub key_decisions: Vec<String>,
}

impl Topic {
    /// Create a new Active topic with no participants and no summary yet.
    pub fn open(id: impl Into<TopicId>, title: impl Into<String>, opened_at: i64) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status: TopicStatus::Active,
            participants: Vec::new(),
            started_at: opened_at,
            closed_at: None,
            summary: String::new(),
            key_decisions: Vec::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status, TopicStatus::Active)
    }

    /// Add `agent` to the participants set (no duplicates).
    pub fn add_participant(&mut self, agent: &AgentId) {
        if !self.participants.iter().any(|p| p == agent) {
            self.participants.push(agent.clone());
        }
    }

    /// Transition Active → Closed and stamp the door-plate fields.
    /// Idempotent: closing an already-closed topic is a no-op.
    pub fn close(&mut self, summary: String, key_decisions: Vec<String>, closed_at: i64) {
        if !self.is_active() {
            return;
        }
        self.status = TopicStatus::Closed;
        self.summary = summary;
        self.key_decisions = key_decisions;
        self.closed_at = Some(closed_at);
    }

    /// The "index entry" projection used in Level 0 context for closed topics.
    /// Cheap; trims the full message stream out.
    pub fn index_entry(&self) -> TopicIndexEntry {
        TopicIndexEntry {
            id: self.id.clone(),
            title: self.title.clone(),
            status: self.status,
            summary: self.summary.clone(),
            key_decisions: self.key_decisions.clone(),
            participants: self.participants.clone(),
        }
    }
}

/// Lightweight projection of `Topic` for Level 0 context injection. No
/// timestamps, no full message stream — just enough to let an Agent decide
/// whether to call `recall_topic` to dig deeper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicIndexEntry {
    pub id: TopicId,
    pub title: String,
    pub status: TopicStatus,
    pub summary: String,
    pub key_decisions: Vec<String>,
    pub participants: Vec<AgentId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_topic_is_active() {
        let t = Topic::open("t-1", "Frontend stack pick", 1000);
        assert_eq!(t.status, TopicStatus::Active);
        assert!(t.is_active());
        assert!(t.summary.is_empty());
        assert!(t.key_decisions.is_empty());
        assert!(t.closed_at.is_none());
    }

    #[test]
    fn add_participant_dedupes() {
        let mut t = Topic::open("t-1", "x", 0);
        t.add_participant(&"PM".to_string());
        t.add_participant(&"PM".to_string());
        t.add_participant(&"frontend_dev".to_string());
        assert_eq!(t.participants, vec!["PM", "frontend_dev"]);
    }

    #[test]
    fn close_writes_summary_and_decisions() {
        let mut t = Topic::open("t-1", "API shape", 1000);
        t.close(
            "Settled on REST".into(),
            vec!["use POST /todos".into(), "id is uuid".into()],
            5000,
        );
        assert_eq!(t.status, TopicStatus::Closed);
        assert_eq!(t.closed_at, Some(5000));
        assert_eq!(t.summary, "Settled on REST");
        assert_eq!(t.key_decisions.len(), 2);
    }

    #[test]
    fn close_is_idempotent() {
        let mut t = Topic::open("t-1", "x", 0);
        t.close("first".into(), vec![], 100);
        t.close("second".into(), vec!["nope".into()], 200);
        // The second close should NOT overwrite.
        assert_eq!(t.summary, "first");
        assert_eq!(t.closed_at, Some(100));
        assert!(t.key_decisions.is_empty());
    }

    #[test]
    fn index_entry_drops_timestamps() {
        let mut t = Topic::open("t-1", "x", 100);
        t.close("done".into(), vec!["d1".into()], 200);
        let idx = t.index_entry();
        assert_eq!(idx.id, "t-1");
        assert_eq!(idx.summary, "done");
        assert_eq!(idx.key_decisions, vec!["d1"]);
    }
}
