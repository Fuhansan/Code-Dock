//! V0.1 hardcoded workshop: PM + frontend_dev + backend_dev.
//!
//! Sprint 2 deliberately ships *one* workshop wired in code, so the
//! multi-agent protocol can be validated without the Sprint 3 workshop
//! editor on the critical path. Sprint 0.3 swaps this constant for a
//! file-loaded workshop definition; the shape (`Vec<RoleConfig>`) is the
//! same.
//!
//! The system prompts here are the single most important piece of
//! Sprint 2 — they encode the protocol the LLM must follow. The Sprint 1
//! stability harness verified the basic tool-call discipline; these prompts
//! extend that with team awareness and topic discipline.

use crate::agent::message::AgentId;
use crate::agent::protocol::{
    TOOL_ANSWER, TOOL_ASK_AGENT, TOOL_BROADCAST, TOOL_DONE, TOOL_PROGRESS, TOOL_SUMMARY,
    TOOL_WORK_START,
};
use crate::agent::role::{Budget, LoopMode, McpAccess, ModelConfig, RoleConfig};

pub const PM_ID: &str = "PM";
pub const FRONTEND_ID: &str = "frontend_dev";
pub const BACKEND_ID: &str = "backend_dev";

/// The shared "house rules" all V0.1 roles share. Per AIDOCK_DESIGN.md the
/// V0.1 baseline keeps every agent on the same model + temperature so a
/// future multi-agent-vs-single-agent A/B isolates the multi-agent
/// mechanism's contribution from any model-tier advantage.
fn shared_rules() -> &'static str {
    "## House rules\n\
     1. Every turn, call EXACTLY ONE tool. Never plain text. Never multiple tool calls.\n\
     2. Pick the right tool:\n   \
        - ANSWER replies to an ASK_AGENT directed at you (use its message id as reply_to).\n   \
        - ASK_AGENT when you need ONE specific teammate's input.\n   \
        - BROADCAST when the whole team needs to know.\n   \
        - WORK_START / PROGRESS / DONE around substantial tasks (think \"I'm coding the login page now\").\n   \
        - SUMMARY only when the topic is GENUINELY DONE — see rule 6.\n\
     3. BROADCASTs are pickup-optional. If you receive one and have nothing meaningful to add, stay quiet by not being scheduled — don't manufacture a response.\n\
     4. Topic discipline: reuse the current topic_id while a thread is alive. Open a new topic (fresh topic_id + new_topic_title) ONLY when the subject genuinely changes (e.g. from \"requirement gathering\" to \"frontend stack pick\"). Close finished topics with SUMMARY before opening the next one.\n\
     5. Do NOT restate the requirement just because the user confirmed or nudged. If the user says \"sounds good\" or \"continue\", emit a short BROADCAST acknowledgment or move into action — never repeat the plan back at them.\n\
     6. SUMMARY only when ALL of these hold:\n   \
        - Every ASK_AGENT in this topic has an ANSWER\n   \
        - Every WORK_START in this topic has a matching DONE\n   \
        - No teammate is currently WORKING on something tied to this topic\n   \
        - The topic's question(s) are actually resolved, not just acknowledged\n     \
        DO NOT SUMMARY merely because the team agreed on an API contract — the contract is a milestone, not the topic's end. Wait until DONE messages have landed.\n\
     7. Voice: terse and action-oriented. No greetings, apologies, filler, or restating the question.\n\
     8. Two QUERY tools are available alongside the action tools:\n   \
        - `recall_topic(topic_id)` — pull the full message stream of a topic you only see summarised in your context. Use BEFORE acting if you genuinely need the detail.\n   \
        - `search_topic(topic_id, query)` — find matching messages in a topic by substring (case-insensitive).\n     \
        Query tools return data to you and let you act next turn. They are not a substitute for the action tools — every turn must still end with one of BROADCAST / ASK_AGENT / ANSWER / WORK_START / PROGRESS / DONE / SUMMARY.\n\
     9. One SCRATCHPAD tool:\n   \
        - `update_scratchpad(current_focus?, add_tasks?, add_files?, add_decisions?)` — write to your private notes. Only YOU see this; teammates don't. The runtime pins it to your prompt every turn so you don't forget. Use it when you decompose a task, commit to a non-obvious decision, or record a file you touched.\n     \
        Don't update the scratchpad every turn — only when something durable changed. After updating, emit your action tool on the NEXT turn.\n\
    10. FILESYSTEM tools (prefix `fs__`): if these are advertised this session, you can ACTUALLY read and write files in the workspace dir. `fs__write_file` and `fs__edit_file` create real files on disk. When the team agrees a file should exist, emit `fs__write_file` instead of pasting the content as a BROADCAST. Tool results loop back to you so you can WORK_START / PROGRESS / DONE around the writes."
}

fn shared_team_block(self_role: &str, teammates: &[&str]) -> String {
    let mut s = String::from("## Team\n");
    s.push_str(&format!("- {self_role} (YOU)\n"));
    for t in teammates {
        let desc = match *t {
            PM_ID => "Product Manager — owns requirements and customer-facing conversations.",
            FRONTEND_ID => "Frontend developer — implements the user interface.",
            BACKEND_ID => "Backend developer — implements server-side logic and data.",
            _ => "Teammate.",
        };
        s.push_str(&format!("- {t} — {desc}\n"));
    }
    s
}

fn baseline_model() -> ModelConfig {
    ModelConfig {
        provider: "bailian".into(),
        primary: "qwen3.6-plus".into(),
        fallback: None,
        // Lower temperature than chat mode — we want deterministic
        // tool-call discipline, not creative text.
        temperature: 0.3,
        max_tokens: 4096,
        extended_thinking: false,
    }
}

fn baseline_budget() -> Budget {
    Budget {
        per_call_tokens: 8000,
        per_session_tokens: Some(400_000),
        per_session_calls: Some(200),
    }
}

fn all_message_tools() -> Vec<String> {
    vec![
        TOOL_BROADCAST.into(),
        TOOL_ASK_AGENT.into(),
        TOOL_ANSWER.into(),
        TOOL_WORK_START.into(),
        TOOL_PROGRESS.into(),
        TOOL_DONE.into(),
        TOOL_SUMMARY.into(),
    ]
}

// ---------- PM ----------

fn pm_system_prompt() -> String {
    let team = shared_team_block(
        "PM",
        &[FRONTEND_ID, BACKEND_ID],
    );
    format!(
        "You are the Product Manager (\"PM\") in AiDock, a multi-agent AI software team.\n\n\
         Your job:\n\
         - Talk to the customer (the human user) and understand what they want.\n\
         - Translate that into actionable work for the engineers.\n\
         - Coordinate the team and decide when to move on.\n\n\
         You are NOT a coder. Even if filesystem tools are advertised to you, do NOT call `fs__write_file` / `fs__edit_file` yourself — that is the engineers' job. Your contribution is via BROADCAST (clear assignments naming the role) and ASK_AGENT (specific questions). When code needs to land on disk, address the engineer by role id and let them do it.\n\n\
         {team}\n\
         {rules}",
        team = team,
        rules = shared_rules(),
    )
}

pub fn pm_role() -> RoleConfig {
    RoleConfig {
        id: PM_ID.into(),
        display_name: "Product Manager".into(),
        description: "Customer interface, requirements, coordination.".into(),
        system_prompt: pm_system_prompt(),
        model: baseline_model(),
        budget: baseline_budget(),
        tools: all_message_tools(),
        teammates: vec![FRONTEND_ID.into(), BACKEND_ID.into()],
        loop_mode: LoopMode::Single,
        max_history_tokens: Some(32_000),
        // PM coordinates, doesn't code. No filesystem access at all —
        // the model literally won't see fs__ tools advertised, so it
        // can't accidentally write code that's the engineers' job.
        mcp_access: McpAccess::None,
    }
}

// ---------- frontend_dev ----------

fn frontend_system_prompt() -> String {
    let team = shared_team_block(
        FRONTEND_ID,
        &[PM_ID, BACKEND_ID],
    );
    format!(
        "You are the frontend developer (\"frontend_dev\") in AiDock, a multi-agent AI software team.\n\n\
         Your job:\n\
         - Implement the user-facing interface based on the PM's requirements.\n\
         - Coordinate with backend_dev on data shapes and API contracts.\n\
         - Be honest about technical constraints when PM proposes something fragile.\n\n\
         {team}\n\
         {rules}",
        team = team,
        rules = shared_rules(),
    )
}

pub fn frontend_role() -> RoleConfig {
    RoleConfig {
        id: FRONTEND_ID.into(),
        display_name: "Frontend Developer".into(),
        description: "Implements the user interface.".into(),
        system_prompt: frontend_system_prompt(),
        model: baseline_model(),
        budget: baseline_budget(),
        tools: all_message_tools(),
        teammates: vec![PM_ID.into(), BACKEND_ID.into()],
        loop_mode: LoopMode::Single,
        max_history_tokens: Some(32_000),
        // Engineers need full filesystem access — read existing code,
        // write new files, edit, organise.
        mcp_access: McpAccess::All,
    }
}

// ---------- backend_dev ----------

fn backend_system_prompt() -> String {
    let team = shared_team_block(
        BACKEND_ID,
        &[PM_ID, FRONTEND_ID],
    );
    format!(
        "You are the backend developer (\"backend_dev\") in AiDock, a multi-agent AI software team.\n\n\
         Your job:\n\
         - Implement server-side logic, data models, and APIs based on PM's requirements.\n\
         - Coordinate with frontend_dev on API contracts before either side codes against them.\n\
         - Be honest about technical constraints when PM proposes something fragile.\n\n\
         {team}\n\
         {rules}",
        team = team,
        rules = shared_rules(),
    )
}

pub fn backend_role() -> RoleConfig {
    RoleConfig {
        id: BACKEND_ID.into(),
        display_name: "Backend Developer".into(),
        description: "Implements server logic and data.".into(),
        system_prompt: backend_system_prompt(),
        model: baseline_model(),
        budget: baseline_budget(),
        tools: all_message_tools(),
        teammates: vec![PM_ID.into(), FRONTEND_ID.into()],
        loop_mode: LoopMode::Single,
        max_history_tokens: Some(32_000),
        // Same rationale as frontend: engineering roles get full
        // filesystem access.
        mcp_access: McpAccess::All,
    }
}

// ---------- Workshop assembly ----------

/// The V0.1 hardcoded workshop: PM + frontend_dev + backend_dev.
pub fn default_workshop() -> Vec<RoleConfig> {
    vec![pm_role(), frontend_role(), backend_role()]
}

/// Look up a role by id in the default workshop.
pub fn role_by_id(id: &AgentId) -> Option<RoleConfig> {
    default_workshop().into_iter().find(|r| r.id == *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_workshop_has_three_roles() {
        let w = default_workshop();
        assert_eq!(w.len(), 3);
        let ids: Vec<&str> = w.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&PM_ID));
        assert!(ids.contains(&FRONTEND_ID));
        assert!(ids.contains(&BACKEND_ID));
    }

    #[test]
    fn role_by_id_finds_each() {
        assert!(role_by_id(&PM_ID.into()).is_some());
        assert!(role_by_id(&FRONTEND_ID.into()).is_some());
        assert!(role_by_id(&BACKEND_ID.into()).is_some());
        assert!(role_by_id(&"nonexistent".into()).is_none());
    }

    #[test]
    fn all_roles_share_baseline_model() {
        for role in default_workshop() {
            assert_eq!(role.model.provider, "bailian");
            assert_eq!(role.model.primary, "qwen3.6-plus");
            assert!(role.model.temperature < 0.5, "agent runs use low temp");
        }
    }

    #[test]
    fn each_role_lists_the_other_two_as_teammates() {
        let pm = pm_role();
        assert_eq!(pm.teammates.len(), 2);
        assert!(pm.teammates.contains(&FRONTEND_ID.to_string()));
        assert!(pm.teammates.contains(&BACKEND_ID.to_string()));

        let fe = frontend_role();
        assert!(fe.teammates.contains(&PM_ID.to_string()));
        assert!(fe.teammates.contains(&BACKEND_ID.to_string()));
    }

    #[test]
    fn system_prompts_mention_team_layout() {
        // The team block is what teaches the model who else is in the chat.
        // If this slips, agents will hallucinate teammate ids.
        let pm = pm_role();
        assert!(pm.system_prompt.contains(FRONTEND_ID));
        assert!(pm.system_prompt.contains(BACKEND_ID));
        assert!(pm.system_prompt.contains("YOU"));

        let fe = frontend_role();
        assert!(fe.system_prompt.contains(PM_ID));
        assert!(fe.system_prompt.contains(BACKEND_ID));
    }

    #[test]
    fn system_prompts_state_tool_discipline() {
        for role in default_workshop() {
            assert!(
                role.system_prompt.contains("EXACTLY ONE tool"),
                "every role must inherit the one-tool-per-turn rule"
            );
            assert!(role.system_prompt.contains("ASK_AGENT"));
            assert!(role.system_prompt.contains("SUMMARY"));
        }
    }
}
