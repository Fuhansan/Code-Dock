//! Level 0 LLM context builder.
//!
//! Per AIDOCK_DESIGN.md §6.3 the runtime gives each agent three layers of
//! history each turn, in order of decreasing detail:
//!
//!   - **System layer**: role prompt + closed-topic index (titles, summaries,
//!     decisions) + cross-topic @-mentions the agent received recently.
//!     This is the "table of contents the model sees automatically."
//!   - **Current topic full stream**: every message in the topic that
//!     triggered this turn, in order, so the model has the live context for
//!     the message it must respond to.
//!   - **Triggering pointer**: a small trailing prompt highlighting which
//!     message is "now" and reminding the model of the tool-call discipline.
//!
//! Sprint 2.7 adds `recall_topic` / `search_topic` tools so the model can
//! pull closed-topic detail on demand — Level 0 only hands out summaries to
//! keep prompts small.
//!
//! Token budget is enforced loosely in V0.1: the current-topic stream is
//! capped at the most recent `MAX_CURRENT_TOPIC_MESSAGES` messages. Real
//! per-token accounting waits for Sprint 3.

use std::collections::{HashMap, HashSet};

use crate::agent::message::{AgentMessage, AgentMessageKind, TopicId};
use crate::agent::role::RoleConfig;
use crate::agent::scratchpad::{render_for_prompt as render_scratchpad, Scratchpad};
use crate::llm::ChatMessage;

/// Most recent N messages from the current topic to inline. V0.1's
/// conversations are short — far below this — so this is a safety cap, not
/// a regular truncation point.
const MAX_CURRENT_TOPIC_MESSAGES: usize = 50;

/// Max cross-topic @-mentions to drop into the system prompt. Keeps the
/// "watch out, someone pinged you over there" awareness from drowning the
/// real work.
const MAX_CROSS_TOPIC_MENTIONS: usize = 5;

/// Build the LLM input for one agent turn.
///
/// `history` is the agent's local accumulator (everything that flowed
/// through its inbox plus its own emits, in arrival order). `triggering`
/// is the message that just caused the agent to think — it pins the current
/// topic and provides the "respond to this" pointer at the very end.
pub fn build_level_0_context(
    role: &RoleConfig,
    history: &[AgentMessage],
    triggering: &AgentMessage,
    scratchpad: &Scratchpad,
) -> Vec<ChatMessage> {
    let current_topic = triggering.topic_id.clone();

    // ---- Bucket history by topic and find closed ones ----

    let mut by_topic: HashMap<TopicId, Vec<&AgentMessage>> = HashMap::new();
    let mut closed_topics: HashSet<TopicId> = HashSet::new();
    let mut topic_summaries: HashMap<TopicId, (String, Vec<String>)> = HashMap::new();
    let mut topic_first_seen_order: Vec<TopicId> = Vec::new();

    for m in history {
        if !by_topic.contains_key(&m.topic_id) {
            topic_first_seen_order.push(m.topic_id.clone());
        }
        by_topic.entry(m.topic_id.clone()).or_default().push(m);

        if let AgentMessageKind::Summary {
            topic_id,
            summary,
            key_decisions,
        } = &m.kind
        {
            closed_topics.insert(topic_id.clone());
            topic_summaries.insert(topic_id.clone(), (summary.clone(), key_decisions.clone()));
        }
    }

    // ---- System layer ----

    // persona（用户可编）+ 团队块 + 协作协议铁律（后两者注入、不可编）。
    let composed = crate::agent::roles::compose_system_prompt(role);
    let mut system = String::with_capacity(composed.len() + 1024);
    system.push_str(&composed);

    // Scratchpad (Sprint 2.8) — pinned immediately under the role prompt so
    // the agent's own breadcrumbs are as prominent as its identity.
    let scratchpad_section = render_scratchpad(scratchpad);
    if !scratchpad_section.is_empty() {
        system.push_str("\n\n---\n");
        system.push_str(&scratchpad_section);
    }

    let closed_section = format_closed_topics(
        &topic_first_seen_order,
        &closed_topics,
        &topic_summaries,
        &by_topic,
        &current_topic,
    );
    if !closed_section.is_empty() {
        system.push_str("\n\n---\n");
        system.push_str(&closed_section);
    }

    let mentions_section = format_cross_topic_mentions(role, history, &current_topic);
    if !mentions_section.is_empty() {
        system.push_str("\n\n---\n");
        system.push_str(&mentions_section);
    }

    // ---- Current-topic conversation ----

    let mut current_msgs: Vec<&AgentMessage> = by_topic
        .get(&current_topic)
        .cloned()
        .unwrap_or_default();
    // Cap to the tail of the topic if it ever runs unusually long.
    if current_msgs.len() > MAX_CURRENT_TOPIC_MESSAGES {
        let drop = current_msgs.len() - MAX_CURRENT_TOPIC_MESSAGES;
        current_msgs.drain(0..drop);
    }

    let mut user = String::with_capacity(current_msgs.len() * 96);
    user.push_str(&format!("## Current topic: {}\n", current_topic));
    if current_msgs.is_empty() {
        user.push_str("(this topic is new — the triggering message below is its first.)\n");
    } else {
        user.push_str("Messages in this topic so far:\n");
        for m in &current_msgs {
            user.push_str(&format_message_line(m));
            user.push('\n');
        }
    }
    user.push_str("\n----\n");
    user.push_str(&format!(
        "The latest message you must respond to is [{}] from {} as {}.\n",
        triggering.id,
        triggering.sender,
        triggering.kind.tag()
    ));
    user.push_str(&format!("Current topic_id: {}\n", current_topic));
    user.push_str("Respond by calling exactly one tool, per the house rules.\n");

    vec![
        ChatMessage::System { content: system },
        ChatMessage::User { content: user },
    ]
}

// ---------- helpers ----------

/// Single-line projection of one message for the prompt.
fn format_message_line(m: &AgentMessage) -> String {
    format!(
        "- [{}] ({}) {}: {}",
        m.id,
        m.sender,
        m.kind.tag(),
        m.kind.payload_preview()
    )
}

fn format_closed_topics(
    topic_first_seen_order: &[TopicId],
    closed_topics: &HashSet<TopicId>,
    summaries: &HashMap<TopicId, (String, Vec<String>)>,
    by_topic: &HashMap<TopicId, Vec<&AgentMessage>>,
    current_topic: &TopicId,
) -> String {
    let closed_in_order: Vec<&TopicId> = topic_first_seen_order
        .iter()
        .filter(|t| closed_topics.contains(*t) && *t != current_topic)
        .collect();

    if closed_in_order.is_empty() {
        return String::new();
    }

    let mut s = String::from("## Closed topics so far\n");
    s.push_str(
        "Each entry is a topic that already finished with a SUMMARY. Call `recall_topic(topic_id)` if you need full message detail.\n\n",
    );
    for tid in closed_in_order {
        let title = derive_topic_title(by_topic.get(tid).map(|v| v.as_slice()).unwrap_or(&[]));
        let default = (String::new(), Vec::new());
        let (summary, decisions) = summaries.get(tid).unwrap_or(&default);
        s.push_str(&format!("- [{}] {}", tid, title));
        if !summary.is_empty() {
            s.push_str(": ");
            s.push_str(summary);
        }
        s.push('\n');
        for d in decisions {
            s.push_str(&format!("    · {}\n", d));
        }
    }
    s
}

/// Cheap title heuristic: first BROADCAST/ASK/USER_INPUT in the topic, first
/// ~60 chars of its payload. Sprint 2.6 stores real titles when topics open
/// — this is the V0.1 fallback for anything that didn't get one.
fn derive_topic_title(msgs: &[&AgentMessage]) -> String {
    for m in msgs {
        let preview = match &m.kind {
            AgentMessageKind::UserInput { content }
            | AgentMessageKind::Broadcast { content } => content.clone(),
            AgentMessageKind::AskAgent { content, .. } => content.clone(),
            _ => continue,
        };
        return preview.chars().take(60).collect();
    }
    "(no title)".into()
}

fn format_cross_topic_mentions(
    role: &RoleConfig,
    history: &[AgentMessage],
    current_topic: &TopicId,
) -> String {
    // Walk backwards so we collect the MOST RECENT mentions first.
    let mut hits: Vec<&AgentMessage> = Vec::new();
    for m in history.iter().rev() {
        if &m.topic_id == current_topic {
            continue;
        }
        if message_mentions_me(&role.id, m) {
            hits.push(m);
            if hits.len() >= MAX_CROSS_TOPIC_MENTIONS {
                break;
            }
        }
    }
    if hits.is_empty() {
        return String::new();
    }
    let mut s = String::from("## Recent cross-topic mentions of you\n");
    // Render in chronological order (oldest first).
    for m in hits.iter().rev() {
        s.push_str(&format!(
            "- [{}] in {} from {}: {}\n",
            m.id,
            m.topic_id,
            m.sender,
            m.kind.payload_preview()
        ));
    }
    s
}

/// True iff this message addresses or names the agent. ASK_AGENT.to is an
/// exact match; anything else needs the role id to appear in the textual
/// payload.
fn message_mentions_me(role_id: &str, m: &AgentMessage) -> bool {
    if let AgentMessageKind::AskAgent { to, .. } = &m.kind {
        if to == role_id {
            return true;
        }
    }
    m.kind.payload_preview().contains(role_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::roles::{pm_role, FRONTEND_ID, PM_ID};
    use crate::agent::scratchpad::apply_update;

    fn empty_pad() -> Scratchpad {
        Scratchpad::default()
    }

    fn make(id: &str, sender: &str, topic: &str, kind: AgentMessageKind) -> AgentMessage {
        AgentMessage::new(id, sender, topic, 0, kind)
    }

    fn user_msg(id: &str, content: &str) -> AgentMessage {
        make(
            id,
            "user",
            "t-default",
            AgentMessageKind::UserInput { content: content.into() },
        )
    }

    #[test]
    fn empty_history_gives_system_plus_user() {
        let trig = user_msg("m-1", "build a todo app");
        let chat = build_level_0_context(&pm_role(), &[], &trig, &empty_pad());
        assert_eq!(chat.len(), 2);
        match &chat[0] {
            ChatMessage::System { content } => assert!(content.contains(PM_ID)),
            _ => panic!("first must be system"),
        }
        match &chat[1] {
            ChatMessage::User { content } => {
                assert!(content.contains("t-default"));
                assert!(content.contains("m-1"));
                assert!(content.contains("exactly one tool"));
            }
            _ => panic!("second must be user"),
        }
    }

    #[test]
    fn closed_topic_appears_in_system_with_summary_and_decisions() {
        let trig = user_msg("m-now", "what's next");
        let history = vec![
            make(
                "m-old-1",
                PM_ID,
                "t-old",
                AgentMessageKind::Broadcast {
                    content: "Pick frontend stack".into(),
                },
            ),
            make(
                "m-old-sum",
                PM_ID,
                "t-old",
                AgentMessageKind::Summary {
                    topic_id: "t-old".into(),
                    summary: "Settled on plain HTML".into(),
                    key_decisions: vec!["no React".into(), "vanilla JS".into()],
                },
            ),
        ];
        let chat = build_level_0_context(&pm_role(), &history, &trig, &empty_pad());
        let ChatMessage::System { content } = &chat[0] else {
            panic!()
        };
        assert!(content.contains("Closed topics"));
        assert!(content.contains("t-old"));
        assert!(content.contains("Settled on plain HTML"));
        assert!(content.contains("no React"));
        assert!(content.contains("vanilla JS"));
    }

    #[test]
    fn current_topic_is_NOT_listed_as_closed() {
        let trig = user_msg("m-now", "continue");
        let history = vec![make(
            "m-1",
            PM_ID,
            "t-default",
            AgentMessageKind::Summary {
                topic_id: "t-default".into(),
                summary: "x".into(),
                key_decisions: vec![],
            },
        )];
        let chat = build_level_0_context(&pm_role(), &history, &trig, &empty_pad());
        let ChatMessage::System { content } = &chat[0] else {
            panic!()
        };
        assert!(!content.contains("Closed topics"));
    }

    #[test]
    fn cross_topic_mention_surfaces_in_system() {
        let trig = make(
            "m-now",
            "user",
            "t-current",
            AgentMessageKind::UserInput { content: "hi".into() },
        );
        let history = vec![make(
            "m-prior",
            PM_ID,
            "t-prior",
            AgentMessageKind::Broadcast {
                content: format!("Heads up to {FRONTEND_ID}, watch the dependencies"),
            },
        )];
        let role = crate::agent::roles::frontend_role();
        let chat = build_level_0_context(&role, &history, &trig, &empty_pad());
        let ChatMessage::System { content } = &chat[0] else {
            panic!()
        };
        assert!(content.contains("cross-topic mentions"));
        assert!(content.contains("t-prior"));
        assert!(content.contains("watch the dependencies"));
    }

    #[test]
    fn current_topic_message_stream_lists_each_message_once() {
        let trig = make(
            "m-3",
            PM_ID,
            "t-1",
            AgentMessageKind::Broadcast {
                content: "ok let's go".into(),
            },
        );
        let history = vec![
            make(
                "m-1",
                "user",
                "t-1",
                AgentMessageKind::UserInput { content: "build x".into() },
            ),
            make(
                "m-2",
                FRONTEND_ID,
                "t-1",
                AgentMessageKind::Broadcast { content: "ack".into() },
            ),
            make(
                "m-3",
                PM_ID,
                "t-1",
                AgentMessageKind::Broadcast {
                    content: "ok let's go".into(),
                },
            ),
        ];
        let chat = build_level_0_context(&pm_role(), &history, &trig, &empty_pad());
        let ChatMessage::User { content } = &chat[1] else {
            panic!()
        };
        for id in ["m-1", "m-2"] {
            assert_eq!(content.matches(id).count(), 1, "id {id} should appear once");
        }
        assert_eq!(content.matches("m-3").count(), 2);
    }

    #[test]
    fn answer_in_current_topic_uses_reply_to_in_preview() {
        let trig = make(
            "m-2",
            FRONTEND_ID,
            "t-1",
            AgentMessageKind::Answer {
                reply_to: "m-1".into(),
                content: "looks good".into(),
            },
        );
        let history = vec![
            make(
                "m-1",
                PM_ID,
                "t-1",
                AgentMessageKind::AskAgent {
                    to: FRONTEND_ID.into(),
                    content: "approve?".into(),
                    expected_format: None,
                },
            ),
            make(
                "m-2",
                FRONTEND_ID,
                "t-1",
                AgentMessageKind::Answer {
                    reply_to: "m-1".into(),
                    content: "looks good".into(),
                },
            ),
        ];
        let chat = build_level_0_context(&pm_role(), &history, &trig, &empty_pad());
        let ChatMessage::User { content } = &chat[1] else {
            panic!()
        };
        assert!(content.contains("(re m-1)"));
    }

    #[test]
    fn current_topic_caps_at_max_messages() {
        let trig = make(
            "m-extra",
            "user",
            "t-1",
            AgentMessageKind::UserInput { content: "go".into() },
        );
        let mut history = Vec::new();
        for i in 0..(MAX_CURRENT_TOPIC_MESSAGES * 2) {
            history.push(make(
                &format!("m-{i}"),
                PM_ID,
                "t-1",
                AgentMessageKind::Broadcast {
                    content: format!("msg #{i}"),
                },
            ));
        }
        let chat = build_level_0_context(&pm_role(), &history, &trig, &empty_pad());
        let ChatMessage::User { content } = &chat[1] else {
            panic!()
        };
        assert!(!content.contains("m-0 "));
        let newest = MAX_CURRENT_TOPIC_MESSAGES * 2 - 1;
        assert!(content.contains(&format!("m-{newest}")));
    }

    #[test]
    fn scratchpad_renders_into_system_prompt() {
        // Sprint 2.8: when the agent has anything in its scratchpad, that
        // content must appear in the system prompt so the agent sees its
        // own breadcrumbs every turn.
        let trig = user_msg("m-1", "continue work");
        let mut pad = Scratchpad::default();
        apply_update(
            &mut pad,
            r#"{"current_focus":"login page","add_decisions":["use JWT in localStorage"]}"#,
        )
        .unwrap();
        let chat = build_level_0_context(&pm_role(), &[], &trig, &pad);
        let ChatMessage::System { content } = &chat[0] else {
            panic!()
        };
        assert!(content.contains("Your scratchpad"));
        assert!(content.contains("Current focus: login page"));
        assert!(content.contains("use JWT in localStorage"));
        // Role identity still present — scratchpad augments, doesn't replace.
        assert!(content.contains(PM_ID));
    }

    #[test]
    fn empty_scratchpad_omits_section_entirely() {
        let trig = user_msg("m-1", "go");
        let chat = build_level_0_context(&pm_role(), &[], &trig, &empty_pad());
        let ChatMessage::System { content } = &chat[0] else {
            panic!()
        };
        assert!(!content.contains("Your scratchpad"));
    }
}
