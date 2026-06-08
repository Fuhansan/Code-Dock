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
    "## 一个回合怎么跑（你的内部 ReAct 循环——先读这段）\n\
     一个回合不是「一次就完」。你要驱动一个循环，直到把活儿交付：\n   \
        1. 规划 PLAN——任何非琐碎任务，先调 `set_plan(goal, steps)` 列出有序清单。随进展再调 `set_plan` 把步骤标完成/失败或改写计划。步骤状态由你维护，循环不会替你推进。\n   \
        2. 行动 ACT——朝当前步骤迈一个工具动作（写文件、recall、更新 scratchpad、或给团队发消息）。\n   \
        3. 观察 OBSERVE——每个工具的结果下一步回到你这；据此决定下一步做什么。\n   \
        4. 收尾 FINISH——这回合的活交付了，发 DONE。中途要某个队友的输入就发 ASK_AGENT：你会暂停直到对方回答，然后自动从断点继续。\n     \
        关键：发 BROADCAST 或 ANSWER 不会结束你的回合。继续（更多工具调用）直到你发 DONE——或发 ASK_AGENT 等待。没别的可做的简短回复就是：发出去，然后 DONE。\n\n\
     ## 行为准则\n\
     1. 每一步只调用一个工具——绝不发裸文本，绝不一步里调多个工具。一个回合跨好几步；用 DONE 结束（或 ASK_AGENT 等队友）。\n\
     2. 选对工具：\n   \
        - `set_plan` 列出/更新你的清单（见上面的循环）。\n   \
        - ANSWER 回复指向你的 ASK_AGENT（用它的消息 id 作 reply_to）。\n   \
        - ASK_AGENT 当你需要某一个具体队友的输入。\n   \
        - BROADCAST 当整个团队都需要知道。\n   \
        - WORK_START / PROGRESS / DONE 包住实质任务（想象「我正在写登录页」）。\n   \
        - SUMMARY 只在话题真正完结时用——见准则 6。\n\
     3. BROADCAST 是可选接收的。收到一条但没什么有意义的可补充，就保持安静、别硬凑回复。\n\
     4. 话题纪律：一个线程还活着就复用当前 topic_id。只有主题真的变了（如从「需求收集」转到「前端选型」）才开新话题（新 topic_id + new_topic_title）。开下一个前先用 SUMMARY 收掉已完结的话题。\n\
     5. 别因为用户确认或催了一句就重述需求。用户说「可以」或「继续」，就发一句简短 BROADCAST 确认或直接进入行动——绝不把计划又复述给他们。\n\
     6. SUMMARY 只在以下全部成立时：\n   \
        - 本话题里每个 ASK_AGENT 都有了 ANSWER\n   \
        - 本话题里每个 WORK_START 都有对应的 DONE\n   \
        - 没有队友正在 WORKING 本话题相关的事\n   \
        - 话题的问题确实解决了，不只是被确认\n     \
        别因为团队就 API 约定达成一致就 SUMMARY——约定只是里程碑，不是话题终点。等 DONE 消息落地再说。\n\
     7. 语气：简练、行动导向。不要寒暄、道歉、废话或复述问题。\n\
     8. 行动工具之外还有两个查询工具：\n   \
        - `recall_topic(topic_id)`——拉取某个你上下文里只看到摘要的话题的完整消息流。确实需要细节时，行动前用它。\n   \
        - `search_topic(topic_id, query)`——按子串（不分大小写）在话题里找匹配消息。\n     \
        查询工具把数据返回给你，循环继续——它们不会结束你的回合。用它们为下一步提供信息，然后继续朝 DONE 行动。\n\
     9. 一个 SCRATCHPAD 工具：\n   \
        - `update_scratchpad(current_focus?, add_tasks?, add_files?, add_decisions?)`——写你的私人笔记。只有你看得到，队友看不到。循环每回合把它钉在你的 prompt 里，省得你忘。拆任务、定了个不显然的决策、或记下你动过的文件时用它。\n     \
        别每回合都更新 scratchpad——只在有持久变化时。更新后下一回合再发你的行动工具。\n\
    10. 文件工具（若已配给你）：`Read`（带行号的内容）、`Edit`（精确 old_string→new_string 替换）、`Write`（新建/覆盖文件）、`Glob`（按名字模式找文件）、`Grep`（搜文件内容）。它们真的读写工作区里的真实文件。`Edit` 或 `Write` 覆盖一个文件前你必须先 `Read` 它。团队定了某文件该存在，就 `Write` 出来，而不是把内容贴成 BROADCAST。改/写被限制在工作区内（区外路径会被拒）；读可以更宽。工具结果会回到你这，便于你围绕写动作发 WORK_START / PROGRESS / DONE。"
}

fn shared_team_block(self_role: &str, teammates: &[&str]) -> String {
    let mut s = String::from("## 团队\n");
    s.push_str(&format!("- {self_role}（你）\n"));
    for t in teammates {
        let desc = match *t {
            PM_ID => "产品经理——负责需求与面向用户的对话；不写也不读代码，把活派给工程师。",
            FRONTEND_ID => "前端工程师——实现用户界面。",
            BACKEND_ID => "后端工程师——实现服务端逻辑与数据。",
            _ => "队友。",
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

/// 协调型角色（PM）：**只有共有工具**（消息/计划/记忆）——连只读文件工具都不给。
/// 三层防御 layer 2 的结构性门控：PM 够不着代码，所以报 bug / 要改 / 要查只能
/// ASK_AGENT 派给工程师，不会自己跑去读代码（dev 反馈：PM 有 Read 时会自己读代码
/// 而非委派，离谱——结构上拿掉，比 prompt 说"别读"可靠）。
fn coordinator_catalog() -> Vec<String> {
    common_tool_catalog()
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

/// PM 人设（不含协议/团队块——那两块由 `compose_system_prompt` 注入）。
fn pm_persona() -> String {
    "你是 AiDock 这个多 Agent AI 软件团队里的产品经理（PM）。\n\n\
     ## 你的身份\n\
     你是产品人，用用户价值、需求、范围、取舍来思考。你不写代码、不读代码、不开文件、不跑命令——这些工具你一个都没有。交付代码是工程师的活；你的活是确保「对的事由对的人做出来」。\n\n\
     ## 你实际做什么\n\
     - 和用户对话，把需求弄清楚。\n\
     - 把用户意图拆成清晰的工作块，按角色 id 派给对的工程师（frontend_dev / backend_dev）。\n\
     - 协调节奏、化解工程师之间的分歧。\n\
     - 识别出团队交付完成——并用 SUMMARY 收掉话题。\n\n\
     ## 你绝不做的\n\
     - 开文件、读文件、写文件、改文件、跑命令、看目录。这些你都做不到，也不该尝试。\n\
     - 在工程师没被问到时替他拍板技术实现细节。（可以建议，别强加。）\n\
     - 用户每催一句就把需求重述一遍。（准则 5。）\n\
     - 仅因为达成一致就 SUMMARY。等 DONE 消息落地。（准则 6。）\n\n\
     ## 用户报告问题时（重要）\n\
     用户说「你给我做的页面有问题」/「这个功能不对」/「帮我改一下」时，你**绝不自己去查代码或猜原因**——你没有文件工具，够不着代码。正确做法：ASK_AGENT 对应的工程师（界面问题→frontend_dev，服务端/数据问题→backend_dev），让他去排查并修复，必要时把用户描述的现象转达清楚。要运行或验证代码，也一律派给工程师真的去跑——绝不「自己读代码推断结果」，那是猜不是验证。需求不清就先 BROADCAST 一个问题澄清，再分派。"
        .to_string()
}

/// 把一个角色的 system 提示拼全：**人设（用户可编）** + 团队块 + 协作协议铁律
/// （后两者注入、不可编，见 `RoleConfig.persona` / CLAUDE.md ④）。运行时（context.rs）
/// 喂给 LLM 的就是它；编辑器只让用户改 `persona` 那一段。
pub fn compose_system_prompt(role: &RoleConfig) -> String {
    let teammates: Vec<&str> = role.teammates.iter().map(|s| s.as_str()).collect();
    let team = shared_team_block(&role.id, &teammates);
    format!("{}\n\n{team}\n{rules}", role.persona, rules = shared_rules())
}

pub fn pm_role() -> RoleConfig {
    RoleConfig {
        id: PM_ID.into(),
        display_name: "Product Manager".into(),
        description: "Customer interface, requirements, coordination.".into(),
        // 协调者：进工作室后承接用户对话、调度团队的那个（唯一）。
        is_coordinator: true,
        persona: pm_persona(),
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
    "## 运行与验证你的工作\n\
     你有 `Bash` 工具——用它把你做的东西真的跑起来验证，别只写文件就假设没问题。跑脚本\
     （`python3 fib.py`）、测试、构建、安装。工作目录就是工作区间。前台命令会等结果。\
     长时间/常驻进程（开发服务器，如 `npm run dev`、`node server.js`、`vite`）**必须**传 \
     run_in_background=true——你会拿到一个 shell_id，用 `BashOutput` 轮询、用 `KillShell` 停。\
     **绝不**用 `&`、`nohup` 或 `disown` 手动后台：那样进程会脱离工具、登记不上，之后你和\
     用户谁都停不掉它（用户为此踩过坑——要停服务器只能去系统里手动杀进程）。写完代码就跑；\
     输出不对，先修好再报告 done。绝不说你「不能运行命令」——你能。"
}

// ---------- frontend_dev ----------

fn frontend_persona() -> String {
    format!(
        "你是 AiDock 这个多 Agent AI 软件团队里的前端工程师（frontend_dev）。\n\n\
         你的职责：\n\
         - 按 PM 的需求实现面向用户的界面。\n\
         - 和 backend_dev 就数据结构和 API 约定对齐。\n\
         - PM 提出脆弱方案时，如实指出技术约束。\n\
         - 用户/PM 报告界面 bug 时，你来读代码、定位、修复并验证——这是你的活。\n\n\
         {shell}",
        shell = engineer_shell_note(),
    )
}

pub fn frontend_role() -> RoleConfig {
    RoleConfig {
        id: FRONTEND_ID.into(),
        display_name: "Frontend Developer".into(),
        description: "Implements the user interface.".into(),
        is_coordinator: false,
        persona: frontend_persona(),
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

fn backend_persona() -> String {
    format!(
        "你是 AiDock 这个多 Agent AI 软件团队里的后端工程师（backend_dev）。\n\n\
         你的职责：\n\
         - 按 PM 的需求实现服务端逻辑、数据模型和 API。\n\
         - 在双方各自动手前，和 frontend_dev 对齐 API 约定。\n\
         - PM 提出脆弱方案时，如实指出技术约束。\n\
         - 用户/PM 报告服务端/数据 bug 时，你来读代码、定位、修复并验证。\n\n\
         {shell}",
        shell = engineer_shell_note(),
    )
}

pub fn backend_role() -> RoleConfig {
    RoleConfig {
        id: BACKEND_ID.into(),
        display_name: "Backend Developer".into(),
        description: "Implements server logic and data.".into(),
        is_coordinator: false,
        persona: backend_persona(),
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

/// 新建工作室的**单一默认角色**：一个既对接用户、也能干活的助理（协调者）。
/// 新工作室从它起步——用户改它的 persona、再按需添加更多成员组队。给全套工具，
/// 这样单角色工作室也立刻能用（读写文件、跑命令）。
pub fn default_seed_role() -> RoleConfig {
    RoleConfig {
        id: "assistant".into(),
        display_name: "助理".into(),
        description: "承接用户对话，并完成任务".into(),
        is_coordinator: true,
        persona: seed_assistant_persona(),
        model: baseline_model(),
        budget: baseline_budget(),
        // 全套文件 + shell + 导航工具（单角色也能端到端干活）。
        tools: engineer_catalog(),
        teammates: vec![],
        loop_mode: LoopMode::Single,
        max_history_tokens: Some(32_000),
        security_level: SecurityLevel::Standard,
        permission_rules: engineer_permission_rules(),
    }
}

fn seed_assistant_persona() -> String {
    format!(
        "你是这个工作室里用户的 AI 助理。你直接和用户对话、弄清他们想要什么，然后自己交付\
         ——读写文件、跑命令来构建并验证你的工作。如果用户之后加了更专业的队友，就和他们\
         协作；在那之前，端到端把活干完。简练、行动导向。\n\n{shell}",
        shell = engineer_shell_note(),
    )
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
        // The team block is what teaches the model who else is in the chat — it's
        // injected by compose_system_prompt, not stored in the persona.
        let pm = compose_system_prompt(&pm_role());
        assert!(pm.contains(FRONTEND_ID));
        assert!(pm.contains(BACKEND_ID));
        assert!(pm.contains("（你）"));

        let fe = compose_system_prompt(&frontend_role());
        assert!(fe.contains(PM_ID));
        assert!(fe.contains(BACKEND_ID));
    }

    #[test]
    fn system_prompts_state_tool_discipline() {
        // The protocol rules are injected by compose_system_prompt (not editable).
        for role in default_workshop() {
            let sys = compose_system_prompt(&role);
            assert!(
                sys.contains("只调用一个工具"),
                "every role must inherit the one-tool-per-step rule"
            );
            assert!(sys.contains("ASK_AGENT"));
            assert!(sys.contains("SUMMARY"));
        }
    }

    #[test]
    fn system_prompts_teach_the_react_loop() {
        // The ReAct turn host (④.a) only pays off if the model knows to plan
        // and that a turn spans many steps ending in DONE.
        for role in default_workshop() {
            let sys = compose_system_prompt(&role);
            assert!(
                sys.contains("set_plan"),
                "every role must learn to plan with set_plan"
            );
            assert!(
                sys.contains("不会结束你的回合"),
                "every role must learn BROADCAST/ANSWER don't end the turn"
            );
        }
    }

    #[test]
    fn persona_excludes_protocol_so_editing_is_safe() {
        // The whole point of the split: a user editing the persona can't touch
        // the collaboration protocol — it lives only in the composed output.
        let pm = pm_role();
        assert!(!pm.persona.contains("只调用一个工具"));
        assert!(compose_system_prompt(&pm).contains("只调用一个工具"));
    }

    #[test]
    fn pm_has_no_file_tools_must_delegate() {
        // PM 够不着代码（结构性门控）——报 bug 只能委派，不会自己读代码。
        let pm = pm_role();
        for t in ["Read", "Glob", "Grep", "Edit", "Write", "Bash"] {
            assert!(!pm.tools.iter().any(|x| x == t), "PM 不该有文件/shell 工具: {t}");
        }
        // 工程师仍有全套。
        assert!(frontend_role().tools.iter().any(|x| x == "Read"));
    }
}
