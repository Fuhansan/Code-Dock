//! LLM provider abstraction.
//!
//! Every concrete provider (Bailian / Anthropic / OpenAI / local) implements
//! `LLMProvider`. V0.1 ships only the Bailian impl; the trait is the seam
//! where future providers slot in without touching call sites.
//!
//! Wire-level types in this module are OpenAI-flavored (`tools` / `tool_choice` /
//! `tool_calls`), since Bailian — the first impl — uses the OpenAI-compatible
//! endpoint. Providers that diverge (e.g. Anthropic) translate to/from these
//! at their own boundary.

pub mod bailian;

use serde::{Deserialize, Serialize};

// ---------- Messages ----------

/// One message in a chat conversation. Tag is the `role` field on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    /// Tool execution result fed back to the model.
    Tool {
        content: String,
        tool_call_id: String,
    },
}

/// A tool call the model wants to make.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    /// OpenAI uses literal "function" today; kept open for future kinds.
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Arguments encoded as a JSON string (matches OpenAI wire format).
    pub arguments: String,
}

// ---------- Tools (advertised to the model) ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub kind: String, // always "function" for now
    pub function: ToolDef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema describing the function's parameters.
    pub parameters: serde_json::Value,
}

impl Tool {
    pub fn function(name: impl Into<String>, description: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            kind: "function".to_string(),
            function: ToolDef {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

// ---------- Request / Response ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    /// "auto" | "none" | { "type": "function", "function": { "name": "..." } }
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    /// The assistant's text content, if any.
    pub content: Option<String>,
    /// Tool calls the model wants to make. Empty if none.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// Why generation stopped: "stop" | "length" | "tool_calls" | etc.
    pub finish_reason: String,
    /// Token usage if the provider reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ---------- Errors ----------

#[derive(Debug, thiserror::Error)]
pub enum LLMError {
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),

    /// The provider returned a non-2xx status with an error payload.
    #[error("provider {provider} returned {status}: {message}")]
    Api {
        provider: &'static str,
        status: u16,
        message: String,
    },

    #[error("failed to parse provider response: {0}")]
    ParseError(String),

    #[error("API key not configured for provider '{0}'")]
    NoApiKey(String),

    #[error("{0}")]
    Other(String),
}

// LLMError must be Serialize so Tauri can return it to the frontend.
impl serde::Serialize for LLMError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

// ---------- Provider trait ----------

/// Provider-agnostic LLM interface.
///
/// Sprint 2's multi-Agent runtime calls into this trait, never into a
/// concrete provider. Adding a new provider means a new impl, no caller
/// changes.
pub trait LLMProvider: Send + Sync {
    /// Stable identifier — `"bailian"`, `"anthropic"`, etc. Used as the
    /// keyring service name and for role-config lookup.
    fn provider_id(&self) -> &'static str;

    /// Whether this provider supports OpenAI-style tool/function calling.
    /// V0.1 mandates true (Sprint 2 mechanism requires it).
    fn supports_tool_use(&self) -> bool;

    /// One round-trip chat completion. Streaming is intentionally out of
    /// scope for V0.1 — see AIDOCK_DESIGN.md §10.2.1.
    fn chat_completion(
        &self,
        req: ChatRequest,
    ) -> impl std::future::Future<Output = Result<ChatResponse, LLMError>> + Send;
}
