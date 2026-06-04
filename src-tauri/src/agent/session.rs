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

use crate::agent::approval::{ApprovalRegistry, PendingApprovals};
use crate::agent::dispatcher::{filter_visible_to, Dispatcher, DispatcherError, DispatcherHandle};
use crate::agent::lsp_pool::LspPool;
use crate::agent::mcp::McpClient;
use crate::agent::mcp_log::{spawn_writer as spawn_mcp_log_writer, McpLogHandle, MCP_LOG_FILE};
use crate::agent::message::{AgentMessage, TopicId};
use crate::agent::roles::default_workshop;
use crate::agent::runtime::{spawn_agent, user_input_message, AgentBoot};
use crate::agent::scratchpad::Scratchpad;
use crate::agent::state::AgentState;

/// V0.1 default topic id. Sprint 2.6 introduces dynamic topic creation
/// driven by Agents declaring `new_topic_title`.
pub const DEFAULT_TOPIC_ID: &str = "t-default";

/// Subdirectory under each session that is the agents' trusted workspace.
/// Agents write real code here — the native file tools' L3 confinement
/// (`fs_tools::within_workspace`) denies writes/deletes outside it.
pub const WORKSPACE_SUBDIR: &str = "workspace";

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
    /// Sprint 4 polish: MCP-call log writer task. Lives for the session's
    /// lifetime; drops cleanly when the last `McpLogHandle` clone goes.
    _mcp_log_task: Option<JoinHandle<()>>,
}

impl Drop for Session {
    /// Abort the dispatcher + agent + log tasks so switching/closing a session
    /// actually stops it. (Dropping a `JoinHandle` only *detaches* the task —
    /// it keeps running — so for a clean session switch we must `abort()`.)
    fn drop(&mut self) {
        self._dispatcher_task.abort();
        for t in &self._agent_tasks {
            t.abort();
        }
        if let Some(t) = &self._mcp_log_task {
            t.abort();
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Dispatcher(#[from] DispatcherError),
    #[error("could not submit user message: dispatcher channel closed")]
    SubmitClosed,
    #[error("workspace dir setup: {context}: {source}")]
    Workspace {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("mcp log setup: {0}")]
    McpLog(#[source] std::io::Error),
}

/// Ensure `session_dir/workspace/` exists and return its canonical path.
/// Canonicalize because the native file tools' confinement check matches the
/// target against the canonical workspace root (the macOS /var → /private/var
/// symlink would otherwise look like a path-traversal escape).
async fn ensure_workspace_dir(session_dir: &PathBuf) -> Result<PathBuf, SessionError> {
    let raw = session_dir.join(WORKSPACE_SUBDIR);
    tokio::fs::create_dir_all(&raw)
        .await
        .map_err(|source| SessionError::Workspace {
            context: "create workspace dir",
            source,
        })?;
    std::fs::canonicalize(&raw).map_err(|source| SessionError::Workspace {
        context: "canonicalize workspace dir",
        source,
    })
}

// 旧的 `build_filesystem_mcp_command` / `spawn_filesystem_mcp` 已删除——参考文件
// MCP 服务器不再自动启动（见 `Session::start` 内说明）。将来接用户自配 MCP 服务器时
// 再按配置重建启动逻辑。

impl Session {
    /// Build a Session: open the dispatcher (which rehydrates from disk),
    /// then for each role in the default workshop derive its restored state,
    /// load its scratchpad, and spawn its runtime.
    ///
    /// File access is via native tools (`fs_tools.rs`), so no MCP server is
    /// started here (`mcp = None`); the MCP plumbing stays for future
    /// user-configured `mcp__` servers.
    ///
    /// Pass `None` for `app_handle` to run headless (dev harnesses).
    pub async fn start(
        session_dir: PathBuf,
        api_key: String,
        app_handle: Option<tauri::AppHandle>,
        approval: ApprovalRegistry,
        pending_approvals: PendingApprovals,
    ) -> Result<Self, SessionError> {
        // Clone before move-into-dispatcher so we can also hand it to
        // each agent runtime for MCP call-event emission (Sprint 4.5).
        let app_handle_for_agents = app_handle.clone();
        let mut dispatcher = Dispatcher::new(session_dir.clone(), app_handle).await?;
        let loaded: Vec<AgentMessage> = dispatcher.take_loaded_messages();
        let handle = dispatcher.handle();

        let scratchpad_dir = session_dir.join("scratchpads");
        let workspace_dir = ensure_workspace_dir(&session_dir).await?;
        let mcp_log_path = session_dir.join(MCP_LOG_FILE);

        // Sprint 4 polish: spawn the MCP-call log writer task. Each agent
        // gets a cheap-clone handle into its mpsc channel.
        let (mcp_log_handle, mcp_log_task) = spawn_mcp_log_writer(mcp_log_path)
            .await
            .map(|(h, t)| (Some(h), Some(t)))
            .map_err(SessionError::McpLog)?;

        // ④ 工具系统重定义（2026-06）：文件操作改走**原生** Read/Edit/Write/Glob/Grep
        // （`fs_tools.rs`），不再用 Node 参考文件服务器 `@modelcontextprotocol/server-filesystem`。
        // 它的工具已不进任何角色目录、永远 advertise 不出去，再起它就是白占一个 npx
        // 子进程。这里**不再自动起它**。MCP 管道（`McpClient`/`McpClientBackend`/
        // `CompositeExecutor` 的统一安全检查路由）保留，留给将来"用户自配 mcp__ 服务器"，
        // 届时把 `None` 换成真正按用户配置启动的客户端即可。
        let mcp: Option<McpClient> = None;

        // ④ LSP：会话级 server 池，所有 agent 共享一份（Arc）。语言服务器懒启动、跨
        // agent/回合复用（rust-analyzer 索引昂贵，不能每 agent 各起一个）。
        let lsp_pool = LspPool::new(workspace_dir.clone());

        let mut agent_tasks = Vec::new();
        for role in default_workshop() {
            let inbox_rx = dispatcher.register_agent(role.id.clone());

            // Sprint 3.3: per-agent recovery.
            let visible = filter_visible_to(&loaded, &role.id);
            let initial_state = AgentState::derive_from_history(&visible, &role.id);
            let scratchpad_path = scratchpad_dir.join(format!("{}.json", role.id));
            let initial_scratchpad = Scratchpad::load(&scratchpad_path);
            // ④.b memory: {session}/agents/{id}/memory (per CLAUDE.md ④.b).
            let memory_root = session_dir.join("agents").join(&role.id).join("memory");

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
                memory_root,
                workspace_dir: workspace_dir.clone(),
                lsp_pool: lsp_pool.clone(),
                mcp: mcp.clone(),
                app_handle: app_handle_for_agents.clone(),
                approval: approval.clone(),
                pending_approvals: pending_approvals.clone(),
                mcp_log: mcp_log_handle.clone(),
            };
            agent_tasks.push(spawn_agent(boot));
        }

        let dispatcher_task = tokio::spawn(dispatcher.run());

        tracing::info!(
            target: "aidock::session",
            agent_count = agent_tasks.len(),
            loaded = loaded.len(),
            mcp_enabled = mcp.is_some(),
            "session started"
        );

        Ok(Self {
            handle,
            _dispatcher_task: dispatcher_task,
            _agent_tasks: agent_tasks,
            _mcp_log_task: mcp_log_task,
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
