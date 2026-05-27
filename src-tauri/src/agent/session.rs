//! One Session = one Dispatcher + N Agent tasks + their wiring.
//!
//! V0.1 ships a single, hardcoded session: the default workshop (PM,
//! frontend_dev, backend_dev) in `~/.aidock/sessions/default/`. Sprint 0.4
//! adds multi-session support; the shape of this struct is sized for that
//! (each `Session` is independent), it's just that for now we only ever hold
//! one.

use std::path::PathBuf;

use tokio::task::JoinHandle;

use crate::agent::dispatcher::{Dispatcher, DispatcherError, DispatcherHandle};
use crate::agent::message::{AgentMessage, TopicId};
use crate::agent::roles::default_workshop;
use crate::agent::runtime::{spawn_agent, user_input_message};

/// V0.1 default topic id. Sprint 2.6 introduces dynamic topic creation
/// driven by Agents declaring `new_topic_title`.
pub const DEFAULT_TOPIC_ID: &str = "t-default";

/// Holds the dispatcher handle plus the JoinHandles of every spawned task.
/// Tasks are kept alive for the lifetime of `Session`; dropping the struct
/// implicitly cancels them (Tokio drops their futures).
pub struct Session {
    handle: DispatcherHandle,
    // Keep JoinHandles so they're not dropped prematurely. Underscore-prefixed
    // because we never await them directly in V0.1 — they run for the
    // session's lifetime and stop when their channels close.
    _dispatcher_task: JoinHandle<()>,
    _agent_tasks: Vec<JoinHandle<()>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Dispatcher(#[from] DispatcherError),
    #[error("could not submit user message: dispatcher channel closed")]
    SubmitClosed,
}

impl Session {
    /// Build a Session: open the dispatcher, register every Agent in the
    /// default workshop, spawn each Agent's runtime task and the dispatcher
    /// task. Pass `None` for `app_handle` to run headless (dev harnesses).
    pub async fn start(
        session_dir: PathBuf,
        api_key: String,
        app_handle: Option<tauri::AppHandle>,
    ) -> Result<Self, SessionError> {
        let mut dispatcher = Dispatcher::new(session_dir, app_handle).await?;
        let handle = dispatcher.handle();

        let mut agent_tasks = Vec::new();
        for role in default_workshop() {
            let inbox_rx = dispatcher.register_agent(role.id.clone());
            let task = spawn_agent(role, inbox_rx, handle.clone(), api_key.clone());
            agent_tasks.push(task);
        }

        let dispatcher_task = tokio::spawn(dispatcher.run());

        tracing::info!(
            target: "aidock::session",
            agent_count = agent_tasks.len(),
            "session started"
        );

        Ok(Self {
            handle,
            _dispatcher_task: dispatcher_task,
            _agent_tasks: agent_tasks,
        })
    }

    /// Push one human-originated message into the chat. The topic_id may be
    /// omitted to use the session's default topic — V0.1 has only one.
    pub async fn submit_user_input(
        &self,
        content: String,
        topic_id: Option<TopicId>,
    ) -> Result<(), SessionError> {
        let topic = topic_id.unwrap_or_else(|| DEFAULT_TOPIC_ID.to_string());
        let msg = user_input_message(content, topic);
        self.handle
            .submit(msg)
            .await
            .map_err(|_| SessionError::SubmitClosed)
    }

    /// Push an arbitrary AgentMessage — escape hatch for testing.
    #[allow(dead_code)]
    pub async fn submit(&self, msg: AgentMessage) -> Result<(), SessionError> {
        self.handle
            .submit(msg)
            .await
            .map_err(|_| SessionError::SubmitClosed)
    }
}
