//! Tauri IPC command surface.
//!
//! Each `#[tauri::command]` here is callable from the Svelte side via
//! `invoke("name", { ... })`. Keep this module thin — commands should be
//! façades that delegate to `llm`, `keyring_store`, `agent`, not contain logic.

use std::path::PathBuf;

use serde_json::Value as JsonValue;
use tauri::Emitter;
use tokio::sync::Mutex;

use crate::agent::approval::{new_pending_approvals, ApprovalRegistry, Decision, PendingApprovals};
use crate::agent::mcp::ToolCallEvent;
use crate::agent::mcp_log::{read_mcp_calls, MCP_LOG_FILE};
use crate::agent::message::AgentMessage;
use crate::agent::persistence;
use crate::agent::session_store::{self, SessionMeta};
use crate::agent::workshop_store;
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
    // 角色来自工作室定义（workshop.json，没有则种子）——而非写死。这样改了工作室
    // 定义后重建会话即换上新角色（P2 热应用的机制基础）。
    let roles = workshop_store::load_workshop(USER, WORKSHOP).roles;
    let session = Session::start(
        dir.clone(),
        roles,
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

/// Launch entrypoint (idempotent): if a session is already running, no-op.
/// Otherwise **resume the most-recently-active non-empty session**. Crucially it
/// does NOT auto-create a session when there are none — that's what produced a
/// phantom "新会话" in the list on every startup. Instead it first prunes any
/// leftover empty sessions (a 新建 that was never used) and, if nothing remains,
/// leaves the app with no active session: the UI shows the empty state and the
/// user starts one explicitly via 新建. A new session must be a deliberate act.
#[tauri::command]
pub async fn start_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), CommandError> {
    if state.session.lock().await.is_some() {
        tracing::debug!(target: "aidock::cmd", "start_session: already running");
        return Ok(());
    }
    // 清掉残留空会话（新建后没发过消息的）——它们不该作为幽灵留到下次启动。
    for meta in session_store::list_sessions(USER, WORKSHOP) {
        let d = session_store::session_dir(USER, WORKSHOP, &meta.id);
        if session_store::is_empty_session(&d) {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
    // 续接剩下里最近的真会话；一个都没有就**不造**，保持无活跃会话。
    match session_store::list_sessions(USER, WORKSHOP).into_iter().next() {
        Some(meta) => {
            let dir = session_store::session_dir(USER, WORKSHOP, &meta.id);
            switch_to(app, &state, dir).await?;
            tracing::info!(target: "aidock::cmd", "session resumed (most-recent non-empty)");
        }
        None => {
            tracing::info!(target: "aidock::cmd", "no sessions; idle until 新建/first message");
        }
    }
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

/// 一个工作室成员（角色）的展示信息 + 实时工作状态。给右栏成员面板用。
#[derive(serde::Serialize)]
pub struct MemberInfo {
    pub id: String,
    pub display_name: String,
    pub description: String,
    /// 承接用户对话的**主理/激活**角色（V0.1 = PM，runtime 里唯一接 UserInput 的）。
    /// 进工作室后它就是和用户对接的那个；其余成员被它调度。
    pub is_primary: bool,
    /// 实时工作状态（从当前会话历史派生）："idle" | "working" | "waiting_answer"。
    pub status: String,
    /// WORKING 时的任务一句话（给 UI 当副标题/tooltip），否则空串。
    pub status_detail: String,
}

/// 当前工作室的成员 + 各自实时工作状态。成员来自 `roles::default_workshop()`；
/// 状态从当前会话的 `messages.jsonl` 用 `AgentState::derive_from_history` 派生
/// （无会话时全部 idle）。前端在消息流动时重拉，得到接近实时的状态。
#[tauri::command]
pub async fn list_members(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<MemberInfo>, CommandError> {
    use crate::agent::state::AgentState;
    let msgs = match state.active_dir.lock().await.clone() {
        Some(dir) => {
            persistence::read_jsonl_messages(&dir.join("messages.jsonl")).unwrap_or_default()
        }
        None => Vec::new(),
    };
    let members = crate::agent::roles::default_workshop()
        .into_iter()
        .map(|role| {
            let (status, status_detail) =
                match AgentState::derive_from_history(&msgs, &role.id) {
                    AgentState::Idle => ("idle".to_string(), String::new()),
                    AgentState::Working { task, .. } => ("working".to_string(), task),
                    AgentState::WaitingAnswer { .. } => {
                        ("waiting_answer".to_string(), String::new())
                    }
                };
            MemberInfo {
                is_primary: role.is_coordinator,
                id: role.id,
                display_name: role.display_name,
                description: role.description,
                status,
                status_detail,
            }
        })
        .collect();
    Ok(members)
}

/// 重命名会话（写入 meta.json 的 title，覆盖 LLM/派生标题）。
#[tauri::command]
pub async fn rename_session(id: String, title: String) -> Result<(), CommandError> {
    let dir = session_store::session_dir(USER, WORKSHOP, &id);
    if !dir.is_dir() {
        return Err(CommandError::SessionNotFound(id));
    }
    let t = title.trim();
    if t.is_empty() {
        return Ok(()); // 空标题忽略（回落派生标题）
    }
    session_store::write_title(&dir, t)
        .map_err(|e| CommandError::Llm(LLMError::Other(format!("rename failed: {e}"))))?;
    Ok(())
}

/// 删除会话（连目录一起删）。若删的是当前活跃会话，先停掉它再删，然后切到剩下里
/// 最近的一个（没有就新建一个），保证 app 始终有个活跃会话。
#[tauri::command]
pub async fn delete_session(
    id: String,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), CommandError> {
    let dir = session_store::session_dir(USER, WORKSHOP, &id);
    if !dir.is_dir() {
        return Err(CommandError::SessionNotFound(id));
    }
    let was_active = state
        .active_dir
        .lock()
        .await
        .as_ref()
        .and_then(|d| d.file_name().and_then(|n| n.to_str()))
        .map(|n| n == id)
        .unwrap_or(false);

    if was_active {
        // 停掉正在用它的 Session，再删它的目录。
        *state.session.lock().await = None;
        *state.active_dir.lock().await = None;
    }
    std::fs::remove_dir_all(&dir)
        .map_err(|e| CommandError::Llm(LLMError::Other(format!("delete failed: {e}"))))?;

    if was_active {
        let next = session_store::list_sessions(USER, WORKSHOP)
            .into_iter()
            .next()
            .map(|m| session_store::session_dir(USER, WORKSHOP, &m.id))
            .unwrap_or_else(|| {
                session_store::session_dir(USER, WORKSHOP, &session_store::new_session_id())
            });
        switch_to(app, &state, next).await?;
    }
    Ok(())
}

// 分享：先不实现（用户要求）。未来可在此加 export/share 命令。

/// Whether a session is currently running.
#[tauri::command]
pub async fn session_status(state: tauri::State<'_, AppState>) -> Result<bool, CommandError> {
    Ok(state.session.lock().await.is_some())
}

/// Push one user-originated message into the running session. Errors if no
/// session has been started.
/// Tauri event emitted when a session's LLM-generated title is ready, so the
/// frontend can update its session list without re-polling.
pub const SESSION_TITLED_EVENT: &str = "aidock:session_titled";

#[derive(Clone, serde::Serialize)]
struct SessionTitled {
    id: String,
    title: String,
}

/// One-shot LLM call: condense the user's first message into a short session
/// title (intent, not the raw message). Best-effort — `None` on any failure, in
/// which case the list keeps the truncated-first-message fallback.
async fn generate_session_title(first_message: &str, key: &str) -> Option<String> {
    let req = ChatRequest {
        // 标题是轻活：用轻量级 flash 模型，比 agent 用的 plus 快得多（避免标题比正文还慢）。
        model: "qwen3.6-flash".to_string(),
        messages: vec![
            ChatMessage::System {
                content: "你是会话标题生成器。根据用户的第一条需求，用一个不超过 12 个汉字的\
                          简短中文短语概括其意图，作为会话标题。只输出标题本身，不要引号、标点、\
                          解释或任何前后缀。"
                    .to_string(),
            },
            ChatMessage::User {
                content: first_message.chars().take(500).collect(),
            },
        ],
        temperature: Some(0.2),
        max_tokens: Some(32),
        tools: Vec::new(),
        tool_choice: None,
    };
    let resp = BailianProvider::new(key.to_string())
        .chat_completion(req)
        .await
        .ok()?;
    let title: String = resp
        .content?
        .trim()
        .trim_matches(|c: char| matches!(c, '"' | '「' | '」' | '“' | '”' | '：' | ':'))
        .trim()
        .chars()
        .take(24)
        .collect();
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

/// Push one user-originated message into the running session. On the FIRST
/// message of a session, kick off an LLM title generation in the background
/// (non-blocking) and emit `SESSION_TITLED_EVENT` when it's ready.
#[tauri::command]
pub async fn send_user_message(
    content: String,
    topic_id: Option<String>,
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), CommandError> {
    let dir = state.active_dir.lock().await.clone();
    // 发之前会话是否还空 → 这条是不是首条（只在首条触发起标题）。
    let should_title = dir
        .as_ref()
        .map(|d| session_store::is_empty_session(d))
        .unwrap_or(false);

    {
        let guard = state.session.lock().await;
        let session = guard.as_ref().ok_or(CommandError::NoActiveSession)?;
        session.submit_user_input(content.clone(), topic_id).await?;
    }

    if should_title {
        if let Some(dir) = dir {
            let id = dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from)
                .unwrap_or_default();
            tokio::spawn(async move {
                // 试着用 LLM 起标题；不论成功与否都发事件，让前端首条消息后刷新列表
                // （这个会话从"空(不显示)"变成"有内容(该显示)"）。
                let mut title = String::new();
                if let Ok(Some(key)) = keyring_store::load_api_key("bailian") {
                    if let Some(t) = generate_session_title(&content, &key).await {
                        let _ = session_store::write_title(&dir, &t);
                        title = t;
                    }
                }
                let _ = app.emit(SESSION_TITLED_EVENT, SessionTitled { id, title });
            });
        }
    }
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
