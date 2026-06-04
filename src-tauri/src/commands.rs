//! Tauri IPC command surface.
//!
//! Each `#[tauri::command]` here is callable from the Svelte side via
//! `invoke("name", { ... })`. Keep this module thin — commands should be
//! façades that delegate to `llm`, `keyring_store`, `agent`, not contain logic.

use std::path::PathBuf;

use serde_json::Value as JsonValue;
use tokio::sync::Mutex;

use crate::agent::approval::{new_pending_approvals, ApprovalRegistry, Decision, PendingApprovals};
use crate::agent::mcp::ToolCallEvent;
use crate::agent::mcp_log::{read_mcp_calls, MCP_LOG_FILE};
use crate::agent::message::AgentMessage;
use crate::agent::persistence;
use crate::agent::session_store::{self, SessionMeta};
use crate::agent::{Session, SessionError};
use crate::keyring_store::{self, KeyringError};
use crate::llm::{
    bailian::BailianProvider, ChatMessage, ChatRequest, ChatResponse, LLMError, LLMProvider, Tool,
};

/// Errors any command can return to the frontend. Serializes as a plain string
/// so JS code can show it directly; if richer structure is needed later, change
/// the `Serialize` impl, not the call sites.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Llm(#[from] LLMError),

    #[error(transparent)]
    Keyring(#[from] KeyringError),

    #[error(transparent)]
    Session(#[from] SessionError),

    #[error("provider '{0}' is not configured — set its API key first")]
    ProviderNotConfigured(String),

    #[error("unknown provider '{0}' — supported providers: bailian")]
    UnknownProvider(String),

    #[error("no active session — call start_session first")]
    NoActiveSession,

    #[error("session not found: {0}")]
    SessionNotFound(String),
}

impl serde::Serialize for CommandError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

// ---------- AppState (managed by Tauri) ----------

/// Process-wide state owned by Tauri's `.manage()`. V0.1 holds at most one
/// running `Session`. Future versions extend this to a map keyed by session id.
pub struct AppState {
    pub session: Mutex<Option<Session>>,
    /// Directory of the currently-active session. Kept alongside `session` so
    /// `load_*_history` / `current_session` target the active session's data
    /// (not a hardcoded path). Always set/cleared together with `session`.
    pub active_dir: Mutex<Option<PathBuf>>,
    /// Sprint 4.6: per-session approval registry for destructive MCP
    /// tools. Lives in AppState so the `respond_to_approval` command can
    /// reach it without going through the session lock.
    pub approval: ApprovalRegistry,
    /// In-flight approval requests awaiting user decision.
    pub pending_approvals: PendingApprovals,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            session: Mutex::new(None),
            active_dir: Mutex::new(None),
            approval: ApprovalRegistry::new(),
            pending_approvals: new_pending_approvals(),
        }
    }
}

// ---------- Sprint 0 health check ----------

/// Echo command used by the Sprint 0 IPC health-check dot. Kept for now —
/// removing it would silently break the green dot in the header.
#[tauri::command]
pub fn greet(name: &str) -> String {
    format!("Hello, {name}! Backend is up.")
}

// ---------- BYOK / keyring ----------

/// Whether a key is configured for `provider`. Cheap, never returns the key.
#[tauri::command]
pub fn provider_status(provider: String) -> bool {
    keyring_store::has_api_key(&provider)
}

/// Persist an API key for the given provider into the OS keychain.
#[tauri::command]
pub fn save_api_key(provider: String, key: String) -> Result<(), CommandError> {
    keyring_store::save_api_key(&provider, &key)?;
    Ok(())
}

// ---------- LLM (Sprint 1 single-agent path) ----------

/// One chat completion round-trip against the chosen provider.
///
/// Resolves the API key from the keychain, instantiates the provider, sends
/// the request, returns the response. V0.1 only routes to Bailian; future
/// providers slot in via the match below.
#[tauri::command]
pub async fn send_chat_message(
    provider: String,
    model: String,
    messages: Vec<ChatMessage>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    tools: Option<Vec<Tool>>,
    tool_choice: Option<JsonValue>,
) -> Result<ChatResponse, CommandError> {
    let key = keyring_store::load_api_key(&provider)?
        .ok_or_else(|| CommandError::ProviderNotConfigured(provider.clone()))?;

    let req = ChatRequest {
        model,
        messages,
        temperature,
        max_tokens,
        tools: tools.unwrap_or_default(),
        tool_choice,
    };

    let resp = match provider.as_str() {
        "bailian" => BailianProvider::new(key).chat_completion(req).await?,
        other => return Err(CommandError::UnknownProvider(other.to_string())),
    };

    Ok(resp)
}

// ---------- Multi-agent session (Sprint 2) ----------

// V0.1：单本地用户 + 单硬编码工作室。TODO（低优，无账号系统暂搁）：有真登录后
// USER 换成账号 id；多工作室编辑器落地后 WORKSHOP 由用户建。路径布局
// users/{USER}/workshops/{WORKSHOP}/sessions/{id} 已为这两层留好位，届时不返工。
const USER: &str = session_store::DEFAULT_USER;
const WORKSHOP: &str = session_store::DEFAULT_WORKSHOP;

/// Stop the current session (its `Drop` aborts the tasks) and start one at `dir`.
/// The single chokepoint for start / new / resume so they all switch cleanly.
async fn switch_to(
    app: tauri::AppHandle,
    state: &AppState,
    dir: PathBuf,
) -> Result<(), CommandError> {
    let key = keyring_store::load_api_key("bailian")?
        .ok_or_else(|| CommandError::ProviderNotConfigured("bailian".to_string()))?;
    // Drop the old session first so its agents stop before the new ones spawn.
    {
        *state.session.lock().await = None;
    }
    let session = Session::start(
        dir.clone(),
        key,
        Some(app),
        state.approval.clone(),
        state.pending_approvals.clone(),
    )
    .await?;
    *state.session.lock().await = Some(session);
    *state.active_dir.lock().await = Some(dir);
    Ok(())
}

/// Launch entrypoint (idempotent): if a session is already running, no-op;
/// otherwise **resume the most-recently-active** session, or create a fresh one
/// if there are none. (Continuing a past session = `Session::start` rehydrating
/// its dir — history, agent state, scratchpad, ④.b memory all come back.)
#[tauri::command]
pub async fn start_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), CommandError> {
    if state.session.lock().await.is_some() {
        tracing::debug!(target: "aidock::cmd", "start_session: already running");
        return Ok(());
    }
    let dir = match session_store::list_sessions(USER, WORKSHOP).into_iter().next() {
        Some(meta) => session_store::session_dir(USER, WORKSHOP, &meta.id),
        None => session_store::session_dir(USER, WORKSHOP, &session_store::new_session_id()),
    };
    switch_to(app, &state, dir).await?;
    tracing::info!(target: "aidock::cmd", "session started (resume-most-recent-or-new)");
    Ok(())
}

/// Start a brand-new, empty session (switching away from the current one).
#[tauri::command]
pub async fn new_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<SessionMeta, CommandError> {
    let id = session_store::new_session_id();
    let dir = session_store::session_dir(USER, WORKSHOP, &id);
    switch_to(app, &state, dir).await?;
    tracing::info!(target: "aidock::cmd", session_id = %id, "new session created");
    Ok(session_store::list_sessions(USER, WORKSHOP)
        .into_iter()
        .find(|m| m.id == id)
        .unwrap_or(SessionMeta {
            id,
            title: "新会话".to_string(),
            created_ms: 0,
            last_active_ms: 0,
            message_count: 0,
        }))
}

/// Resume an existing session by id (continues from its persisted history).
#[tauri::command]
pub async fn resume_session(
    id: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), CommandError> {
    let dir = session_store::session_dir(USER, WORKSHOP, &id);
    if !dir.is_dir() {
        return Err(CommandError::SessionNotFound(id));
    }
    switch_to(app, &state, dir).await?;
    tracing::info!(target: "aidock::cmd", session_id = %id, "session resumed");
    Ok(())
}

/// History list: all sessions for the current user/workshop, newest first.
#[tauri::command]
pub async fn list_sessions() -> Result<Vec<SessionMeta>, CommandError> {
    Ok(session_store::list_sessions(USER, WORKSHOP))
}

/// The currently-active session's metadata (`None` if none started yet).
#[tauri::command]
pub async fn current_session(
    state: tauri::State<'_, AppState>,
) -> Result<Option<SessionMeta>, CommandError> {
    let dir = state.active_dir.lock().await.clone();
    let Some(dir) = dir else {
        return Ok(None);
    };
    let Some(id) = dir.file_name().and_then(|n| n.to_str()).map(String::from) else {
        return Ok(None);
    };
    Ok(session_store::list_sessions(USER, WORKSHOP)
        .into_iter()
        .find(|m| m.id == id))
}

/// Whether a session is currently running.
#[tauri::command]
pub async fn session_status(state: tauri::State<'_, AppState>) -> Result<bool, CommandError> {
    Ok(state.session.lock().await.is_some())
}

/// Push one user-originated message into the running session. Errors if no
/// session has been started.
#[tauri::command]
pub async fn send_user_message(
    content: String,
    topic_id: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), CommandError> {
    let guard = state.session.lock().await;
    let session = guard.as_ref().ok_or(CommandError::NoActiveSession)?;
    session.submit_user_input(content, topic_id).await?;
    Ok(())
}

/// Sprint 4.6: frontend calls this when the user clicks a button on an
/// approval prompt. Looks up the pending sender by id and fulfils the
/// oneshot channel waiting in the runtime.
#[tauri::command]
pub async fn respond_to_approval(
    id: String,
    decision: Decision,
    state: tauri::State<'_, AppState>,
) -> Result<(), CommandError> {
    let mut pending = state.pending_approvals.lock().await;
    match pending.remove(&id) {
        Some(tx) => {
            // Result is OK iff receiver still listening — timeout could
            // have dropped it. Either way nothing else for us to do.
            let _ = tx.send(decision);
            Ok(())
        }
        None => {
            // Either the runtime never registered (bug) or the request
            // already timed out. Tell the user, but not as a hard error.
            tracing::info!(
                target: "aidock::cmd",
                approval_id = %id,
                "respond_to_approval: no pending request — already resolved or timed out"
            );
            Ok(())
        }
    }
}

/// Read the persisted message history for the default session. Sprint 3:
/// front-end calls this on mount so reloaded sessions don't show an empty
/// chat panel even though the backend remembers everything.
///
/// Returns the full stream in arrival order. Malformed lines (typically a
/// crash-truncated tail) are skipped silently by the underlying reader.
#[tauri::command]
pub async fn load_message_history(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<AgentMessage>, CommandError> {
    let Some(dir) = state.active_dir.lock().await.clone() else {
        return Ok(Vec::new()); // no active session yet
    };
    let path = dir.join("messages.jsonl");
    let msgs = persistence::read_jsonl_messages(&path).map_err(|e| {
        // Roll the persistence error up as a generic LLM-ish string for
        // the frontend. The real diagnosis is in the tracing logs.
        CommandError::Llm(LLMError::Other(format!(
            "could not load message history: {e}"
        )))
    })?;
    Ok(msgs)
}

/// Sprint 4 polish: like `load_message_history` but for the MCP-call log
/// so reloaded sessions can show prior tool-call cards too, not just
/// messages.
#[tauri::command]
pub async fn load_mcp_call_history(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ToolCallEvent>, CommandError> {
    let Some(dir) = state.active_dir.lock().await.clone() else {
        return Ok(Vec::new()); // no active session yet
    };
    let path = dir.join(MCP_LOG_FILE);
    let calls = read_mcp_calls(&path).map_err(|e| {
        CommandError::Llm(LLMError::Other(format!(
            "could not load MCP call history: {e}"
        )))
    })?;
    Ok(calls)
}
