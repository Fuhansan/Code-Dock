//! ④ 工具调用系统 — 统一注册表（CLAUDE.md ④「工具调用系统（统一注册表）」）。
//!
//! **统一前门、差异化后端**：模型只看见一张扁平工具目录、自己挑；"协议 / 记忆 /
//! 外部"的区别是"被挑中后怎么处置"的后端差异，不泄到门面。一个 [`ToolSpec`]
//! 注册表，每个能力一处声明 —— 终结"加一个工具改 5 处"。
//!
//! **这是 step 1（骨架）**：定义类型 + 收齐当前工具 + 用角色目录(`RoleConfig.tools`)
//! 驱动广告。范围内**不改工具名、不改执行路径**（renames / 原生 Read/Edit/Bash
//! 留给 step 2/3，安全检查的 `danger`/`Level` 留给 step 4）。所以：
//!   - `Handling::Call` 暂只带 `backend`，`danger` 字段 step 4 再加；
//!   - 动态 fs__ MCP 文件工具仍按过渡授权 token（[`FS_READ_GRANT`]/[`FS_WRITE_GRANT`]）
//!     放行，step 2 上原生文件工具后连同这两个 token 一起删。

use serde::{Deserialize, Serialize};

use crate::agent::fs_tools::{edit_tool, glob_tool, grep_tool, read_tool, write_tool};
use crate::agent::llm_source::set_plan_tool;
use crate::agent::lsp::lsp_tool;
use crate::agent::mcp::McpClient;
use crate::agent::memory::recall_detail_tool;
use crate::agent::protocol::{message_tools, TOOL_ASK_AGENT, TOOL_DONE};
use crate::agent::recall::query_tools;
use crate::agent::scratchpad::scratchpad_tools;
use crate::agent::shell::{bash_output_tool, bash_tool, kill_shell_tool};
use crate::llm::Tool;

/// 角色策略：被挑中的工具调用时如何把关（可配，挂 `RoleConfig`）。它只挪 L1/L2 的
/// "放行↔问人"，碰不到 L3 红线。完整语义在 ④.d 安全检查（step 4）；step 1 只是把
/// 字段立起来取代旧 `mcp_access`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SecurityLevel {
    /// L1 区内改动也要问人。
    Strict,
    /// L1 放行、L2 问人、L3 拒。默认。
    #[default]
    Standard,
    /// L1+L2 放行、L3 仍拒。
    Permissive,
}

/// 执行后端：被挑中的 Call 工具由谁执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// 原生 Rust（shell；step 2+ 的 Read/Edit/Write/Glob/Grep/Bash）。
    Native,
    /// MCP 子进程（当前 `fs__*`；未来 `mcp__server__tool`）。
    Mcp,
    /// agent 私域（recall / scratchpad）。
    Internal,
}

/// 被挑中后循环怎么处置 + 各自需要的数据。**只有 `Call` 类才过安全检查(④.d)+执行器。**
/// 这一格 = 之前散在 `llm_source::translate` + `CompositeExecutor` if-else 的硬编码，
/// 归位成数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handling {
    /// 过安全检查 + 执行器，结果双通道回灌。（`danger` 字段 step 4 再加。）
    Call { backend: Backend },
    /// 改 `TurnState.plan`（write_todos / 现 set_plan）。
    Plan,
    /// 翻成 ③ 消息（BROADCAST/ANSWER/WORK_START/PROGRESS/SUMMARY）。
    Message,
    /// 挂起转 WAITING_ANSWER（ASK_AGENT）。
    Suspend,
    /// 收尾（DONE）。
    End,
}

/// 注册表条目：对模型的统一门面（名字/描述/schema 在 `tool` 里）+ 后端处置。
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub tool: Tool,
    pub handling: Handling,
}

impl ToolSpec {
    pub fn name(&self) -> &str {
        &self.tool.function.name
    }
}

/// 静态原生/内部/控制工具的注册表。MCP 动态工具不在这里（见 [`mcp_specs`]）。
///
/// Step 1 用**当前**工具名（set_plan / recall_topic / … 未改名）——执行路径不变。
pub fn static_registry() -> Vec<ToolSpec> {
    let mut specs = Vec::new();

    // 控制类：消息工具(7)——多数是 Message，ASK_AGENT=Suspend、DONE=End。
    for t in message_tools() {
        let handling = match t.function.name.as_str() {
            TOOL_ASK_AGENT => Handling::Suspend,
            TOOL_DONE => Handling::End,
            _ => Handling::Message,
        };
        specs.push(ToolSpec { tool: t, handling });
    }

    // 控制类：set_plan = Plan（循环状态，不过安全检查）。
    specs.push(ToolSpec {
        tool: set_plan_tool(),
        handling: Handling::Plan,
    });

    // 记忆/查询：Internal Call。
    for t in query_tools() {
        specs.push(ToolSpec {
            tool: t,
            handling: Handling::Call {
                backend: Backend::Internal,
            },
        });
    }
    for t in scratchpad_tools() {
        specs.push(ToolSpec {
            tool: t,
            handling: Handling::Call {
                backend: Backend::Internal,
            },
        });
    }
    specs.push(ToolSpec {
        tool: recall_detail_tool(),
        handling: Handling::Call {
            backend: Backend::Internal,
        },
    });

    // 外部：Bash 三件套（step 3）= Native Call。
    for t in [bash_tool(), bash_output_tool(), kill_shell_tool()] {
        specs.push(ToolSpec {
            tool: t,
            handling: Handling::Call {
                backend: Backend::Native,
            },
        });
    }

    // 外部：原生文件工具 = Native Call（step 2，取代动态 fs__ MCP 文件工具）。
    for t in [
        read_tool(),
        edit_tool(),
        write_tool(),
        glob_tool(),
        grep_tool(),
    ] {
        specs.push(ToolSpec {
            tool: t,
            handling: Handling::Call {
                backend: Backend::Native,
            },
        });
    }

    // 代码语义：LSP = Native Call（L0）。注册但暂不进任何角色目录（P4 接线后加入工程师）。
    specs.push(ToolSpec {
        tool: lsp_tool(),
        handling: Handling::Call {
            backend: Backend::Native,
        },
    });

    specs
}

/// 把一个 MCP 客户端广告的工具投影成 `Call`/`Mcp` 的 ToolSpec。
pub fn mcp_specs(mcp: &McpClient) -> Vec<ToolSpec> {
    mcp.advertised_tools()
        .iter()
        .map(|t| ToolSpec {
            tool: t.clone(),
            handling: Handling::Call {
                backend: Backend::Mcp,
            },
        })
        .collect()
}

/// 按角色目录(`RoleConfig.tools`)从注册表过滤出广告给 LLM 的工具表。
///
/// 统一**逐名**：原生/内部/控制工具来自 [`static_registry`]，MCP 工具来自
/// [`mcp_specs`]（用户 MCP 服务器的 `mcp__server__tool`），两者一视同仁按目录里的
/// 名字过滤。取代旧 `build_all_tools` + `mcp_access` 过滤；MCP 不再特殊。
pub fn advertised_tools(catalog: &[String], mcp: Option<&McpClient>) -> Vec<Tool> {
    let mut specs = static_registry();
    if let Some(client) = mcp {
        specs.extend(mcp_specs(client));
    }
    specs
        .into_iter()
        .filter(|s| catalog.iter().any(|n| n == s.name()))
        .map(|s| s.tool)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(tools: &[Tool]) -> Vec<String> {
        tools.iter().map(|t| t.function.name.clone()).collect()
    }

    #[test]
    fn registry_assigns_correct_handling() {
        let reg = static_registry();
        let find = |n: &str| reg.iter().find(|s| s.name() == n).map(|s| s.handling);

        assert_eq!(find("set_plan"), Some(Handling::Plan));
        assert_eq!(find("DONE"), Some(Handling::End));
        assert_eq!(find("ASK_AGENT"), Some(Handling::Suspend));
        assert_eq!(find("BROADCAST"), Some(Handling::Message));
        assert_eq!(
            find("recall_topic"),
            Some(Handling::Call {
                backend: Backend::Internal
            })
        );
        assert_eq!(
            find("Bash"),
            Some(Handling::Call {
                backend: Backend::Native
            })
        );
        assert_eq!(
            find("Read"),
            Some(Handling::Call {
                backend: Backend::Native
            })
        );
    }

    #[test]
    fn catalog_filters_by_exact_name() {
        // A coordinator-shaped catalog: messages + plan, no shell.
        let catalog: Vec<String> = vec![
            "BROADCAST".into(),
            "ASK_AGENT".into(),
            "set_plan".into(),
            "recall_topic".into(),
        ];
        let got = names(&advertised_tools(&catalog, None));
        assert!(got.contains(&"BROADCAST".to_string()));
        assert!(got.contains(&"set_plan".to_string()));
        assert!(got.contains(&"recall_topic".to_string()));
        // Not in the catalog → not advertised. Structural gating (三层防御 layer 2).
        assert!(!got.contains(&"Bash".to_string()));
        assert!(!got.contains(&"update_scratchpad".to_string()));
    }

    #[test]
    fn bash_appears_only_when_catalogued() {
        let without: Vec<String> = vec!["BROADCAST".into()];
        assert!(!names(&advertised_tools(&without, None)).contains(&"Bash".to_string()));

        let with: Vec<String> = vec!["BROADCAST".into(), "Bash".into()];
        assert!(names(&advertised_tools(&with, None)).contains(&"Bash".to_string()));
    }
}
