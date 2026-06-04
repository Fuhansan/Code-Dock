//! ④.d 安全检查 —— Call 工具执行前的关卡（CLAUDE.md ④.d「安全检查」）。
//!
//! 两步，与 CLAUDE.md 一致：
//!   1. **客观危险分级**（引擎算，不可配）：`(工具, 参数, 工作区间) → Level`（L0/L1/L2/L3）。
//!   2. **角色安全级别**（可配）：`(Level, SecurityLevel) → Decision`（放行/问人/拒）。
//!
//! L3（改/删越界）**永拒**，任何安全级别都松不动。"问人"由调用方复用现有
//! `APPROVAL_REQUEST_EVENT` 审批通道（见 `CompositeExecutor`）。
//!
//! 注：这里实现的 `classify_level` 就是设计里 `Handling::Call.danger` 的落地——做成
//! 一个集中函数（按工具名 + 参数 + 工作区间分级）而非每个 spec 挂一个 fn 指针，更简单。
//! 物理 L3 底线另由 `fs_tools::within_workspace` 在文件工具执行处再兜一层（防御纵深）。

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agent::fs_tools::{
    within_workspace, TOOL_EDIT, TOOL_GLOB, TOOL_GREP, TOOL_READ, TOOL_WRITE,
};
use crate::agent::shell::{TOOL_BASH, TOOL_BASH_OUTPUT, TOOL_KILL_SHELL};
use crate::agent::tools::SecurityLevel;

/// 单次调用的客观危险级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// 无害：只读 + 一切内部操作。
    L0,
    /// 区内改动：改/删，路径在工作区间内。
    L1,
    /// 需确认：影响无法静态判定（Bash、MCP、装依赖/联网）。
    L2,
    /// 禁止：改/删，路径越出工作区间。
    L3,
}

/// 安全检查算出来的处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

/// 危险分级要用的上下文。目前只需工作区间根；以后 specifier 规则也从这儿取。
pub struct SecurityCtx {
    pub workspace: std::path::PathBuf,
}

/// 第一步：客观危险分级。`tool` 是工具名，`args` 是 LLM 产生的 JSON 参数。
pub fn classify_level(tool: &str, args: &str, ctx: &SecurityCtx) -> Level {
    match tool {
        // 只读原生工具 —— 可越界。
        TOOL_READ | TOOL_GLOB | TOOL_GREP => Level::L0,
        // 改/删 —— 路径在区内 L1、越界 L3。
        TOOL_EDIT | TOOL_WRITE => mutation_level(args, ctx),
        // 任意命令 —— 影响判不了，需确认。
        TOOL_BASH => Level::L2,
        // 管理已起的后台进程 —— 无害。
        TOOL_BASH_OUTPUT | TOOL_KILL_SHELL => Level::L0,
        // MCP（用户服务器 / 旧 fs__）—— 未知外部影响，需确认。
        _ if tool.starts_with("mcp__") || tool.starts_with("fs__") => Level::L2,
        // 其余 = 内部记忆/查询工具（recall_* / update_scratchpad）—— 无害。
        _ => Level::L0,
    }
}

/// Edit/Write 的级别：解析 `file_path`，区内 L1、越界 L3。解析不出路径时给 L1
/// （让文件工具自己去报"坏参数"并再做一次 confinement 兜底）。
fn mutation_level(args: &str, ctx: &SecurityCtx) -> Level {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return Level::L1;
    };
    let Some(fp) = v.get("file_path").and_then(|x| x.as_str()) else {
        return Level::L1;
    };
    let p = Path::new(fp);
    let target = if p.is_absolute() {
        p.to_path_buf()
    } else {
        ctx.workspace.join(p)
    };
    if within_workspace(&target, &ctx.workspace) {
        Level::L1
    } else {
        Level::L3
    }
}

/// 第二步：按角色安全级别决定处置。**L3 永拒、L0 永放**，安全级别只挪 L1/L2 的
/// "放行↔问人"边界。
pub fn decide(level: Level, sec: SecurityLevel) -> Decision {
    match level {
        Level::L0 => Decision::Allow,
        Level::L3 => Decision::Deny,
        Level::L1 => match sec {
            SecurityLevel::Strict => Decision::Ask,
            _ => Decision::Allow,
        },
        Level::L2 => match sec {
            SecurityLevel::Permissive => Decision::Allow,
            _ => Decision::Ask,
        },
    }
}

// ---------------------------------------------------------------------------
// Specifier rules（CC 的 `ToolName(specifier)` 模式）—— 级别→策略**之前**的一道短路。
// `Deny` 优先（命中即拒），其次 `Allow`（命中即放行、免问），都不命中才落 `decide`。
// 让常用安全命令免打扰、明确危险命令硬拒；也把"读密钥默认拒"做成一条规则。
// 注意（同 CC）：对 Bash 命令串的匹配是 best-effort、可绕（`r''m`、`$()`），不是墙。
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    Allow,
    Deny,
}

/// 一条权限规则：对 `tool`（`"*"`=任意工具）的 specifier 做 glob 匹配。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool: String,
    /// glob：`*`=任意串、`?`=单字符；`"*"` 即"该工具任何调用"。
    pub pattern: String,
    pub action: RuleAction,
}

impl PermissionRule {
    pub fn allow(tool: impl Into<String>, pattern: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            pattern: pattern.into(),
            action: RuleAction::Allow,
        }
    }
    pub fn deny(tool: impl Into<String>, pattern: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            pattern: pattern.into(),
            action: RuleAction::Deny,
        }
    }
}

/// 规则短路结果。`None` = 没规则命中，交给 `decide`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleVerdict {
    Allow,
    Deny,
}

/// 跑一遍规则。Deny 优先于 Allow（更安全）。
pub fn match_rules(tool: &str, args: &str, rules: &[PermissionRule]) -> Option<RuleVerdict> {
    let spec = extract_specifier(tool, args);
    if rules
        .iter()
        .any(|r| r.action == RuleAction::Deny && rule_applies(r, tool, &spec))
    {
        return Some(RuleVerdict::Deny);
    }
    if rules
        .iter()
        .any(|r| r.action == RuleAction::Allow && rule_applies(r, tool, &spec))
    {
        return Some(RuleVerdict::Allow);
    }
    None
}

fn rule_applies(r: &PermissionRule, tool: &str, spec: &str) -> bool {
    (r.tool == "*" || r.tool == tool) && (r.pattern == "*" || wildcard_match(&r.pattern, spec))
}

/// 从参数里抽出该工具的 specifier：Bash 看 `command`、文件工具看路径。
fn extract_specifier(tool: &str, args: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::Value::Null);
    let pick = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
    };
    match tool {
        "Bash" | "BashOutput" | "KillShell" => pick("command").unwrap_or_default(),
        "Read" | "Edit" | "Write" => pick("file_path").unwrap_or_default(),
        "Glob" | "Grep" => pick("path").or_else(|| pick("pattern")).unwrap_or_default(),
        _ => pick("command").or_else(|| pick("file_path")).unwrap_or_default(),
    }
}

/// 平铺 glob：`*`=任意串（含 `/`）、`?`=单字符。回溯匹配，命令串与路径通用。
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SecurityCtx {
        let ws = std::env::temp_dir().join(format!("aidock-sec-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        SecurityCtx { workspace: ws }
    }

    #[test]
    fn levels_by_tool() {
        let c = ctx();
        assert_eq!(classify_level("Read", "{}", &c), Level::L0);
        assert_eq!(classify_level("Glob", "{}", &c), Level::L0);
        assert_eq!(classify_level("Bash", r#"{"command":"ls"}"#, &c), Level::L2);
        assert_eq!(classify_level("BashOutput", "{}", &c), Level::L0);
        assert_eq!(classify_level("recall_topic", "{}", &c), Level::L0);
        assert_eq!(classify_level("mcp__github__create_issue", "{}", &c), Level::L2);
    }

    #[test]
    fn edit_in_workspace_is_l1_outside_is_l3() {
        let c = ctx();
        assert_eq!(
            classify_level("Write", r#"{"file_path":"sub/a.txt","content":"x"}"#, &c),
            Level::L1
        );
        assert_eq!(
            classify_level("Edit", r#"{"file_path":"../escape.txt","old_string":"a","new_string":"b"}"#, &c),
            Level::L3
        );
        assert_eq!(
            classify_level("Write", r#"{"file_path":"/etc/passwd","content":"x"}"#, &c),
            Level::L3
        );
    }

    #[test]
    fn decisions_respect_level_and_security() {
        use Decision::*;
        // L0 always allowed, L3 always denied — regardless of security level.
        for s in [SecurityLevel::Strict, SecurityLevel::Standard, SecurityLevel::Permissive] {
            assert_eq!(decide(Level::L0, s), Allow);
            assert_eq!(decide(Level::L3, s), Deny);
        }
        // L1: strict asks, others allow.
        assert_eq!(decide(Level::L1, SecurityLevel::Strict), Ask);
        assert_eq!(decide(Level::L1, SecurityLevel::Standard), Allow);
        assert_eq!(decide(Level::L1, SecurityLevel::Permissive), Allow);
        // L2: permissive allows, others ask.
        assert_eq!(decide(Level::L2, SecurityLevel::Standard), Ask);
        assert_eq!(decide(Level::L2, SecurityLevel::Strict), Ask);
        assert_eq!(decide(Level::L2, SecurityLevel::Permissive), Allow);
    }

    #[test]
    fn wildcard_matches() {
        assert!(wildcard_match("npm *", "npm install"));
        assert!(wildcard_match("npm test*", "npm test -- --watch"));
        assert!(!wildcard_match("npm *", "pnpm install"));
        assert!(wildcard_match("*/.ssh/*", "/Users/x/.ssh/id_rsa"));
        assert!(wildcard_match("/src/**", "/src/a/b.rs"));
        assert!(wildcard_match("*", "anything at all"));
    }

    #[test]
    fn rules_deny_takes_precedence_and_allow_short_circuits() {
        let rules = vec![
            PermissionRule::allow("Bash", "npm *"),
            PermissionRule::deny("Bash", "npm publish*"),
            PermissionRule::deny("Read", "*/.ssh/*"),
        ];
        // Allow rule matches → Allow.
        assert_eq!(
            match_rules("Bash", r#"{"command":"npm install"}"#, &rules),
            Some(RuleVerdict::Allow)
        );
        // Both allow (npm *) and deny (npm publish*) match → Deny wins.
        assert_eq!(
            match_rules("Bash", r#"{"command":"npm publish --tag x"}"#, &rules),
            Some(RuleVerdict::Deny)
        );
        // Secrets read denied via path specifier.
        assert_eq!(
            match_rules("Read", r#"{"file_path":"/Users/x/.ssh/id_rsa"}"#, &rules),
            Some(RuleVerdict::Deny)
        );
        // No rule matches → fall through to policy.
        assert_eq!(
            match_rules("Bash", r#"{"command":"cargo build"}"#, &rules),
            None
        );
        // Tool name must match: a Bash rule doesn't gate a Read.
        assert_eq!(
            match_rules("Read", r#"{"file_path":"src/a.rs"}"#, &rules),
            None
        );
    }
}
