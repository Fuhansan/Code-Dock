//! Tauri IPC command surface.
//!
//! Each `#[tauri::command]` here is callable from the Svelte side via
//! `invoke("name", { ... })`. Keep this module thin — commands should be
//! façades that delegate to `llm`, `keyring_store`, `agent`, not contain logic.

use std::path::PathBuf;

use serde_json::Value as JsonValue;
use tokio::sync::Mutex;

use crate::agent::approval::{new_pending_approvals, ApprovalRegistry, Decision, PendingApprovals};
use crate::agent::mcp::McpCallEvent;
use crate::agent::mcp_log::{read_mcp_calls, MCP_LOG_FILE};
use crate::agent::message::AgentMessage;
use crate::agent::persistence;
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

fn default_session_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".aidock").join("sessions").join("default")
}

/// Start the V0.1 default workshop session (PM + frontend_dev + backend_dev).
/// Idempotent: calling on an already-started session is a no-op.
#[tauri::command]
pub async fn start_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), CommandError> {
    let mut guard = state.session.lock().await;
    if guard.is_some() {
        tracing::debug!(target: "aidock::cmd", "start_session: already running");
        return Ok(());
    }
    let key = keyring_store::load_api_key("bailian")?
        .ok_or_else(|| CommandError::ProviderNotConfigured("bailian".to_string()))?;
    let session = Session::start(
        default_session_dir(),
        key,
        Some(app),
        state.approval.clone(),
        state.pending_approvals.clone(),
    )
    .await?;
    *guard = Some(session);
    tracing::info!(target: "aidock::cmd", "session started");
    Ok(())
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
pub async fn load_message_history() -> Result<Vec<AgentMessage>, CommandError> {
    let path = default_session_dir().join("messages.jsonl");
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
pub async fn load_mcp_call_history() -> Result<Vec<McpCallEvent>, CommandError> {
    let path = default_session_dir().join(MCP_LOG_FILE);
    let calls = read_mcp_calls(&path).map_err(|e| {
        CommandError::Llm(LLMError::Other(format!(
            "could not load MCP call history: {e}"
        )))
    })?;
    Ok(calls)
}
