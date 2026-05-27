//! Central message router for the multi-agent group chat.
//!
//! Every message — user input or Agent output — flows through here. The
//! dispatcher's only job is to glue four things together:
//!
//! 1. **Routing**: ASK_AGENT to the named recipient's inbox; everything else
//!    fan-outs to every Agent except the sender. Agents self-filter via
//!    their state machine.
//! 2. **Persistence**: append the wire form to `messages.jsonl` so the
//!    session can be reconstructed (Sprint 3 makes that recovery robust).
//! 3. **Topic bookkeeping**: open new topics declared via `new_topic_title`,
//!    record participants, close topics on SUMMARY.
//! 4. **UI event emission**: every processed message becomes an
//!    `aidock:message` Tauri event so the chat panel updates in real time.
//!
//! The dispatcher runs as one tokio task. Agents and the UI push into a
//! shared `submit` channel; the dispatcher reads from it serially. That
//! serialisation is the dispatcher's only synchronisation — there is no
//! shared mutable state with the Agent tasks.

use std::collections::HashMap;
use std::path::PathBuf;

use tauri::Emitter;
use tokio::fs::{File, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use crate::agent::message::{AgentId, AgentMessage, AgentMessageKind, TopicId};
use crate::agent::topic::{Topic, TopicStatus};

/// Derive a fallback topic title from the first message in a topic, when the
/// sender forgot to provide `opens_topic_title`. Takes a short prefix of the
/// payload that's appropriate as a heading.
fn derive_title_from_first_message(msg: &AgentMessage) -> Option<String> {
    const MAX: usize = 60;
    let raw: &str = match &msg.kind {
        AgentMessageKind::UserInput { content } => content,
        AgentMessageKind::Broadcast { content } => content,
        AgentMessageKind::AskAgent { content, .. } => content,
        AgentMessageKind::WorkStart { task } => task,
        // SUMMARY / DONE / PROGRESS / ANSWER aren't first-message material.
        _ => return None,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let cleaned = trimmed.lines().next().unwrap_or(trimmed);
    Some(cleaned.chars().take(MAX).collect())
}

/// Apply one message to a topic map: open the topic on first sight (with
/// title priority opens_topic_title → first-payload prefix → topic_id),
/// add the sender as a participant, and close on SUMMARY.
///
/// Used by both the live `update_topic` path and the Sprint 3 reload path
/// in `Dispatcher::new`, so reload and live state stay in lockstep.
pub(crate) fn apply_message_to_topics(
    topics: &mut HashMap<TopicId, Topic>,
    msg: &AgentMessage,
) {
    let is_new = !topics.contains_key(&msg.topic_id);
    if is_new {
        let title = msg
            .opens_topic_title
            .clone()
            .or_else(|| derive_title_from_first_message(msg))
            .unwrap_or_else(|| msg.topic_id.clone());
        topics.insert(
            msg.topic_id.clone(),
            Topic::open(msg.topic_id.clone(), title, msg.timestamp),
        );
    }
    let topic = topics
        .get_mut(&msg.topic_id)
        .expect("just inserted above if missing");
    topic.add_participant(&msg.sender);

    if let AgentMessageKind::Summary {
        summary,
        key_decisions,
        ..
    } = &msg.kind
    {
        topic.close(summary.clone(), key_decisions.clone(), msg.timestamp);
    }
}

/// Filter a message stream down to messages this agent would have seen
/// via the dispatcher's routing rules. Used by `Session::start` to seed
/// each agent's local history after a restart.
///
/// Rules mirror `Dispatcher::route`:
/// - Own emits: included (so the agent recognises its own past actions
///   and doesn't duplicate ANSWERs — the Sprint 2.4 bug).
/// - ASK_AGENT { to }: only included for the named target.
/// - Everything else: included for any agent that isn't the sender.
pub fn filter_visible_to(messages: &[AgentMessage], agent_id: &str) -> Vec<AgentMessage> {
    messages
        .iter()
        .filter(|m| {
            if m.sender == agent_id {
                return true;
            }
            match &m.kind {
                AgentMessageKind::AskAgent { to, .. } => to == agent_id,
                _ => true,
            }
        })
        .cloned()
        .collect()
}

/// Channel capacities chosen for V0.1's expected message rate. Bumping later
/// is harmless — the producers all use `send` (back-pressure aware), not
/// `try_send`.
const SUBMIT_CAPACITY: usize = 256;
const INBOX_CAPACITY: usize = 64;

/// Tauri event name the frontend subscribes to.
pub const MESSAGE_EVENT: &str = "aidock:message";

/// Handle held by callers outside the dispatcher task. Cheap to clone.
#[derive(Clone)]
pub struct DispatcherHandle {
    submit_tx: mpsc::Sender<AgentMessage>,
}

impl DispatcherHandle {
    /// Push a message into the dispatcher. Used by:
    /// - Tauri commands (forwarding user input)
    /// - Agent runtimes (emitting their own output)
    pub async fn submit(&self, msg: AgentMessage) -> Result<(), DispatcherError> {
        self.submit_tx
            .send(msg)
            .await
            .map_err(|_| DispatcherError::DispatcherClosed)
    }

    /// Borrow the underlying sender — useful when wiring an Agent's runtime
    /// that needs to send many messages.
    pub fn sender(&self) -> mpsc::Sender<AgentMessage> {
        self.submit_tx.clone()
    }
}

/// Builder/runner combo. Build phase registers Agents (returns each Agent's
/// inbox receiver), then `start()` consumes self and spawns the routing task.
///
/// `submit_tx` is `Option` so `run()` can drop our keeper-clone (releasing
/// `None`) and let the channel close once every external `DispatcherHandle`
/// clone goes out of scope. Field-level partial move would block reborrowing
/// `self` for `handle_message`.
///
/// `app_handle` is `Option` so dev/test harnesses (e.g. the multi-agent
/// example) can run without Tauri. When `None`, message events are written
/// to messages.jsonl and tracing but no IPC event is emitted.
pub struct Dispatcher {
    inboxes: HashMap<AgentId, mpsc::Sender<AgentMessage>>,
    submit_tx: Option<mpsc::Sender<AgentMessage>>,
    submit_rx: mpsc::Receiver<AgentMessage>,
    messages_file: File,
    topics: HashMap<TopicId, Topic>,
    /// Messages loaded from a previous run's `messages.jsonl` on startup.
    /// Sprint 3: `Session::start` reads this to seed each agent's local
    /// history + initial state. Cleared via `take_loaded_messages()` after
    /// distribution so the dispatcher doesn't hold a duplicate copy.
    loaded_messages: Vec<AgentMessage>,
    app_handle: Option<tauri::AppHandle>,
}

#[derive(Debug, thiserror::Error)]
pub enum DispatcherError {
    #[error("dispatcher is closed — submit channel dropped")]
    DispatcherClosed,
    #[error("io: {context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl Dispatcher {
    /// Open (or create) the session directory and prepare a new dispatcher.
    /// `messages.jsonl` is opened in append mode; old contents are preserved.
    /// `app_handle` may be `None` for headless dev harnesses.
    pub async fn new(
        session_dir: PathBuf,
        app_handle: Option<tauri::AppHandle>,
    ) -> Result<Self, DispatcherError> {
        tokio::fs::create_dir_all(&session_dir)
            .await
            .map_err(|source| DispatcherError::Io {
                context: "create session dir",
                source,
            })?;

        let messages_path = session_dir.join("messages.jsonl");

        // Sprint 3: rehydrate previous-session state from messages.jsonl
        // BEFORE opening for append. Malformed tail-lines are skipped with
        // a warning by read_jsonl_messages — we never refuse to start.
        let loaded_messages = crate::agent::persistence::read_jsonl_messages(&messages_path)
            .map_err(|source| DispatcherError::Io {
                context: "load messages.jsonl",
                source: std::io::Error::other(source.to_string()),
            })?;

        let mut topics: HashMap<TopicId, Topic> = HashMap::new();
        for msg in &loaded_messages {
            apply_message_to_topics(&mut topics, msg);
        }

        if !loaded_messages.is_empty() {
            tracing::info!(
                target: "aidock::dispatcher",
                loaded = loaded_messages.len(),
                topics = topics.len(),
                "rehydrated state from messages.jsonl"
            );
        }

        let messages_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&messages_path)
            .await
            .map_err(|source| DispatcherError::Io {
                context: "open messages.jsonl",
                source,
            })?;

        let (submit_tx, submit_rx) = mpsc::channel(SUBMIT_CAPACITY);

        tracing::info!(
            target: "aidock::dispatcher",
            session_dir = %session_dir.display(),
            "dispatcher initialized"
        );

        Ok(Self {
            inboxes: HashMap::new(),
            submit_tx: Some(submit_tx),
            submit_rx,
            messages_file,
            topics,
            loaded_messages,
            app_handle,
        })
    }

    /// Hand the previously-loaded messages to `Session::start` for
    /// distribution to agents. After this call the dispatcher no longer
    /// holds them — they live in each agent's local history.
    pub fn take_loaded_messages(&mut self) -> Vec<AgentMessage> {
        std::mem::take(&mut self.loaded_messages)
    }

    /// Register an Agent. Returns the receiver the Agent's runtime task will
    /// drain. Must be called before `start()` — late joiners aren't supported
    /// in V0.1.
    pub fn register_agent(&mut self, id: AgentId) -> mpsc::Receiver<AgentMessage> {
        let (tx, rx) = mpsc::channel(INBOX_CAPACITY);
        if self.inboxes.insert(id.clone(), tx).is_some() {
            tracing::warn!(target: "aidock::dispatcher", %id, "agent re-registered, old inbox dropped");
        }
        rx
    }

    /// Get a handle for pushing messages in from the outside (Tauri commands,
    /// Agent runtimes). Panics if called after `run()` has consumed `self`.
    pub fn handle(&self) -> DispatcherHandle {
        DispatcherHandle {
            submit_tx: self
                .submit_tx
                .as_ref()
                .expect("Dispatcher::handle called after run()")
                .clone(),
        }
    }

    /// Consume self and run the dispatcher loop until the submit channel
    /// closes (all senders dropped). Intended to be spawned with
    /// `tokio::spawn(dispatcher.run())`.
    pub async fn run(mut self) {
        tracing::info!(target: "aidock::dispatcher", agents = self.inboxes.len(), "dispatcher running");
        // Release our keeper-clone of submit_tx so the loop terminates when
        // every external sender disconnects. Otherwise we'd keep our own
        // reference and the channel would never close.
        self.submit_tx = None;

        while let Some(msg) = self.submit_rx.recv().await {
            if let Err(e) = self.handle_message(msg).await {
                tracing::error!(target: "aidock::dispatcher", error = %e, "message handling failed");
            }
        }

        tracing::info!(target: "aidock::dispatcher", "dispatcher shutting down");
    }

    async fn handle_message(&mut self, msg: AgentMessage) -> Result<(), DispatcherError> {
        tracing::debug!(
            target: "aidock::dispatcher",
            id = %msg.id,
            sender = %msg.sender,
            kind = msg.kind.tag(),
            topic = %msg.topic_id,
            "routing"
        );

        self.persist(&msg).await?;
        self.update_topic(&msg);
        self.route(&msg).await;
        self.emit_event(&msg);

        Ok(())
    }

    async fn persist(&mut self, msg: &AgentMessage) -> Result<(), DispatcherError> {
        let line = serde_json::to_string(msg).map_err(|e| DispatcherError::Io {
            context: "serialize message",
            source: std::io::Error::other(e.to_string()),
        })?;
        self.messages_file
            .write_all(line.as_bytes())
            .await
            .map_err(|source| DispatcherError::Io {
                context: "append message",
                source,
            })?;
        self.messages_file
            .write_all(b"\n")
            .await
            .map_err(|source| DispatcherError::Io {
                context: "append newline",
                source,
            })?;
        self.messages_file
            .flush()
            .await
            .map_err(|source| DispatcherError::Io {
                context: "flush messages.jsonl",
                source,
            })?;
        Ok(())
    }

    /// Mutate `self.topics` based on this message. Topic creation is implicit:
    /// the first time a `topic_id` is seen, we open it. SUMMARY closes it.
    fn update_topic(&mut self, msg: &AgentMessage) {
        apply_message_to_topics(&mut self.topics, msg);
    }

    async fn route(&self, msg: &AgentMessage) {
        match &msg.kind {
            // Directed — deliver only to the named target. Even if the target
            // is currently WORKING they still get it in their inbox; their
            // runtime decides what to do.
            AgentMessageKind::AskAgent { to, .. } => {
                if let Some(inbox) = self.inboxes.get(to) {
                    let _ = inbox.send(msg.clone()).await;
                } else {
                    tracing::warn!(
                        target: "aidock::dispatcher",
                        to = %to,
                        msg_id = %msg.id,
                        "ASK_AGENT target unknown — dropped"
                    );
                }
            }
            // Everything else fan-outs to every Agent except the sender.
            // Agents filter by state + relevance.
            _ => {
                for (id, inbox) in &self.inboxes {
                    if id != &msg.sender {
                        let _ = inbox.send(msg.clone()).await;
                    }
                }
            }
        }
    }

    fn emit_event(&self, msg: &AgentMessage) {
        let Some(handle) = self.app_handle.as_ref() else {
            return; // headless mode — JSONL + tracing are the only sinks
        };
        if let Err(e) = handle.emit(MESSAGE_EVENT, msg) {
            tracing::warn!(target: "aidock::dispatcher", error = %e, "failed to emit message event");
        }
    }

    /// Read-only snapshot of the topic map. Intended for queries from
    /// commands; the dispatcher does NOT expose the mutable map.
    pub fn topics_snapshot(&self) -> Vec<Topic> {
        self.topics.values().cloned().collect()
    }

    /// True iff the topic exists and is closed.
    pub fn topic_is_closed(&self, id: &TopicId) -> bool {
        self.topics
            .get(id)
            .map(|t| matches!(t.status, TopicStatus::Closed))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::message::AgentMessageKind;

    fn make(id: &str, sender: &str, kind: AgentMessageKind) -> AgentMessage {
        AgentMessage::new(id, sender, "t-1", 0, kind)
    }

    #[test]
    fn filter_includes_own_emits() {
        // Own messages must be in the visible stream — without this the
        // Sprint 2.4 duplicate-ANSWER fix regresses after a restart.
        let history = vec![make(
            "m-1",
            "PM",
            AgentMessageKind::Broadcast {
                content: "kick".into(),
            },
        )];
        let visible = filter_visible_to(&history, "PM");
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "m-1");
    }

    #[test]
    fn filter_excludes_ask_directed_at_someone_else() {
        let history = vec![make(
            "m-1",
            "PM",
            AgentMessageKind::AskAgent {
                to: "frontend_dev".into(),
                content: "?".into(),
                expected_format: None,
            },
        )];
        // backend_dev wasn't the target and isn't the sender; they
        // never saw this in their inbox at runtime, so reload must
        // hide it too.
        let visible = filter_visible_to(&history, "backend_dev");
        assert!(visible.is_empty());
    }

    #[test]
    fn filter_includes_ask_directed_at_self() {
        let history = vec![make(
            "m-1",
            "PM",
            AgentMessageKind::AskAgent {
                to: "frontend_dev".into(),
                content: "?".into(),
                expected_format: None,
            },
        )];
        let visible = filter_visible_to(&history, "frontend_dev");
        assert_eq!(visible.len(), 1);
    }

    #[test]
    fn filter_includes_broadcast_from_others() {
        let history = vec![make(
            "m-1",
            "PM",
            AgentMessageKind::Broadcast {
                content: "all hands".into(),
            },
        )];
        let visible = filter_visible_to(&history, "frontend_dev");
        assert_eq!(visible.len(), 1);
        let visible = filter_visible_to(&history, "backend_dev");
        assert_eq!(visible.len(), 1);
    }

    #[test]
    fn filter_includes_user_input_for_everyone() {
        let history = vec![make(
            "m-u",
            "user",
            AgentMessageKind::UserInput { content: "hi".into() },
        )];
        for agent in ["PM", "frontend_dev", "backend_dev"] {
            let visible = filter_visible_to(&history, agent);
            assert_eq!(visible.len(), 1, "{agent} should see user input");
        }
    }

    #[test]
    fn apply_message_to_topics_opens_and_closes() {
        let mut topics: HashMap<TopicId, Topic> = HashMap::new();
        let m_open = make(
            "m-1",
            "PM",
            AgentMessageKind::Broadcast {
                content: "kicking off frontend".into(),
            },
        );
        apply_message_to_topics(&mut topics, &m_open);
        assert_eq!(topics.len(), 1);
        let t = topics.get("t-1").unwrap();
        assert!(t.is_active());
        // Title falls back to first message's payload prefix when
        // opens_topic_title is absent.
        assert!(t.title.contains("kicking off"));

        let m_close = AgentMessage::new(
            "m-2",
            "PM",
            "t-1",
            5,
            AgentMessageKind::Summary {
                topic_id: "t-1".into(),
                summary: "done".into(),
                key_decisions: vec!["a".into()],
            },
        );
        apply_message_to_topics(&mut topics, &m_close);
        let t = topics.get("t-1").unwrap();
        assert!(matches!(t.status, TopicStatus::Closed));
        assert_eq!(t.summary, "done");
        assert_eq!(t.key_decisions, vec!["a"]);
    }

    #[test]
    fn apply_message_respects_opens_topic_title() {
        let mut topics: HashMap<TopicId, Topic> = HashMap::new();
        let mut m = make(
            "m-1",
            "PM",
            AgentMessageKind::Broadcast {
                content: "lots of words about something".into(),
            },
        );
        m.opens_topic_title = Some("Frontend stack pick".into());
        apply_message_to_topics(&mut topics, &m);
        assert_eq!(topics["t-1"].title, "Frontend stack pick");
    }
}
