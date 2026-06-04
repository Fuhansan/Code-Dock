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

use crate::agent::fs_tools::{TOOL_EDIT, TOOL_GLOB, TOOL_GREP, TOOL_READ, TOOL_WRITE};
use crate::agent::llm_source::TOOL_SET_PLAN;
use crate::agent::lsp::TOOL_LSP;
use crate::agent::memory::TOOL_RECALL_DETAIL;
use crate::agent::message::AgentId;
use crate::agent::protocol::{
    TOOL_ANSWER, TOOL_ASK_AGENT, TOOL_BROADCAST, TOOL_DONE, TOOL_PROGRESS, TOOL_SUMMARY,
    TOOL_WORK_START,
};
use crate::agent::recall::{TOOL_RECALL_TOPIC, TOOL_SEARCH_TOPIC};
use crate::agent::role::{Budget, LoopMode, ModelConfig, RoleConfig};
use crate::agent::security::PermissionRule;
use crate::agent::scratchpad::TOOL_UPDATE_SCRATCHPAD;
use crate::agent::shell::{TOOL_BASH, TOOL_BASH_OUTPUT, TOOL_KILL_SHELL};
use crate::agent::tools::SecurityLevel;

pub const PM_ID: &str = "PM";
pub const FRONTEND_ID: &str = "frontend_dev";
pub const BACKEND_ID: &str = "backend_dev";

/// The shared "house rules" all V0.1 roles share. Per AIDOCK_DESIGN.md the
/// V0.1 baseline keeps every agent on the same model + temperature so a
/// future multi-agent-vs-single-agent A/B isolates the multi-agent
/// mechanism's contribution from any model-tier advantage.
fn shared_rules() -> &'static str {
    "## How a turn works (your inner ReAct loop — read this first)\n\
     A turn is NOT one-and-done. You drive a loop until the work is delivered:\n   \
        1. PLAN — call `set_plan(goal, steps)` first for any non-trivial task: lay out an ordered checklist. Re-call `set_plan` to mark steps done/failed or rewrite the plan as you learn. YOU own the step statuses — the runtime never advances them for you.\n   \
        2. ACT — take ONE tool action toward the current step (write a file with fs__*, recall, update your scratchpad, or message the team).\n   \
        3. OBSERVE — each tool's result comes back to you next step; use it to decide what to do next.\n   \
        4. FINISH — when this turn's work is delivered, emit DONE. To get a teammate's input mid-task, emit ASK_AGENT: you PAUSE until they answer, then resume automatically right where you left off.\n     \
        Crucial: sending a BROADCAST or ANSWER does NOT end your turn. Keep going (more tool calls) until you emit DONE — or ASK_AGENT to wait. A quick reply with nothing left to do is just: send it, then DONE.\n\n\
     ## House rules\n\
     1. Each STEP, call EXACTLY ONE tool — never bare plain text, never multiple tool calls in one step. A turn spans several steps; end it with DONE (or ASK_AGENT to wait on a teammate).\n\
     2. Pick the right tool:\n   \
        - `set_plan` to lay out / update your checklist (see the loop above).\n   \
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
        Query tools return data to you, then the loop continues — they do NOT end your turn. Use them to inform your next step, then keep acting toward DONE.\n\
     9. One SCRATCHPAD tool:\n   \
        - `update_scratchpad(current_focus?, add_tasks?, add_files?, add_decisions?)` — write to your private notes. Only YOU see this; teammates don't. The runtime pins it to your prompt every turn so you don't forget. Use it when you decompose a task, commit to a non-obvious decision, or record a file you touched.\n     \
        Don't update the scratchpad every turn — only when something durable changed. After updating, emit your action tool on the NEXT turn.\n\
    10. FILE tools (if advertised): `Read` (content with line numbers), `Edit` (exact old_string→new_string replacement), `Write` (create/overwrite a file), `Glob` (find files by name pattern), `Grep` (search file contents). These ACTUALLY read/write real files in the workspace. You MUST `Read` a file before you `Edit` it or overwrite it with `Write`. When the team agrees a file should exist, `Write` it instead of pasting content as a BROADCAST. Edits/writes are confined to the workspace (paths outside are refused); reads may range wider. Tool results loop back to you so you can WORK_START / PROGRESS / DONE around the writes."
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

/// 每个角色共有的工具目录（CLAUDE.md ④「工具调用系统」逐名目录）：消息工具 +
/// 计划 + 查询/记忆/scratchpad。fs/shell 由各角色按需追加。
fn common_tool_catalog() -> Vec<String> {
    let mut v = all_message_tools();
    v.push(TOOL_SET_PLAN.into());
    v.push(TOOL_RECALL_TOPIC.into());
    v.push(TOOL_SEARCH_TOPIC.into());
    v.push(TOOL_UPDATE_SCRATCHPAD.into());
    v.push(TOOL_RECALL_DETAIL.into());
    v
}

/// 协调型角色（PM）：共有 + **只读**文件工具（Read/Glob/Grep）。没有 Edit/Write/shell
/// ——三层防御 layer 2 的结构性门控就在这张目录里：写工具从不进它的工具表。
fn coordinator_catalog() -> Vec<String> {
    let mut v = common_tool_catalog();
    v.push(TOOL_READ.into());
    v.push(TOOL_GLOB.into());
    v.push(TOOL_GREP.into());
    v
}

/// 所有角色共有的 specifier 规则：拒读密钥（read 可越界，但密钥不给读——CLAUDE.md
/// ④.d 记过的 CC 坑"别抄它默认不挡密钥"）。
fn secrets_deny_rules() -> Vec<PermissionRule> {
    vec![
        PermissionRule::deny(TOOL_READ, "*/.ssh/*"),
        PermissionRule::deny(TOOL_READ, "*/.aws/*"),
        PermissionRule::deny(TOOL_GREP, "*/.ssh/*"),
    ]
}

/// 工程师的 specifier 规则：拒密钥 + 常用"验证"命令免问 + 明确危险硬拒。
/// allow 是 best-effort 便利（减少"每条命令都问"的痛）；deny 是 best-effort 防御
/// （命令串可绕，不是墙——见 CLAUDE.md ④.d）。
fn engineer_permission_rules() -> Vec<PermissionRule> {
    let mut r = secrets_deny_rules();
    for cmd in [
        "ls*",
        "cat *",
        "pwd*",
        "echo *",
        "git status*",
        "git diff*",
        "git log*",
        "cargo check*",
        "cargo test*",
        "cargo build*",
        "pytest*",
        "npm test*",
        "npm run build*",
    ] {
        r.push(PermissionRule::allow(TOOL_BASH, cmd));
    }
    r.push(PermissionRule::deny(TOOL_BASH, "sudo *"));
    r.push(PermissionRule::deny(TOOL_BASH, "rm -rf /*"));
    r.push(PermissionRule::deny(TOOL_BASH, "rm -rf ~*"));
    r
}

/// 工程型角色：共有 + 全套文件工具（Read/Edit/Write/Glob/Grep）+ shell。
fn engineer_catalog() -> Vec<String> {
    let mut v = common_tool_catalog();
    v.push(TOOL_READ.into());
    v.push(TOOL_EDIT.into());
    v.push(TOOL_WRITE.into());
    v.push(TOOL_GLOB.into());
    v.push(TOOL_GREP.into());
    v.push(TOOL_LSP.into());
    v.push(TOOL_BASH.into());
    v.push(TOOL_BASH_OUTPUT.into());
    v.push(TOOL_KILL_SHELL.into());
    v
}

// ---------- PM ----------

fn pm_system_prompt() -> String {
    let team = shared_team_block(
        "PM",
        &[FRONTEND_ID, BACKEND_ID],
    );
    format!(
        "You are the Product Manager (\"PM\") in AiDock, a multi-agent AI software team.\n\n\
         ## Your identity\n\
         You are a PRODUCT person. You think in user value, requirements, scope, and trade-offs. You do NOT write code, edit files, create files, or run commands. You have READ-ONLY file access — you may open and read the team's files to review and coordinate, but the write/edit/delete tools are not yours (they're not even given to you). Delivering code is the engineers' job; your job is to make sure the right thing gets built by the right person.\n\n\
         ## What you actually do\n\
         - Have a conversation with the customer to clarify what they want.\n\
         - Decompose the customer's intent into clear pieces of work and assign each piece to the right engineer by role id (frontend_dev / backend_dev).\n\
         - Coordinate timing and unblock disagreements between engineers.\n\
         - Recognise when the team has delivered something — and stop the topic with SUMMARY.\n\n\
         ## What you DO NOT do\n\
         - Open files. Write files. Edit files. Move files. Run commands. Inspect directories.\n\
         - Decide a technical implementation detail on the engineer's behalf if the engineer hasn't been asked. (Suggest, don't dictate.)\n\
         - Re-broadcast the requirement after every nudge from the customer. (House rule 5.)\n\
         - Emit SUMMARY just because agreement was reached. Wait until DONE messages have landed. (House rule 6.)\n\n\
         When something needs to be written, built, RUN, or VERIFIED, do NOT do it yourself — you have no code editor and no shell. ASK_AGENT the engineer who owns it (frontend_dev / backend_dev). ESPECIALLY: if the user asks to RUN or VERIFY code (e.g. \"run fib.py and show the output\"), delegate it to an engineer who will ACTUALLY execute it in the sandbox. NEVER 'compute / infer the output by reading the code yourself' — that is a guess, not verification; hand it to an engineer to run for real and report the true output. If the request is unclear, clarify it (BROADCAST a question) before assigning — understand the intent first, don't just relay the literal words.\n\n\
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
        tools: coordinator_catalog(),
        teammates: vec![FRONTEND_ID.into(), BACKEND_ID.into()],
        loop_mode: LoopMode::Single,
        max_history_tokens: Some(32_000),
        // 三层防御 (CLAUDE.md ④「角色能力三层防御」), layer 2 = tool-gating:
        // PM 的只读现在由**目录**决定——`coordinator_catalog()` 给 Read/Glob/Grep
        // (只读)，不给 Edit/Write/shell，于是写/改/删工具从不进它的工具表，*结构上
        // 写不了代码*。身份-only（mcp_access: All + "请别"）已证伪（dev-test
        // 2026-06-02: PM 自己写了 todo.py）。security_level 只管"拿到的怎么把关"。
        security_level: SecurityLevel::Standard,
        permission_rules: secrets_deny_rules(),
    }
}

/// Shared note for engineer roles: they have a sandboxed `shell` tool and
/// should use it to actually run + verify their work. (Layer-1 prompt half of
/// the shell capability; the tool gate + sandbox are layers 2/3.)
fn engineer_shell_note() -> &'static str {
    "## Running & verifying your work\n\
     You have a `Bash` tool — USE IT to actually run what you build and verify it works, don't \
     just write files and assume. Run scripts (`python3 fib.py`), tests, builds, installs. The \
     working directory is the workspace. Foreground commands wait for the result; for \
     long-running processes (e.g. a dev server) pass run_in_background=true — you get a \
     shell_id, poll it with `BashOutput` and stop it with `KillShell`. After writing code, run \
     it; if the output is wrong, fix it before reporting done. Never claim you \"can't run \
     commands\" — you can."
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
         {shell}\n\
         {team}\n\
         {rules}",
        shell = engineer_shell_note(),
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
        tools: engineer_catalog(),
        teammates: vec![PM_ID.into(), BACKEND_ID.into()],
        loop_mode: LoopMode::Single,
        max_history_tokens: Some(32_000),
        // 工程师拿全套文件工具 + shell（`engineer_catalog()`）。security_level
        // 默认标准：区内改动直接落盘、Bash 问人、越界永拒。
        security_level: SecurityLevel::Standard,
        permission_rules: engineer_permission_rules(),
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
         {shell}\n\
         {team}\n\
         {rules}",
        shell = engineer_shell_note(),
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
        tools: engineer_catalog(),
        teammates: vec![PM_ID.into(), FRONTEND_ID.into()],
        loop_mode: LoopMode::Single,
        max_history_tokens: Some(32_000),
        // 同 frontend：工程型角色拿全套文件工具 + shell。
        security_level: SecurityLevel::Standard,
        permission_rules: engineer_permission_rules(),
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
                "every role must inherit the one-tool-per-step rule"
            );
            assert!(role.system_prompt.contains("ASK_AGENT"));
            assert!(role.system_prompt.contains("SUMMARY"));
        }
    }

    #[test]
    fn system_prompts_teach_the_react_loop() {
        // The ReAct turn host (④.a) only pays off if the model knows to plan
        // and that a turn spans many steps ending in DONE.
        for role in default_workshop() {
            assert!(
                role.system_prompt.contains("set_plan"),
                "every role must learn to plan with set_plan"
            );
            assert!(
                role.system_prompt.contains("does NOT end your turn"),
                "every role must learn BROADCAST/ANSWER don't end the turn"
            );
        }
    }
}
