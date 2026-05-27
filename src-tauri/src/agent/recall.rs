//! Query tools the agent can invoke to dig into history without emitting
//! a message — `recall_topic` and `search_topic` (AIDOCK_DESIGN.md §6.3
//! Level 1).
//!
//! Level 0 context only inlines closed-topic *summaries*. When an agent
//! decides it actually needs the underlying message detail (e.g. "what
//! exactly did backend_dev propose about auth?"), it calls one of these
//! tools and the runtime feeds the result back as an OpenAI `tool` role
//! message, then re-asks the model. That second turn is where the agent's
//! real BROADCAST / ASK_AGENT / etc. lands.
//!
//! V0.1 scope: queries operate on the **agent's own local history** (every
//! message that flowed through its inbox + every message it emitted). They
//! do not reach into the dispatcher's global topic map — ASK_AGENT traffic
//! between two other roles is therefore invisible. Sprint 3+ will add a
//! dispatcher-side recall channel.

use serde_json::{json, Value};

use crate::agent::message::AgentMessage;
use crate::llm::Tool;

pub const TOOL_RECALL_TOPIC: &str = "recall_topic";
pub const TOOL_SEARCH_TOPIC: &str = "search_topic";

/// True iff this tool name is a query (look up, don't emit).
pub fn is_query_tool(name: &str) -> bool {
    matches!(name, TOOL_RECALL_TOPIC | TOOL_SEARCH_TOPIC)
}

/// The tool definitions to advertise to the model alongside the action
/// tools from `protocol::message_tools`. Keep these descriptions tight —
/// they shape when the model decides to dig vs. act.
pub fn query_tools() -> Vec<Tool> {
    vec![
        Tool::function(
            TOOL_RECALL_TOPIC,
            "Pull the full message stream of a topic you only see summarised in your context. Use when you need the actual back-and-forth detail from a closed topic before deciding next steps. Result is delivered back to you; you still need to emit a separate action tool afterwards.",
            json!({
                "type": "object",
                "properties": {
                    "topic_id": {
                        "type": "string",
                        "description": "The topic_id whose messages you want to read."
                    }
                },
                "required": ["topic_id"]
            }),
        ),
        Tool::function(
            TOOL_SEARCH_TOPIC,
            "Search the message stream of one topic for a substring (case-insensitive). Result is a list of matching message previews. Useful when the topic is long and you only care about one decision or API field.",
            json!({
                "type": "object",
                "properties": {
                    "topic_id": { "type": "string" },
                    "query": {
                        "type": "string",
                        "description": "Substring to match against message payload text."
                    }
                },
                "required": ["topic_id", "query"]
            }),
        ),
    ]
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("invalid args for {tool}: {source}")]
    InvalidArgs {
        tool: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("missing required field '{field}' in args for {tool}")]
    MissingField {
        tool: &'static str,
        field: &'static str,
    },
    #[error("unknown query tool '{0}'")]
    UnknownTool(String),
}

/// Execute one query tool call against `history`. Returns the string body to
/// feed back to the model as the `tool` role content.
pub fn execute_query_tool(
    name: &str,
    raw_args: &str,
    history: &[AgentMessage],
) -> Result<String, QueryError> {
    let v: Value = serde_json::from_str(raw_args).map_err(|source| QueryError::InvalidArgs {
        tool: match name {
            TOOL_RECALL_TOPIC => TOOL_RECALL_TOPIC,
            TOOL_SEARCH_TOPIC => TOOL_SEARCH_TOPIC,
            _ => "unknown",
        },
        source,
    })?;

    match name {
        TOOL_RECALL_TOPIC => {
            let topic_id = v
                .get("topic_id")
                .and_then(Value::as_str)
                .ok_or(QueryError::MissingField {
                    tool: TOOL_RECALL_TOPIC,
                    field: "topic_id",
                })?;
            Ok(recall_topic(history, topic_id))
        }
        TOOL_SEARCH_TOPIC => {
            let topic_id = v
                .get("topic_id")
                .and_then(Value::as_str)
                .ok_or(QueryError::MissingField {
                    tool: TOOL_SEARCH_TOPIC,
                    field: "topic_id",
                })?;
            let query = v
                .get("query")
                .and_then(Value::as_str)
                .ok_or(QueryError::MissingField {
                    tool: TOOL_SEARCH_TOPIC,
                    field: "query",
                })?;
            Ok(search_topic(history, topic_id, query))
        }
        other => Err(QueryError::UnknownTool(other.to_string())),
    }
}

fn recall_topic(history: &[AgentMessage], topic_id: &str) -> String {
    let msgs: Vec<&AgentMessage> = history.iter().filter(|m| m.topic_id == topic_id).collect();
    if msgs.is_empty() {
        return format!(
            "No messages in topic '{}' are visible to you. (You may not have been a participant — ASK_AGENT chains between other roles aren't routed through your inbox.)",
            topic_id
        );
    }
    let mut s = format!(
        "Topic '{}' — {} message{} in your visible history.\n\n",
        topic_id,
        msgs.len(),
        if msgs.len() == 1 { "" } else { "s" }
    );
    for m in msgs {
        s.push_str(&format!(
            "- [{}] ({}) {}: {}\n",
            m.id,
            m.sender,
            m.kind.tag(),
            m.kind.payload_preview()
        ));
    }
    s
}

fn search_topic(history: &[AgentMessage], topic_id: &str, query: &str) -> String {
    let q = query.to_lowercase();
    let hits: Vec<&AgentMessage> = history
        .iter()
        .filter(|m| m.topic_id == topic_id)
        .filter(|m| m.kind.payload_preview().to_lowercase().contains(&q))
        .collect();

    if hits.is_empty() {
        return format!(
            "No matches for '{}' in topic '{}' (within your visible history).",
            query, topic_id
        );
    }
    let mut s = format!(
        "{} match{} for '{}' in topic '{}'.\n\n",
        hits.len(),
        if hits.len() == 1 { "" } else { "es" },
        query,
        topic_id
    );
    for m in hits {
        s.push_str(&format!(
            "- [{}] ({}) {}: {}\n",
            m.id,
            m.sender,
            m.kind.tag(),
            m.kind.payload_preview()
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::message::AgentMessageKind;

    fn make(id: &str, sender: &str, topic: &str, kind: AgentMessageKind) -> AgentMessage {
        AgentMessage::new(id, sender, topic, 0, kind)
    }

    #[test]
    fn query_tool_set_has_two_names() {
        let tools = query_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&TOOL_RECALL_TOPIC));
        assert!(names.contains(&TOOL_SEARCH_TOPIC));
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn is_query_tool_distinguishes() {
        assert!(is_query_tool(TOOL_RECALL_TOPIC));
        assert!(is_query_tool(TOOL_SEARCH_TOPIC));
        assert!(!is_query_tool("BROADCAST"));
        assert!(!is_query_tool("anything-else"));
    }

    #[test]
    fn recall_topic_returns_all_messages_in_topic() {
        let history = vec![
            make(
                "m-1",
                "PM",
                "t-1",
                AgentMessageKind::Broadcast { content: "kick off".into() },
            ),
            make(
                "m-2",
                "frontend_dev",
                "t-1",
                AgentMessageKind::Broadcast { content: "ack".into() },
            ),
            make(
                "m-3",
                "PM",
                "t-2",
                AgentMessageKind::Broadcast { content: "different topic".into() },
            ),
        ];
        let result = execute_query_tool(TOOL_RECALL_TOPIC, r#"{"topic_id":"t-1"}"#, &history).unwrap();
        assert!(result.contains("m-1"));
        assert!(result.contains("m-2"));
        assert!(!result.contains("m-3"), "must not bleed across topics");
        assert!(result.contains("2 messages"));
    }

    #[test]
    fn recall_topic_empty_explains_visibility() {
        let result = execute_query_tool(
            TOOL_RECALL_TOPIC,
            r#"{"topic_id":"t-unseen"}"#,
            &[],
        )
        .unwrap();
        assert!(result.contains("t-unseen"));
        assert!(result.contains("visible to you") || result.contains("not been a participant"));
    }

    #[test]
    fn search_topic_returns_only_matching_messages() {
        let history = vec![
            make(
                "m-1",
                "PM",
                "t-1",
                AgentMessageKind::Broadcast { content: "let's use JWT".into() },
            ),
            make(
                "m-2",
                "backend_dev",
                "t-1",
                AgentMessageKind::Broadcast { content: "Express server".into() },
            ),
            make(
                "m-3",
                "frontend_dev",
                "t-1",
                AgentMessageKind::Broadcast { content: "JWT in localStorage".into() },
            ),
        ];
        let result = execute_query_tool(
            TOOL_SEARCH_TOPIC,
            r#"{"topic_id":"t-1","query":"jwt"}"#,
            &history,
        )
        .unwrap();
        assert!(result.contains("m-1"));
        assert!(result.contains("m-3"));
        assert!(!result.contains("Express"), "non-matching message must be excluded");
        assert!(result.contains("2 matches"));
    }

    #[test]
    fn search_is_case_insensitive() {
        let history = vec![make(
            "m-1",
            "PM",
            "t-1",
            AgentMessageKind::Broadcast { content: "Use JWT".into() },
        )];
        let result = execute_query_tool(
            TOOL_SEARCH_TOPIC,
            r#"{"topic_id":"t-1","query":"jwt"}"#,
            &history,
        )
        .unwrap();
        assert!(result.contains("m-1"));
    }

    #[test]
    fn missing_field_errors() {
        let err = execute_query_tool(TOOL_RECALL_TOPIC, r#"{}"#, &[]).unwrap_err();
        match err {
            QueryError::MissingField { tool, field } => {
                assert_eq!(tool, TOOL_RECALL_TOPIC);
                assert_eq!(field, "topic_id");
            }
            other => panic!("expected MissingField, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tool_errors() {
        let err = execute_query_tool("unrelated", r#"{}"#, &[]).unwrap_err();
        match err {
            QueryError::UnknownTool(s) => assert_eq!(s, "unrelated"),
            other => panic!("expected UnknownTool, got {other:?}"),
        }
    }
}
