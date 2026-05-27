//! Wire protocol between the LLM and the runtime.
//!
//! The runtime never asks the LLM for free text. Instead it advertises a
//! small set of OpenAI-style function tools — one per `AgentMessageKind`
//! variant the Agent is allowed to emit — and validates the model's
//! `tool_calls` back into typed `AgentMessageKind` values.
//!
//! The Sprint 1 stability harness already verified Qwen produces these
//! shapes reliably (GREEN at 100%).

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::message::{AgentId, AgentMessageKind, MessageId, TopicId};
use crate::llm::Tool;

// ---------- Tool name constants ----------

pub const TOOL_BROADCAST: &str = "BROADCAST";
pub const TOOL_ASK_AGENT: &str = "ASK_AGENT";
pub const TOOL_ANSWER: &str = "ANSWER";
pub const TOOL_WORK_START: &str = "WORK_START";
pub const TOOL_PROGRESS: &str = "PROGRESS";
pub const TOOL_DONE: &str = "DONE";
pub const TOOL_SUMMARY: &str = "SUMMARY";

// ---------- Argument shapes (deserialize targets) ----------
//
// One per tool. These match the JSON Schema the LLM is asked to fill, then
// we lift them into the typed `AgentMessageKind` so the rest of the runtime
// never touches raw JSON.

#[derive(Debug, Deserialize)]
struct BroadcastArgs {
    content: String,
    #[serde(default)]
    topic_id: Option<TopicId>,
    /// Optional title for opening a new topic in this turn.
    #[serde(default)]
    new_topic_title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AskAgentArgs {
    to: AgentId,
    content: String,
    #[serde(default)]
    topic_id: Option<TopicId>,
    #[serde(default)]
    new_topic_title: Option<String>,
    #[serde(default)]
    expected_format: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnswerArgs {
    reply_to: MessageId,
    content: String,
    #[serde(default)]
    topic_id: Option<TopicId>,
}

#[derive(Debug, Deserialize)]
struct WorkStartArgs {
    task: String,
    #[serde(default)]
    topic_id: Option<TopicId>,
}

#[derive(Debug, Deserialize)]
struct ProgressArgs {
    task: String,
    percent: u8,
    #[serde(default)]
    note: String,
    #[serde(default)]
    topic_id: Option<TopicId>,
}

#[derive(Debug, Deserialize)]
struct DoneArgs {
    summary: String,
    #[serde(default)]
    artifact_id: Option<String>,
    #[serde(default)]
    topic_id: Option<TopicId>,
}

#[derive(Debug, Deserialize)]
struct SummaryArgs {
    topic_id: TopicId,
    summary: String,
    #[serde(default)]
    key_decisions: Vec<String>,
}

// ---------- Parsed result ----------

/// What the runtime gets back after parsing one LLM tool call. The
/// `topic_id` (or hint to open one) lives at this outer layer because it's
/// orthogonal to which kind of message was sent.
#[derive(Debug, Clone, Serialize)]
pub struct ParsedToolCall {
    pub kind: AgentMessageKind,
    /// Topic the Agent claims this message belongs to. `None` means the
    /// Agent didn't pick one — runtime falls back to "current topic".
    pub topic_id: Option<TopicId>,
    /// If the Agent wants to open a new topic, this is its title.
    pub new_topic_title: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("unknown tool '{0}'")]
    UnknownTool(String),
    #[error("invalid arguments for tool '{tool}': {source}")]
    InvalidArgs {
        tool: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

/// Parse one LLM tool call into a typed `ParsedToolCall`.
pub fn parse_tool_call(name: &str, raw_args: &str) -> Result<ParsedToolCall, ProtocolError> {
    let parse = |tool: &'static str, raw: &str| -> Result<serde_json::Value, ProtocolError> {
        serde_json::from_str(raw).map_err(|source| ProtocolError::InvalidArgs { tool, source })
    };

    match name {
        TOOL_BROADCAST => {
            let args: BroadcastArgs = serde_json::from_str(raw_args).map_err(|source| {
                ProtocolError::InvalidArgs {
                    tool: TOOL_BROADCAST,
                    source,
                }
            })?;
            Ok(ParsedToolCall {
                kind: AgentMessageKind::Broadcast { content: args.content },
                topic_id: args.topic_id,
                new_topic_title: args.new_topic_title,
            })
        }
        TOOL_ASK_AGENT => {
            let args: AskAgentArgs = serde_json::from_str(raw_args).map_err(|source| {
                ProtocolError::InvalidArgs {
                    tool: TOOL_ASK_AGENT,
                    source,
                }
            })?;
            Ok(ParsedToolCall {
                kind: AgentMessageKind::AskAgent {
                    to: args.to,
                    content: args.content,
                    expected_format: args.expected_format,
                },
                topic_id: args.topic_id,
                new_topic_title: args.new_topic_title,
            })
        }
        TOOL_ANSWER => {
            let args: AnswerArgs = serde_json::from_str(raw_args).map_err(|source| {
                ProtocolError::InvalidArgs {
                    tool: TOOL_ANSWER,
                    source,
                }
            })?;
            Ok(ParsedToolCall {
                kind: AgentMessageKind::Answer {
                    reply_to: args.reply_to,
                    content: args.content,
                },
                topic_id: args.topic_id,
                new_topic_title: None,
            })
        }
        TOOL_WORK_START => {
            let args: WorkStartArgs = serde_json::from_str(raw_args).map_err(|source| {
                ProtocolError::InvalidArgs {
                    tool: TOOL_WORK_START,
                    source,
                }
            })?;
            Ok(ParsedToolCall {
                kind: AgentMessageKind::WorkStart { task: args.task },
                topic_id: args.topic_id,
                new_topic_title: None,
            })
        }
        TOOL_PROGRESS => {
            let args: ProgressArgs = serde_json::from_str(raw_args).map_err(|source| {
                ProtocolError::InvalidArgs {
                    tool: TOOL_PROGRESS,
                    source,
                }
            })?;
            Ok(ParsedToolCall {
                kind: AgentMessageKind::Progress {
                    task: args.task,
                    percent: args.percent.min(100),
                    note: args.note,
                },
                topic_id: args.topic_id,
                new_topic_title: None,
            })
        }
        TOOL_DONE => {
            let args: DoneArgs = serde_json::from_str(raw_args).map_err(|source| {
                ProtocolError::InvalidArgs {
                    tool: TOOL_DONE,
                    source,
                }
            })?;
            Ok(ParsedToolCall {
                kind: AgentMessageKind::Done {
                    summary: args.summary,
                    artifact_id: args.artifact_id,
                },
                topic_id: args.topic_id,
                new_topic_title: None,
            })
        }
        TOOL_SUMMARY => {
            let args: SummaryArgs = serde_json::from_str(raw_args).map_err(|source| {
                ProtocolError::InvalidArgs {
                    tool: TOOL_SUMMARY,
                    source,
                }
            })?;
            // Pin SUMMARY's topic_id at both layers — the kind carries it AND
            // we surface it at the outer level so the dispatcher doesn't
            // need to crack open the variant.
            Ok(ParsedToolCall {
                kind: AgentMessageKind::Summary {
                    topic_id: args.topic_id.clone(),
                    summary: args.summary,
                    key_decisions: args.key_decisions,
                },
                topic_id: Some(args.topic_id),
                new_topic_title: None,
            })
        }
        other => {
            // Still try to parse so callers can log the raw payload.
            let _ = parse("unknown", raw_args);
            Err(ProtocolError::UnknownTool(other.to_string()))
        }
    }
}

// ---------- Tool schemas (advertised to the LLM) ----------

fn topic_id_property() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "Topic this message belongs to. Reuse the current topic_id when continuing a discussion; pick a new id like 't-2', 't-3' only when starting a meaningfully different subject (and provide new_topic_title)."
    })
}

fn new_topic_title_property() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "Required ONLY when you set topic_id to a fresh id this turn. A short human-readable title for the new topic, e.g. 'Frontend stack pick'."
    })
}

/// Build the full set of tools an Agent can emit message-side. Sprint 2.7
/// will add recall_topic / search_topic alongside these.
pub fn message_tools() -> Vec<Tool> {
    vec![
        Tool::function(
            TOOL_BROADCAST,
            "Speak to the entire team. Use to announce, kick off a topic, or share status. Pickup is optional for recipients.",
            json!({
                "type": "object",
                "properties": {
                    "content": { "type": "string" },
                    "topic_id": topic_id_property(),
                    "new_topic_title": new_topic_title_property(),
                },
                "required": ["content", "topic_id"]
            }),
        ),
        Tool::function(
            TOOL_ASK_AGENT,
            "Direct a question at ONE specific teammate. They MUST answer. Use when you need someone's specific input — not for vague broadcasts.",
            json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "Recipient role id (e.g. 'frontend_dev')." },
                    "content": { "type": "string" },
                    "topic_id": topic_id_property(),
                    "new_topic_title": new_topic_title_property(),
                    "expected_format": { "type": "string", "description": "Optional hint about the response shape." }
                },
                "required": ["to", "content", "topic_id"]
            }),
        ),
        Tool::function(
            TOOL_ANSWER,
            "Respond to an ASK_AGENT directed at you. The reply_to MUST be that ASK_AGENT message's id.",
            json!({
                "type": "object",
                "properties": {
                    "reply_to": { "type": "string", "description": "The ASK_AGENT message id you are answering." },
                    "content": { "type": "string" },
                    "topic_id": topic_id_property(),
                },
                "required": ["reply_to", "content"]
            }),
        ),
        Tool::function(
            TOOL_WORK_START,
            "Announce you're beginning a substantial task. Transitions you into WORKING; BROADCASTs will no longer interrupt until you DONE.",
            json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "One-line description of what you are starting." },
                    "topic_id": topic_id_property(),
                },
                "required": ["task", "topic_id"]
            }),
        ),
        Tool::function(
            TOOL_PROGRESS,
            "Lightweight status update while WORKING. Doesn't change state. Use sparingly — only when there's meaningful new info.",
            json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string" },
                    "percent": { "type": "integer", "minimum": 0, "maximum": 100 },
                    "note": { "type": "string" },
                    "topic_id": topic_id_property(),
                },
                "required": ["task", "percent", "topic_id"]
            }),
        ),
        Tool::function(
            TOOL_DONE,
            "Announce a task is complete. Releases you from WORKING. Use ONLY when you've actually delivered.",
            json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string" },
                    "artifact_id": { "type": "string" },
                    "topic_id": topic_id_property(),
                },
                "required": ["summary", "topic_id"]
            }),
        ),
        Tool::function(
            TOOL_SUMMARY,
            "Close the topic with conclusions. Use when the topic's question(s) are resolved and the team should move on.",
            json!({
                "type": "object",
                "properties": {
                    "topic_id": { "type": "string", "description": "The topic being closed." },
                    "summary": { "type": "string" },
                    "key_decisions": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Concrete decisions made during this topic."
                    }
                },
                "required": ["topic_id", "summary"]
            }),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_broadcast() {
        let parsed = parse_tool_call(
            TOOL_BROADCAST,
            r#"{"content":"hi team","topic_id":"t-1","new_topic_title":"Kickoff"}"#,
        )
        .unwrap();
        match parsed.kind {
            AgentMessageKind::Broadcast { content } => assert_eq!(content, "hi team"),
            _ => panic!("expected Broadcast"),
        }
        assert_eq!(parsed.topic_id.as_deref(), Some("t-1"));
        assert_eq!(parsed.new_topic_title.as_deref(), Some("Kickoff"));
    }

    #[test]
    fn parses_ask_agent() {
        let parsed = parse_tool_call(
            TOOL_ASK_AGENT,
            r#"{"to":"frontend_dev","content":"can you?","topic_id":"t-2"}"#,
        )
        .unwrap();
        match parsed.kind {
            AgentMessageKind::AskAgent { to, content, .. } => {
                assert_eq!(to, "frontend_dev");
                assert_eq!(content, "can you?");
            }
            _ => panic!("expected AskAgent"),
        }
    }

    #[test]
    fn parses_summary_double_pins_topic_id() {
        let parsed = parse_tool_call(
            TOOL_SUMMARY,
            r#"{"topic_id":"t-3","summary":"done","key_decisions":["a","b"]}"#,
        )
        .unwrap();
        match parsed.kind {
            AgentMessageKind::Summary {
                topic_id,
                key_decisions,
                ..
            } => {
                assert_eq!(topic_id, "t-3");
                assert_eq!(key_decisions, vec!["a", "b"]);
            }
            _ => panic!("expected Summary"),
        }
        assert_eq!(parsed.topic_id.as_deref(), Some("t-3"));
    }

    #[test]
    fn unknown_tool_errors() {
        let err = parse_tool_call("DELETE_DATABASE", "{}").unwrap_err();
        match err {
            ProtocolError::UnknownTool(name) => assert_eq!(name, "DELETE_DATABASE"),
            _ => panic!("expected UnknownTool"),
        }
    }

    #[test]
    fn missing_required_field_errors() {
        // Missing `content` in BROADCAST
        let err = parse_tool_call(TOOL_BROADCAST, r#"{"topic_id":"t-1"}"#).unwrap_err();
        match err {
            ProtocolError::InvalidArgs { tool, .. } => assert_eq!(tool, TOOL_BROADCAST),
            _ => panic!("expected InvalidArgs"),
        }
    }

    #[test]
    fn percent_is_clamped_to_100() {
        let parsed = parse_tool_call(
            TOOL_PROGRESS,
            r#"{"task":"x","percent":200,"topic_id":"t-1"}"#,
        )
        .unwrap();
        match parsed.kind {
            AgentMessageKind::Progress { percent, .. } => assert_eq!(percent, 100),
            _ => panic!("expected Progress"),
        }
    }

    #[test]
    fn message_tools_returns_seven() {
        let tools = message_tools();
        assert_eq!(tools.len(), 7);
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&TOOL_BROADCAST));
        assert!(names.contains(&TOOL_ASK_AGENT));
        assert!(names.contains(&TOOL_SUMMARY));
    }
}
