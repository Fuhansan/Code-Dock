//! Tauri IPC command surface.
//!
//! Each `#[tauri::command]` here is callable from the Svelte side via
//! `invoke("name", { ... })`. Keep this module thin — commands should be
//! façades that delegate to `llm`, `keyring_store`, etc., not contain logic.

use serde_json::Value as JsonValue;

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

    #[error("provider '{0}' is not configured — set its API key first")]
    ProviderNotConfigured(String),

    #[error("unknown provider '{0}' — supported providers: bailian")]
    UnknownProvider(String),
}

impl serde::Serialize for CommandError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
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

// ---------- LLM ----------

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
