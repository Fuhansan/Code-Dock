//! Model Context Protocol (MCP) client integration.
//!
//! Wraps the official [`rmcp`] SDK in a small async-safe handle the
//! Session can hand to agents. Sprint 4 architecture (validated by the
//! `rmcp_smoke` example, commit 27cab05):
//!
//!   * One subprocess per MCP server. V0.1 uses just one: the Node-based
//!     reference filesystem server (`@modelcontextprotocol/server-filesystem`).
//!   * Tools are discovered at session start via `list_all_tools` and
//!     re-projected into the LLM-side `Tool` schema so they show up in
//!     `all_tools()` alongside our action / query / scratchpad tools.
//!   * Calls are async: an agent emits a tool call, runtime dispatches
//!     here, we forward to the MCP service, return the result. Same
//!     auxiliary-loop pattern as `recall_topic` / `update_scratchpad` —
//!     the tool returns data, the agent acts on it next turn.
//!
//! Lifecycle: `McpClient::start` blocks until the MCP server has
//! handshaken (typically ~200ms but `npx -y` first-run can take 30s+).
//! `McpClient::shutdown` cancels the rmcp service which drives the
//! child process to exit gracefully (waiting up to 3s before SIGKILL).

use std::sync::Arc;

use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde::Serialize;
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::llm::Tool as LlmTool;

/// Convention prefix every MCP-routed tool name carries on the LLM side.
/// Keeps MCP tools from colliding with our own action / query / scratchpad
/// tool names and gives the runtime a cheap way to detect "is this an
/// MCP tool?" without a registry lookup.
pub const MCP_TOOL_PREFIX: &str = "fs__";

/// Tauri event name the frontend subscribes to for live MCP tool call
/// visibility (Sprint 4.5). Emitted from the agent runtime, one event per
/// executed MCP tool call.
pub const MCP_CALL_EVENT: &str = "aidock:mcp_call";

/// What the front-end sees for one MCP tool execution. Lightweight by
/// design — args / result are truncated previews, full audit lives in
/// tracing logs.
#[derive(Clone, Debug, Serialize)]
pub struct McpCallEvent {
    /// Stable id for de-duplication on the frontend if needed.
    pub id: String,
    /// Unix milliseconds, same clock as AgentMessage timestamps so a
    /// merged timeline sorts cleanly.
    pub timestamp: i64,
    /// Role id of the agent that issued the call.
    pub agent: String,
    /// Full LLM-side tool name (with `fs__` prefix).
    pub tool: String,
    /// Truncated argument JSON for display. Capped at PREVIEW_MAX chars.
    pub args_preview: String,
    /// Truncated result body for display. Capped at PREVIEW_MAX chars.
    pub result_preview: String,
    /// True iff the result didn't begin with "ERROR:".
    pub success: bool,
}

/// Cap on args / result preview length. Long writes get a `…` ellipsis
/// so the UI stays compact and the prompt-side render_tool_result still
/// has the full text for the LLM.
pub const MCP_PREVIEW_MAX: usize = 200;

/// Build an event from a raw arg JSON + a result string. The runtime
/// calls this immediately after a successful (or error-returned)
/// `McpClient::call_tool` and emits via Tauri.
pub fn make_call_event(
    agent: &str,
    tool: &str,
    raw_args: &str,
    result_body: &str,
    timestamp: i64,
) -> McpCallEvent {
    let success = !result_body.trim_start().starts_with("ERROR:");
    McpCallEvent {
        id: format!("mcpc-{}", uuid::Uuid::new_v4()),
        timestamp,
        agent: agent.to_string(),
        tool: tool.to_string(),
        args_preview: truncate(raw_args, MCP_PREVIEW_MAX),
        result_preview: truncate(result_body, MCP_PREVIEW_MAX),
        success,
    }
}

fn truncate(s: &str, max: usize) -> String {
    // `chars()` keeps us at code-point boundaries — `s[..max]` would
    // panic on multibyte text (Chinese tool args, etc).
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Cheap async-shared MCP client handle. `Clone` is cheap (Arc), so the
/// session distributes one to each agent runtime.
#[derive(Clone)]
pub struct McpClient {
    inner: Arc<Mutex<RunningService<RoleClient, ()>>>,
    /// Tool schemas projected to LLM format, captured at startup. We
    /// don't re-fetch on every call — the filesystem server's tools are
    /// static across the session.
    advertised_tools: Arc<Vec<LlmTool>>,
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("io: {context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("rmcp service error: {0}")]
    Service(String),
    #[error("tool '{0}' not advertised by any MCP server")]
    UnknownTool(String),
    #[error("invalid arguments for tool '{tool}': not a JSON object")]
    InvalidArgs { tool: String },
}

impl McpClient {
    /// Spawn an MCP server via the given command and wait for protocol
    /// initialisation. The command is whatever brings the server up on
    /// stdio — typically `npx -y @modelcontextprotocol/server-filesystem
    /// <allowed_dir>` for V0.1.
    ///
    /// Tool prefix: all advertised tool names are prefixed with
    /// `MCP_TOOL_PREFIX` on the LLM side. The raw server name is what we
    /// pass through on `call_tool`.
    pub async fn start(command: Command) -> Result<Self, McpError> {
        let transport = TokioChildProcess::new(command).map_err(|source| McpError::Io {
            context: "spawn MCP server subprocess",
            source,
        })?;

        let service = ().serve(transport).await.map_err(|e| {
            tracing::error!(target: "aidock::mcp", error = %e, "MCP service init failed");
            McpError::Service(e.to_string())
        })?;

        let raw_tools = service
            .list_all_tools()
            .await
            .map_err(|e| McpError::Service(e.to_string()))?;

        let advertised_tools: Vec<LlmTool> = raw_tools
            .iter()
            .map(|t| project_to_llm_tool(t.name.as_ref(), &t.description, &t.input_schema))
            .collect();

        tracing::info!(
            target: "aidock::mcp",
            tool_count = advertised_tools.len(),
            "MCP server ready"
        );

        Ok(Self {
            inner: Arc::new(Mutex::new(service)),
            advertised_tools: Arc::new(advertised_tools),
        })
    }

    /// LLM-side tool schemas the runtime should advertise. Already
    /// prefixed; descriptions trimmed; arguments forwarded verbatim.
    pub fn advertised_tools(&self) -> &[LlmTool] {
        &self.advertised_tools
    }

    /// True iff `name` looks like one of this client's tools (prefix
    /// match — cheaper than a registry lookup for the runtime hot path).
    pub fn handles(&self, name: &str) -> bool {
        name.starts_with(MCP_TOOL_PREFIX)
            && self
                .advertised_tools
                .iter()
                .any(|t| t.function.name == name)
    }

    /// Forward one tool call to the MCP server. `raw_args` is the JSON
    /// string the LLM produced (matches OpenAI tool-use wire format).
    /// Returns the human-readable text response from the server — what
    /// we'll feed back to the LLM as the `tool` role content.
    pub async fn call_tool(&self, name: &str, raw_args: &str) -> Result<String, McpError> {
        let bare = name
            .strip_prefix(MCP_TOOL_PREFIX)
            .ok_or_else(|| McpError::UnknownTool(name.to_string()))?;
        // Allow either a JSON object or "{}". Anything else is the LLM
        // misbehaving; surface that as InvalidArgs so the runtime can
        // tell the model to try again.
        let parsed: Value = serde_json::from_str(raw_args)
            .map_err(|_| McpError::InvalidArgs { tool: name.to_string() })?;
        let args_map = match parsed {
            Value::Object(map) => Some(map),
            Value::Null => None,
            _ => return Err(McpError::InvalidArgs { tool: name.to_string() }),
        };

        let req = match args_map {
            Some(m) => CallToolRequestParams::new(bare.to_string()).with_arguments(m),
            None => CallToolRequestParams::new(bare.to_string()),
        };

        let service = self.inner.lock().await;
        let result = service
            .call_tool(req)
            .await
            .map_err(|e| McpError::Service(e.to_string()))?;

        Ok(render_tool_result(&result))
    }

    /// Gracefully stop the MCP server. Idempotent — the rmcp service's
    /// own Drop also tears the child down, this just runs first when we
    /// have a clean shutdown path.
    pub async fn shutdown(self) {
        // Try to extract sole ownership; if other clones still hold the
        // Arc, the child will exit when the last clone drops anyway.
        let Some(mutex) = Arc::into_inner(self.inner) else {
            tracing::debug!(target: "aidock::mcp", "shutdown: other refs still alive, deferring");
            return;
        };
        let service = mutex.into_inner();
        match service.cancel().await {
            Ok(reason) => tracing::info!(
                target: "aidock::mcp",
                ?reason,
                "MCP service cancelled"
            ),
            Err(e) => tracing::warn!(
                target: "aidock::mcp",
                error = %e,
                "MCP service cancel returned error"
            ),
        }
    }
}

// ---------- projection helpers ----------

fn project_to_llm_tool(
    server_name: &str,
    description: &Option<std::borrow::Cow<'static, str>>,
    input_schema: &Arc<rmcp::model::JsonObject>,
) -> LlmTool {
    let llm_name = format!("{MCP_TOOL_PREFIX}{server_name}");
    let desc = description
        .as_ref()
        .map(|s| s.to_string())
        .unwrap_or_default();
    // Use the server's input_schema verbatim — it's already JSON Schema
    // and Qwen / Claude / OpenAI all accept that shape directly.
    let parameters = Value::Object((**input_schema).clone());
    LlmTool::function(llm_name, desc, parameters)
}

fn render_tool_result(result: &rmcp::model::CallToolResult) -> String {
    let mut out = String::new();
    if result.is_error.unwrap_or(false) {
        out.push_str("ERROR: ");
    }
    for content in &result.content {
        // `Annotated<RawContent>` — debug-print is good enough for V0.1.
        // We pull the text body when it's there to keep prompts tight.
        match &content.raw {
            rmcp::model::RawContent::Text(t) => {
                out.push_str(&t.text);
                out.push('\n');
            }
            other => {
                out.push_str(&format!("{:?}\n", other));
            }
        }
    }
    if out.is_empty() {
        out.push_str("(empty tool result)");
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_prefix_keeps_names_disjoint() {
        // MCP names must NEVER collide with our message tool names.
        let mcp_name = format!("{MCP_TOOL_PREFIX}read_file");
        assert!(!mcp_name.starts_with("BROADCAST"));
        assert!(!mcp_name.starts_with("recall_topic"));
        assert!(!mcp_name.starts_with("update_scratchpad"));
    }

    #[test]
    fn project_to_llm_tool_strips_arc() {
        // Build a minimal MCP-style schema and confirm projection works.
        let schema: rmcp::model::JsonObject = serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        }))
        .unwrap();
        let tool = project_to_llm_tool(
            "read_file",
            &Some("Read a file.".into()),
            &Arc::new(schema),
        );
        assert_eq!(tool.function.name, "fs__read_file");
        assert_eq!(tool.function.description, "Read a file.");
        assert_eq!(tool.function.parameters["properties"]["path"]["type"], "string");
    }

    fn text_content(s: &str) -> rmcp::model::Content {
        // Uses the public constructor — works regardless of how the
        // non_exhaustive raw struct evolves in future rmcp versions.
        rmcp::model::Content::text(s.to_string())
    }

    #[test]
    fn render_text_result_concatenates_lines() {
        let result = rmcp::model::CallToolResult::success(vec![text_content("hello world")]);
        let rendered = render_tool_result(&result);
        assert_eq!(rendered, "hello world");
    }

    #[test]
    fn render_error_result_prefixed() {
        let result =
            rmcp::model::CallToolResult::error(vec![text_content("path outside allowed dirs")]);
        let rendered = render_tool_result(&result);
        assert!(rendered.starts_with("ERROR:"));
        assert!(rendered.contains("path outside"));
    }

    #[test]
    fn truncate_handles_multibyte() {
        // Naive `&s[..n]` would panic mid-char on 中文; truncate must
        // count chars (code points), not bytes.
        let s = "用户名密码登录API契约前端后端开发实现";
        let out = truncate(s, 4);
        assert_eq!(out, "用户名密…");
    }

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hi", 100), "hi");
    }

    #[test]
    fn make_call_event_flags_error_results() {
        let ev = make_call_event(
            "frontend_dev",
            "fs__write_file",
            r#"{"path":"x"}"#,
            "ERROR: not allowed",
            123,
        );
        assert!(!ev.success);
        assert_eq!(ev.agent, "frontend_dev");
        assert_eq!(ev.tool, "fs__write_file");
        assert_eq!(ev.timestamp, 123);
    }

    #[test]
    fn make_call_event_success_default() {
        let ev = make_call_event(
            "backend_dev",
            "fs__list_directory",
            r#"{"path":"."}"#,
            "[FILE] index.html",
            456,
        );
        assert!(ev.success);
        assert!(ev.id.starts_with("mcpc-"));
    }
}
