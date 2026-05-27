//! One Session = one Dispatcher + N Agent tasks + their wiring.
//!
//! Sprint 3 turned this from "spin everything up fresh" into "spin
//! everything up, restoring whatever the last process wrote to disk".
//! Concretely, `Session::start` now:
//!
//!   1. Creates the dispatcher, which rehydrates topic state from
//!      `messages.jsonl` on its own.
//!   2. Takes those loaded messages back from the dispatcher.
//!   3. For each agent in `default_workshop()`:
//!      - Filters messages visible to that agent (route rules in reverse).
//!      - Derives its `AgentState` from the filtered stream.
//!      - Loads its scratchpad from `sessions/{id}/scratchpads/{role}.json`.
//!      - Spawns the runtime task with that boot bundle.
//!
//! V0.1 holds at most one session, hardcoded to the default workshop, in
//! `~/.aidock/sessions/default/`. Sprint V0.4 will extend this to a map
//! keyed by session id.

use std::path::PathBuf;

use tokio::task::JoinHandle;

use crate::agent::dispatcher::{filter_visible_to, Dispatcher, DispatcherError, DispatcherHandle};
use crate::agent::message::{AgentMessage, TopicId};
use crate::agent::roles::default_workshop;
use crate::agent::runtime::{spawn_agent, user_input_message, AgentBoot};
use crate::agent::scratchpad::Scratchpad;
use crate::agent::state::AgentState;

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
    /// Build a Session: open the dispatcher (which rehydrates from disk),
    /// for each role in the default workshop derive its restored state from
    /// the loaded messages, load its scratchpad, and spawn its runtime.
    ///
    /// Pass `None` for `app_handle` to run headless (dev harnesses).
    pub async fn start(
        session_dir: PathBuf,
        api_key: String,
        app_handle: Option<tauri::AppHandle>,
    ) -> Result<Self, SessionError> {
        let mut dispatcher = Dispatcher::new(session_dir.clone(), app_handle).await?;
        let loaded: Vec<AgentMessage> = dispatcher.take_loaded_messages();
        let handle = dispatcher.handle();

        let scratchpad_dir = session_dir.join("scratchpads");

        let mut agent_tasks = Vec::new();
        for role in default_workshop() {
            let inbox_rx = dispatcher.register_agent(role.id.clone());

            // Sprint 3.3: per-agent recovery.
            let visible = filter_visible_to(&loaded, &role.id);
            let initial_state = AgentState::derive_from_history(&visible, &role.id);
            let scratchpad_path = scratchpad_dir.join(format!("{}.json", role.id));
            let initial_scratchpad = Scratchpad::load(&scratchpad_path);

            tracing::info!(
                target: "aidock::session",
                role = %role.id,
                recovered_msgs = visible.len(),
                state = initial_state.tag(),
                scratchpad_focus = %if initial_scratchpad.current_focus.is_empty() {
                    "(none)".to_string()
                } else {
                    initial_scratchpad.current_focus.clone()
                },
                "spawning agent with recovered state"
            );

            let boot = AgentBoot {
                role,
                inbox_rx,
                dispatcher: handle.clone(),
                api_key: api_key.clone(),
                initial_history: visible,
                initial_state,
                initial_scratchpad,
                scratchpad_path,
            };
            agent_tasks.push(spawn_agent(boot));
        }

        let dispatcher_task = tokio::spawn(dispatcher.run());

        tracing::info!(
            target: "aidock::session",
            agent_count = agent_tasks.len(),
            loaded = loaded.len(),
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
