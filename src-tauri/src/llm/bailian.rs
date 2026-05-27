//! Aliyun Bailian (Qwen) provider.
//!
//! Bailian exposes an OpenAI-compatible chat completions endpoint, so most of
//! `ChatRequest` flows through unchanged. We translate at the response side:
//! Bailian returns the OpenAI `choices[0].message.{content, tool_calls}` shape,
//! we lift that into our trait-level `ChatResponse`.
//!
//! Endpoint reference:
//! https://help.aliyun.com/zh/model-studio/developer-reference/compatibility-of-openai-with-dashscope

use serde::Deserialize;
use std::time::Duration;

use super::{ChatRequest, ChatResponse, LLMError, LLMProvider, ToolCall, Usage};

const DEFAULT_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";
const PROVIDER_ID: &str = "bailian";

/// Bailian / Qwen provider.
pub struct BailianProvider {
    api_key: String,
    base_url: String,
    http: reqwest::Client,
}

impl BailianProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            // Bailian can keep the connection idle a while between requests.
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .expect("reqwest client should build with rustls");
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            http,
        }
    }
}

// ---------- Bailian-side wire types ----------

#[derive(Debug, Deserialize)]
struct BailianResponse {
    choices: Vec<BailianChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct BailianChoice {
    message: BailianMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BailianMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Deserialize)]
struct BailianErrorEnvelope {
    error: BailianErrorBody,
}

#[derive(Debug, Deserialize)]
struct BailianErrorBody {
    message: String,
    #[serde(default)]
    #[allow(dead_code)] // surfaced via tracing, not returned
    code: Option<String>,
}

// ---------- Trait impl ----------

impl LLMProvider for BailianProvider {
    fn provider_id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn supports_tool_use(&self) -> bool {
        true
    }

    async fn chat_completion(&self, req: ChatRequest) -> Result<ChatResponse, LLMError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        tracing::debug!(
            target: "aidock::llm::bailian",
            model = %req.model,
            msg_count = req.messages.len(),
            tool_count = req.tools.len(),
            "sending chat completion request"
        );

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await?;

        let status = resp.status();

        if !status.is_success() {
            // Try to extract Bailian's structured error; fall back to raw body.
            let body = resp.text().await.unwrap_or_default();
            let message = serde_json::from_str::<BailianErrorEnvelope>(&body)
                .map(|e| e.error.message)
                .unwrap_or_else(|_| {
                    if body.is_empty() {
                        format!("HTTP {} with empty body", status.as_u16())
                    } else {
                        body
                    }
                });
            tracing::warn!(
                target: "aidock::llm::bailian",
                status = status.as_u16(),
                %message,
                "Bailian API returned error"
            );
            return Err(LLMError::Api {
                provider: PROVIDER_ID,
                status: status.as_u16(),
                message,
            });
        }

        let body_text = resp.text().await?;
        let parsed: BailianResponse = serde_json::from_str(&body_text)
            .map_err(|e| LLMError::ParseError(format!("{e}; body was: {body_text}")))?;

        let choice = parsed.choices.into_iter().next().ok_or_else(|| {
            LLMError::ParseError("Bailian response had no choices".to_string())
        })?;

        Ok(ChatResponse {
            content: choice.message.content,
            tool_calls: choice.message.tool_calls,
            finish_reason: choice.finish_reason.unwrap_or_else(|| "unknown".to_string()),
            usage: parsed.usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, ChatRequest};

    #[test]
    fn provider_advertises_tool_use() {
        let p = BailianProvider::new("sk-test");
        assert_eq!(p.provider_id(), "bailian");
        assert!(p.supports_tool_use());
    }

    #[test]
    fn request_serializes_to_openai_shape() {
        // Sanity check that our ChatRequest serializes to the field names
        // Bailian's compatible-mode expects. Catches accidental renames.
        let req = ChatRequest {
            model: "qwen3.6-plus".into(),
            messages: vec![ChatMessage::User { content: "hi".into() }],
            temperature: Some(0.3),
            max_tokens: Some(64),
            tools: vec![],
            tool_choice: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "qwen3.6-plus");
        assert_eq!(json["temperature"], 0.3);
        assert_eq!(json["max_tokens"], 64);
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "hi");
        // tools omitted when empty, not serialized as []
        assert!(json.get("tools").is_none());
    }
}
