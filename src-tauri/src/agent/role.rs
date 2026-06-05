//! Role configuration — the multi-dimensional definition of an Agent.
//!
//! Per AIDOCK_DESIGN.md §5, a Role is NOT just a system prompt. It bundles
//! model choice, sampling, tool grants, context strategy, work-loop mode, and
//! token budget. For V0.1 only the fields the runtime actually consumes are
//! populated; the rest are reserved (`Option`) so future sprints can fill
//! them without reshaping the type.

use serde::{Deserialize, Serialize};

use crate::agent::message::AgentId;
use crate::agent::security::PermissionRule;
use crate::agent::tools::SecurityLevel;

/// Which provider+model pair this role uses, plus sampling knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Provider id matching an `LLMProvider::provider_id()`. V0.1: `"bailian"`.
    pub provider: String,
    pub primary: String,
    /// Optional fallback model on the same provider (Sprint 3+ may use it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    pub temperature: f32,
    pub max_tokens: u32,
    /// Reserved for `qwen3-vl-*` / extended-thinking models. V0.1: false.
    #[serde(default)]
    pub extended_thinking: bool,
}

/// Per-call & per-session budget caps. Cheap protection against runaway agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    pub per_call_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_session_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_session_calls: Option<u32>,
}

/// Loop mode the runtime should drive this Agent with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopMode {
    /// One LLM call → emit messages → done. V0.1 default.
    Single,
    /// Think → Act → Observe → repeat (Sprint 2.4+).
    React,
    /// Plan up front, then execute steps without re-planning (Sprint 3+).
    PlanExecute,
}

impl Default for LoopMode {
    fn default() -> Self {
        LoopMode::Single
    }
}

// NOTE: 旧 `McpAccess`（None/ReadOnly/All）已删除。它把"给哪些工具"和"只读与否"
// 揉成一个枚举；现在拆成 `RoleConfig.tools`（目录，逐名）+ `tools::SecurityLevel`
// （策略）。三层防御 layer 2 的结构性门控 = 工具不在目录里就 advertise 不出去
// （见 `tools::advertised_tools`）。

/// Full role definition. The runtime reads this; the workshop editor (Sprint
/// V0.3) will eventually write it. For V0.1 these are hardcoded in 2.2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleConfig {
    /// Stable id — `"PM"`, `"frontend_dev"`, `"backend_dev"`, etc.
    pub id: AgentId,
    /// Human-readable display name shown in the UI.
    pub display_name: String,
    /// Short, one-line description of responsibilities.
    pub description: String,

    /// The **coordinator**: the single activated role that fronts the user — it
    /// (and only it) picks up the user's messages and dispatches the rest of the
    /// team. A workshop has many members but exactly one coordinator (V0.1 = PM).
    /// Replaces the old hardcoded `id == PM_ID` check so a future workshop can
    /// name a different front-desk role without touching the runtime.
    #[serde(default)]
    pub is_coordinator: bool,

    /// The role's **persona** — its identity, responsibilities, voice. This is
    /// the user-editable part of the prompt. The multi-agent **protocol rules**
    /// (`shared_rules()`) and the **team block** are NOT stored here; they are
    /// injected at context-assembly time by `roles::compose_system_prompt`, so a
    /// user editing a persona can never break the collaboration protocol
    /// (CLAUDE.md ④ layer 1 — identity stays clean). May be long; must not embed
    /// runtime state (that goes via context injection).
    pub persona: String,

    pub model: ModelConfig,
    pub budget: Budget,

    /// Allowlist of tool names the Agent may call (Sprint 4 wires actual MCP
    /// tools; for now these are message kinds + recall tools).
    pub tools: Vec<String>,

    /// Other roles this Agent is expected to collaborate with frequently.
    /// Surfaces in the system prompt so the model knows the team layout.
    #[serde(default)]
    pub teammates: Vec<AgentId>,

    #[serde(default)]
    pub loop_mode: LoopMode,

    /// Reserved for Sprint 2.5 context-builder caps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_history_tokens: Option<u32>,

    /// 调用时把关的策略（CLAUDE.md ④.d 安全检查）：严格/标准/宽松，只挪 L1/L2 的
    /// "放行↔问人"，碰不到 L3 红线。取代旧 `mcp_access`——"能拿到哪些工具"现在由
    /// 上面的 `tools` 目录决定，这里只管"拿到的怎么把关"。
    #[serde(default)]
    pub security_level: SecurityLevel,

    /// specifier 权限规则（CLAUDE.md ④.d）：如 `Bash(npm test*)` 免问、
    /// `Read(*/.ssh/*)` 拒。级别策略之前短路（Deny 优先 / Allow 免问）。默认空。
    #[serde(default)]
    pub permission_rules: Vec<PermissionRule>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_mode_default_is_single() {
        assert_eq!(LoopMode::default(), LoopMode::Single);
    }

    #[test]
    fn role_config_serializes_with_default_loop_mode() {
        let r = RoleConfig {
            id: "PM".into(),
            display_name: "Product Manager".into(),
            description: "x".into(),
            is_coordinator: true,
            persona: "you are PM".into(),
            model: ModelConfig {
                provider: "bailian".into(),
                primary: "qwen3.6-plus".into(),
                fallback: None,
                temperature: 0.5,
                max_tokens: 4096,
                extended_thinking: false,
            },
            budget: Budget {
                per_call_tokens: 8000,
                per_session_tokens: None,
                per_session_calls: None,
            },
            tools: vec!["BROADCAST".into(), "ASK_AGENT".into()],
            teammates: vec!["frontend_dev".into()],
            loop_mode: LoopMode::default(),
            max_history_tokens: None,
            security_level: SecurityLevel::default(),
            permission_rules: vec![],
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["id"], "PM");
        assert_eq!(v["model"]["provider"], "bailian");
        assert_eq!(v["loop_mode"], "single");
        // Default security_level is Standard.
        assert_eq!(v["security_level"], "standard");
    }

    #[test]
    fn security_level_default_is_standard() {
        assert_eq!(SecurityLevel::default(), SecurityLevel::Standard);
    }
}
