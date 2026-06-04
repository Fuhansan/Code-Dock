# AiDock — Responsibility Blocks

This file defines the **ownership boundaries** between the layers of the
codebase. Before making any change, locate which block you're in. A change
inside one block must not bleed into another block — that is the rule.

If a request seems to require touching two blocks, stop and call it out
explicitly; usually the right answer is to fix the contract between them
(block ②) rather than entangle them.

---

## ① UI / 渲染
- **Files**: `src/lib/components/*.svelte`, `src/routes/**`, `src/lib/*.ts` *except* `ipc.ts`
- **Owns**: how events / history are laid out, card summaries, expand/collapse, input affordances
- **Does NOT own**: agent decisions, LLM context, what gets persisted
- **Rule**: read-only consumer of events from block ②. Never reach past ② to change backend behavior.

## ② IPC 契约
- **Files**: `src/lib/ipc.ts` ↔ `src-tauri/src/commands.rs`, Tauri event names/payloads (`MESSAGE_EVENT`, `TOOL_CALL_EVENT`〔原 `MCP_CALL_EVENT`；step 5 泛化成所有 Call 工具〕, `APPROVAL_REQUEST_EVENT`)
- **Owns**: command names, payload schemas, the wire types between front and back
- **The narrow waist**: this is the *only* surface that crosses ① ↔ ③/④. Changing a schema here means both sides must update in lockstep.

## ③ Agent 协作运行时
- **Files**: `src-tauri/src/agent/runtime.rs` (dispatcher loop), `router.rs`, `message.rs`, `session.rs`, `persistence.rs`
- **Owns**: multi-agent scheduling, state machine (IDLE / WORKING / WAITING_ANSWER), message routing, turn-taking, who speaks when
- **Does NOT own**: how a single agent "thinks" inside one turn, what its prompt looks like, which tools it has

## ④ 单 Agent 内部系统
- **Files**: `src-tauri/src/agent/roles.rs`, `context.rs`, `scratchpad.rs`, `approval.rs`, auxiliary tools (recall/search/scratchpad)
- **Owns**: identity & system prompt, tool catalog, memory, LLM context assembly, approval gating, **the per-turn reasoning loop**
- **Does NOT own**: cross-agent protocol (lives in ③), UI rendering (lives in ①)

### 角色能力 = 三层防御（缺一不可）

一个角色"能干什么/不能干什么"靠**三层叠加**约束，不是单点。**身份自觉单独用不可靠**（dev-test 2026-06-02 证伪：PM 被要求"别写代码"却自己写了 todo.py）。

1. **提示层（身份）**：system_prompt 把角色说清楚——是什么、能干什么、**绝不能干什么**。让 agent "懂"边界。要求身份**稳在上下文里、不被历史污染**（system prompt 永远在 context 最前；④.b 压缩记忆 + 会话边界帮它不被旧消息带歪）。
2. **工具门控层（结构）**：身份不算数时，**结构上够不着**。`RoleConfig.mcp_access`（None/ReadOnly/All）在 `build_all_tools` 里 BEFORE-advertise 过滤——模型看不到的工具就调不了。例：PM = `ReadOnly`，写/改/删工具从不进它的工具表，**物理上没法写代码**。
3. **不可违背的硬约束层（工作区间 confinement）**：无论怎么命令都**物理做不到**。进工作室时选定一个**可信工作区间**；改/删类工具的目标路径经 `canonicalize`（解 `..` + 软链）后必须落在区间内，否则拒（详见 ④.d 安全检查的 L3 红线）。对**结构化文件工具**（Edit/Write/删除）这是铁的——路径是参数，校验跑不掉；对 **Bash** 是尽力而为（命令串无法静态判定，靠安全级别/规则把关）。**这层 prompt 改不动**——这正是"我让它删系统文件它哪天真删了"这类担忧的最终答案。
> **重大转向（2026-06-03）**：原"OS 沙箱（Seatbelt/ASRT）"方案**整个废弃**。本地桌面、用户在场、威胁是"agent 犯错"而非"恶意攻击"，可信工作区间 + 路径校验即足够，且对齐 CC 默认（CC 默认不开 OS 沙箱，底线靠权限系统）。详见 ④.d。

> 推论：危险/不可逆能力（写盘、shell、删除）的开放，必须同时过 2、3 层，不能只靠第 1 层。

### ④ 工具调用系统（统一注册表）—— 2026-06-03 重定义

> **重定义**：原"MCP 文件服务器（`fs__*`）+ `shell`"换成 **CC 对齐的原生工具族 + MCP 降级为普通工具来源**。
> **现状**：✅ **step 1**（`agent/tools.rs`：`ToolSpec`/`Handling`/`Backend`/`SecurityLevel` + `static_registry`/`mcp_specs`/`advertised_tools`；`RoleConfig` 拆 `tools`(逐名目录)+`security_level`，`mcp_access` 删；`runtime::build_all_tools` 退役）。✅ **step 2**（`agent/fs_tools.rs`：原生 Read/Edit/Write/Glob/Grep，`Backend::Native`，经 `CompositeExecutor` 路由；read-before-edit 读状态；改/删经 `mutation_in_workspace` 锁工作区间 L3；过渡 token 已删，目录改逐名；MCP 走 `advertised_tools` 统一逐名）。✅ **step 3**（`agent/shell.rs` 重写：`shell`→`Bash`，**去 OS 沙箱**——`Seatbelt/AsrtSandbox` 删，`tokio::process` 直跑 cwd=工作区间；前台超时 + 输出超量落盘只回预览+路径；后台三件套 `Bash(run_in_background)`/`BashOutput`/`KillShell` + per-agent 进程登记表 `BashRegistry`；engineer 目录 + system prompt 同步）。✅ **step 4**（`agent/security.rs`：危险分级 L0-L3 + `decide`；`CompositeExecutor::execute` 在文件改/删 + Bash 前过安全检查——L3 拒、L2/严格-L1 复用 `APPROVAL_REQUEST_EVENT` 问人、否则放行；Bash 真空窗关闭）。✅ **step 5**（事件泛化：`McpCallEvent`→`ToolCallEvent`、`MCP_CALL_EVENT`→`TOOL_CALL_EVENT`(值 `aidock:tool_call`)，前后端锁步；`McpExecutor::emit` + `CompositeExecutor` 给**每个原生 Call 工具**发统一事件 → UI 现在能看到文件/Bash/recall 调用卡，不再只有 MCP；被拒的尝试也发卡）。✅ **MCP 审批收口**（`McpExecutor` 退成 `call_backend`——纯后端调用、无审批无事件；`mcp__`/`fs__` 经 `is_mcp_tool` 走 `CompositeExecutor` 的**同一道**安全检查 L2→问人 + 同一处 emit。`McpExecutor::execute` 仅余其单测用）。✅ **清理空转 fs__ 服务器**（`session.rs` 不再 `spawn` `@modelcontextprotocol/server-filesystem`；`mcp = None`，MCP 管道保留给将来用户自配 `mcp__` 服务器）。✅ **specifier 规则**（`PermissionRule` + `match_rules`，挂 `RoleConfig.permission_rules`；默认拒读密钥 + 工程师验证命令免问 + 危险硬拒）。⬜ 待做：前端 `McpCallEvent` 接口名 + `load_mcp_call_history`/`mcp_calls.jsonl` 内部名（纯命名）、LSP（独立立项）。

**统一前门、差异化后端**：模型只看见**一张扁平工具目录**、自己挑；"协议 / 记忆 / 外部"的区别是"被挑中后怎么处置"的后端差异，**不泄到门面**。一个 **`ToolSpec` 注册表**，每个能力一处声明：
```
ToolSpec { name, description, schema, handling }
enum Handling {
    Call { backend, danger },    // 唯一过安全检查(④.d) + 执行器的一类
    Plan, Message, Suspend, End  // 控制类，循环内联，不碰安全检查
}
enum Backend { Native(Rust), Mcp(server), Internal }
```
`handling` 这一格 = 之前散在 `llm_source::translate` + `CompositeExecutor` if-else 的硬编码，归位成数据。**推论：只有 Call 类才可能危险**——write_todos/DONE/ASK 等控制类天然到不了安全检查，安全机器永远只裹 Call 族。

**效果类别 → 成员**：

| Handling | 成员 | 处置 |
|---|---|---|
| Call | Read/Edit/Write/Glob/Grep/Bash/LSP、mem_recall、topic_recall/topic_browse、`mcp__*` | 过安全检查(④.d) + 执行器，结果双通道回灌 |
| Plan | write_todos | 改 `TurnState.plan` |
| Message | BROADCAST/ANSWER/WORK_START/PROGRESS/SUMMARY | 翻成 ③ 消息 |
| Suspend | ASK_AGENT | 挂起转 WAITING_ANSWER |
| End | DONE | 收尾 |

**工具改名 + 前缀即命名空间**（模型扫前缀就知道碰哪一层）：

| 新名 | 旧名 | 前缀/族 |
|---|---|---|
| `write_todos` | set_plan | 控制（无前缀） |
| `mem_recall` | recall_detail | `mem_` = ④.b 私域记忆 |
| `topic_recall` | recall_topic | `topic_` = ③ 协作历史（整段拉全） |
| `topic_browse` | search_topic | `topic_` = ③ 协作历史（带 query 过滤） |
| `mcp__server__tool` | `fs__*`（MCP 文件服务器） | MCP 不再特殊，同走权限/事件/双通道 |

> 原生工具用 CC 的 PascalCase 名（Read/Edit/Bash…），原生内部用 snake_case + 族前缀。`fs__` 双下划线是 MCP 强加的；原生工具单下划线。

**外部工具族（CC 对齐，原生 Rust）**：
- **Read / Glob / Grep** — 只读，可越界（L0）；Read 带行号、大文件 offset/limit，读成功登记 `read_files`（喂 read-before-edit）。
- **Edit** — `old_string → new_string` 精确替换 + **read-before-edit**（没读过/已变更则拒）；old 找不到/不唯一报错。
- **Write** — 覆盖**已存在**文件要先 Read（防盲覆盖），新文件免；**自动 `create_dir_all` 父目录**。
- **Bash** — **无沙箱**，`tokio::process`，cwd=workspace；超时（默认 120s/上限 600s）；**输出截断落盘**（全量→⑥、预览+路径→LLM，双通道）；后台三件套 `run_in_background` + `BashOutput` + `KillShell` + 进程登记表（对齐 CC）。
- **LSP** — 见下（路2 自管轻 manager）。

**LSP（路2：自管轻 manager，不照搬 CC）**：CC 的 LSP 要么白嫖宿主 IDE 的语言服务器、要么 opt-in 插件自管；AiDock 无宿主 IDE，路子 = **一次性写通用 LSP 客户端**（`lsp-types` + stdio JSON-RPC，非从零造协议）接**用户机器装的**语言服务器，**语言→server 做成配置驱动**（社区可扩 = 你们版"插件市场"）。
- 服务器映射：rust-analyzer / pyright / gopls / clangd（C+C++）/ jdtls（Java，重）/ omnisharp（C#）/ **volar（Vue）** / **typescript-language-server（含 React、Electron——它俩不是语言，就是 JS/TS）**。
- **降级链**：装了→启它管它；没装→grep / tree-sitter 顶 + **提醒用户装**（走 ②）。诊断另走 Bash 跑 `cargo check`/`tsc`/`pyright`（一次性、不用常驻 manager）。
- 客户端写一次，加语言 = 加配置行（+ per-server 怪癖长尾）。先接 Rust/TS/Python/Go，其余按需。
- **MVP 细化**：先用 `serde_json::Value` 直接拼/读协议、暂不引 `lsp-types`（4 操作形状简单、省重依赖）；server 池**会话级共享**（rust-analyzer 启动要索引整个工程，必须长驻跨 agent/回合复用，类比 `McpClient`）；文档同步先 didOpen-on-query；v1 操作 = definition / references / diagnostics / hover；诊断无 server 时回落 Bash 检查器。

##### LSP 实施进度（分阶段）
- ✅ **P0**：`lsp.rs` 工具定义（`LSP{operation,file_path,line?,character?}`，L0）+ 执行器路由；占位回落"用 Grep / 跑 cargo check"引导；**暂不进角色目录**（P4 接线后加入工程师）。
- ✅ **P2**：`lsp_client.rs` 客户端内核——Content-Length 分帧 + 请求↔响应关联 + 通知(publishDiagnostics)收集 + 服务器反向请求回 null。泛型于 reader/writer，**`tokio::io::duplex` 假服务器全测**（不依赖装 rust-analyzer）。
- ✅ **P3**：`lsp_pool.rs` 会话级 `LspPool`——`ServerSpec` + `server_for_path`(rust-analyzer/ts-language-server/pyright/gopls，.ts/.js 共用一个)+ `binary_on_path` PATH 探测 + `path_to_uri` + 懒启动(start_lock 防并发双启)+ initialize/initialized 握手 + `kill_on_drop` 生命周期。配置/探测/uri/缺二进制→装提示 全单测；真 spawn 段靠 dev 机 smoke。
- ✅ **P4**：`lsp.rs` 4 操作接进 `LSP` 工具——`execute_lsp(pool,args)`：`server_for_path`→**参数校验(位置 fail-fast，起 server 前)**→`get_or_start`→didOpen→请求→`format_locations/hover/diagnostics`(响应→`path:line:col`+片段，1-based↔0-based 转换；诊断 didOpen 后轮询 publishDiagnostics)。`LspPool` 经 `session→AgentBoot→runtime→CompositeExecutor` 注入(会话级共享)；`LSP` 进工程师目录(此时才广告)。纯格式化 + run_operation(duplex 假 client) 全测。
- ✅ **P5**：降级——未配语言 / server 没装 → `degraded()` 回落"Grep 导航 + `cargo check`/`tsc`/`pyright` via Bash + 装 server"引导（`success:true` 信息态、不触发 backstop；经统一 `TOOL_CALL_EVENT` 也到 UI，省了单独的 ② 装提示事件）。
- ✅ **dev-smoke 已验通**（`examples/lsp_smoke.rs`，`cargo run --example lsp_smoke`，需 `rustup component add rust-analyzer`）：对真 rust-analyzer 端到端 4 操作全绿——definition→`lib.rs:1:8`、references→两处调用+声明、hover→`pub fn greet(name:&str)->String`、diagnostics→`expected u8, found String`。**smoke 逮到并修了 3 个真 bug**：① server 退出时请求干等 30s → reader 收 EOF 清空 `pending` 秒回（`request_fails_fast_when_server_disconnects` 守）；② 查询早于索引完成返回空 → opt-in `experimental/serverStatus`，池握手后 `wait_until_ready` 再交出 client；③ rootUri 未 canonicalize 而文件 uri canonicalize（macOS /tmp↔/private/tmp）→ server 认文件不属工程返回空 → `LspPool::new` 统一 canonicalize 工作区。
- ⬜ **P6（延后）**：改完文件自动诊断、全量 didChange、tree-sitter 导航中间档、更多语言、initialize 握手 dev 验/加固。

**`RoleConfig` 拆字段**（`mcp_access` 揉了"目录"和"只读"两件事，拆开）：`tools: Vec<String>`（目录，**逐名**）+ `security_level`（严格/标准/宽松，见 ④.d）。`build_all_tools` 退化成 `registry().filter(|t| role.tools.contains(t.name))`。PM 的"只读"本质由**目录**决定（够不着写工具），不是安全级别。

**注册表落盘**：`agent/tools/` 一工具一文件、自带 `ToolSpec`；`registry()` 启动收齐。终结"加一个工具改 5 处"。

**② 契约变动**：`MCP_CALL_EVENT` 泛化成通用 `TOOL_CALL_EVENT`（"MCP 不特殊"的必然）；① 渲染从"MCP 调用卡"泛化成"工具调用卡"。双通道规则不变（全量→⑥/UI、摘要→LLM）。①/② 锁步改。

### ④ 子模块拆分

V0.1 把整个 ④ 当成一个"reflex"实现：一条入消息 → 一次 LLM call → 一个出动作。下个阶段把它拆成四个**可独立设计的子模块**。任何 ④ 的改动都该明确说自己改的是哪个子模块；跨子模块的改动要先调它们之间的接口、再分别改。

#### ④.a ReAct 计划循环
- **管**: 单 agent 一回合的内部状态机，把"接消息 → 直接出动作"改成"plan → act → 观察 → 必要时再 plan → ..."
- **不管**: 上下文是哪里来的（④.b 管）、动作的结果是否符合预期（④.c 管）、危险动作的安全校验（④.d 管）
- **接口**:
  - 入：incoming `AgentMessage` + ④.b 给的 context bundle
  - 出：next action（发消息 / 调工具 / 等待 / 结束回合），可循环
- **现状**: ✅ 已落地（#1+#2）。`react.rs`(纯循环) + `llm_source.rs`(ActionSource/⑤) + `mcp_executor.rs`(ActionExecutor/④.d 钩子 + CompositeExecutor 路由 mcp/query/scratchpad) + `turn_bridge.rs`(③ 边界翻译)。`runtime.rs::run_agent` 已从退化 loop 改成 ReAct 回合宿主（classify_incoming + 挂起/恢复 + 待办队列）。**待验**：端到端（真 LLM+MCP）尚需 dev server 跑一遍；进度事件(②)仍用 NullSink，留给 #3

##### 数据模型（一回合里存的东西）

一条消息进来到交出回合 = 一个 **Turn**（信封，装这一回合所有思考/动作留痕）。里面：

- **Plan**：`总目标 + 一串有序步骤`，**完整清单**（不是"只存下一步"）。每个步骤自带状态：`待办 / 进行中 / 完成 / 失败`。一个步骤完成 ≈ 一个**小主题**闭合（跟 ④.b L1 对齐，归纳 agent 正好有料可灌）。计划可被改写。
- **Action**：当前步骤落成的一个动作，归四类——调工具 / 对外发消息（含 ASK_AGENT）/ 等待 / 结束回合。跟 ② 的消息类型对得上。
- **Observation**：动作结果 + agent 对结果的判断。判断那一格是 **④.c 的预留位**，MVP 阶段恒为"通过"。

##### 循环形状：一条连续循环，**不是两段式流水线**

> 对标 Claude Code 后修正过来的（见下方「对标依据」）。早期设计成"先单独一次 call 列计划、再逐步一次次 call 执行"，太死。

实际形状是**一条连续的 agent 循环**（plan 和 act 揉在一起，不拆成独立两次 LLM call）：

```
消息进来 → 找 ④.b 要上下文
  ── 循环 ──
  · 模型这一回合：看「上下文 + 当前 Plan + 走到哪」→ 吐出下一个动作
    （第一圈通常先吐出 Plan 清单本身——像调个 TaskCreate；之后各圈执行步骤）
  · 执行动作 → 拿到结果（Observation）→ 塞回对话
  · 模型据此继续：下一步 / 改写清单 / 收尾
  ── 直到模型自己判断干完，发 DONE 交回合 ──
```

要点：**Plan 清单是循环内模型自己维护的产物**，不是循环外先列好、再拿来逐条死驱动的东西。保留完整清单的理由也变了——不是为了驱动循环，而是 ① 对齐 ④.b 的「小主题」② 给一个人肉之外的进度锚点。

> **谁推进步骤：模型，不是循环。** 步骤状态（pending/in_progress/done/failed）由模型重发 `SetPlan` 维护；循环**绝不**自动把 ToolCall 成功当成"某步完成"——一步可能要多次工具调用，"一次调用=一步"不成立。循环只管 act/observe + 兜底 + 收尾，步骤状态只拿来**显示进度**、不强制顺序。（实现见 `agent/react.rs`。）

##### 失败重试：模型决定 + **一道硬底线**

失败处理**主逻辑学 Claude Code**：工具出错 → 错误当结果塞回 → 模型自己决定换法 / 换工具 / 放弃。**但必须自带一道硬底线**：**连续 ToolCall 失败 ≥ K 次**（成功或重排即清零，measures「卡住」非「曾失败」），或整个循环超过 N 圈 → 强制停，escalate 给用户 / PM。按"连续失败"而非"按步计数"，因为前者不依赖模型正确维护 current_step，更稳。

> **为什么硬底线在我们这是必需、在 Claude Code 可有可无**：Claude Code 有人肉刹车（用户随时按 Esc、计划手动批准）；AiDock 是多 agent 半自动、互相对话、用户不盯每回合，**没人按 Esc**，所以跑飞必须靠框架自己兜。K / N 阈值 = **TBD**，先 hand-wave 跑起来再校准。

> **TODO（后续加强）**：重排时机从"只在失败时"（B）演进到"顺利但跑偏也能察觉"（A/C）。这种检测本就是 ④.c 的活；等 ④.c 上线，它给的 `retry / escalate` 信号即"触发重排"的第二个入口。不阻塞 MVP。

##### 对标依据（Claude Code 控制流）

| 维度 | Claude Code | AiDock 取舍 |
|------|-------------|------------|
| 计划 vs 执行 | 不分 call，一条连续循环 | **学它**——揉进一条循环 |
| 计划清单 | 便签，模型可不 follow | **保留完整清单**，但当"循环内产物 + 进度锚点"，理由不同 |
| 失败重试 | 模型决定，无硬上限 | **主逻辑学它**，但**加硬底线**兜跑飞 |
| 刹车 | 软刹车（上下文满 / 超时 / 用户 Esc） | 用户不盯每回合，**硬底线替代人肉 Esc** |

##### 扩展点 / 失败模式（给 ④.c / ④.d 预留的插座）

现在不实现 ④.c/④.d，但 ④.a 地基必须先留好它们的"插座"，否则以后是刨地基重铺。三个插座：

- **④.c 插座（结果校验）**：循环里"拿到结果"和"据此继续"之间，**永远**过一道 `判断(结果) → 信号(continue/retry/escalate/abort)` 关卡。MVP 这关卡是**桩**、恒返回 `continue`；④.c 上线 = 换掉桩。**循环分叉只认信号、不直接看原始结果**——这样加 ④.c 不动骨架。
- **④.d 插座（安全检查）**：动作不许直接调工具，必须过一个**唯一执行口子** `执行(动作)`。MVP = 直接真调；④.d = 口子前先过安全检查关卡（危险分级 → 安全级别 → 放行/问人/拒，L3 越界永拒）。注意安全检查 ≠ 审批门：界内直接跑，只在 L1/L2 按角色安全级别才"问人"（问人复用审批通道）。〔原"沙箱/dry-run"设计已废，见 ④.d〕
- **④.a「等待」↔ ③ 接口**：见下方硬约束。

##### 硬约束：Turn 可暂停 + Plan 可存取（等待动作走挂起-恢复）

agent 执行"等待"动作（如 ASK_AGENT 后等回答）采用**挂起-恢复（不是阻塞占线程）**——多 agent 并发下阻塞会互等死锁。由此两条硬约束：

1. **一个 Turn 不是"一口气跑完"**——它可中途暂停、跨越一次等待。**Plan 状态必须能存下来再拿回来**（落盘走 ⑥，层级语义归 ④.b）。
2. **④↔③ 走 ② 契约，不许 ④ 直接捅 ③**：④.a 只吐出"我要问 X 并等待"这个动作；送信 / 挂进 `WAITING_ANSWER` / 回答到了重新进循环从存点续跑——全是 ③ 的活。

##### 可观测性：内部活动漏给 UI（新 ② 旁路事件，**完全照搬 `MCP_CALL_EVENT` 待遇**）

agent 内部循环的活动（干到哪步 / 失败 / 改了几次方案 / 为啥 escalate）**要展示给 UI**——用户需要看到当前进度。但有两条红线：

- **红线一：不许 ④ 直接捅 UI。** 走 ② 加一个**新事件类型**（旁路通道，套路同 `MCP_CALL_EVENT`），④ 往里发、① 来收，④ 自己不碰 UI、不写 IPC。
- **红线二：这条进度绝不回灌进任何 agent 的 LLM 上下文。** 双通道规则：内部进度事件 → 走 ② 给 ① 展示 + 落 ⑥ 重启恢复，**纯单向到 UI 为止**；**不进** ④ 自己下回合 history，**不进**别的 agent 上下文。零 token。

> **与已有 `PROGRESS` 消息的区别**：`PROGRESS` 是**跨 agent 协作消息**（③ 协议，进对话历史、喂别的 agent，"我告诉队友干到 50%"）。内部循环的齿轮转动是 ④ 内部机械活动，**绝不复用 `PROGRESS`**——塞进去会污染协作上下文、把"对队友汇报"和"内部机械"混成一锅。必须是独立的 UI-only 旁路事件。

**漏多细：里程碑级结构化事件（选 A，非逐圈/逐字）**。在这几个边界点各发一条：

```
计划生成 / 计划改写(带原因 + 第几次改) / 某步开始 / 某步结果(成功|失败) / 重试中 / escalate(带原因) / 回合结束
```

① 渲染成"活的进度卡"（Claude-Code 风格）："正在第 2 步 · 已重排 2 次 · 卡住了，已问用户"。

##### 测试策略：把 LLM 做成可替换接缝

④.a 含 LLM 调用——慢 / 不确定 / 花钱，不能每跑一次测试就真打模型。核心：**"模型决定下一个动作"这步做成可注入函数**，真跑插真 LLM、测试插**写死剧本的假 LLM**，循环骨架即可脱离真模型确定性地测。专测最易在重构时悄悄坏的点：

1. **顺利路径**：3 步计划全成功 → 正确发 DONE。
2. **失败重排**：第 2 步失败 → 改写计划 → 续跑。
3. **硬底线触发**：同一步失败 K 次 → 强制 escalate（防"以为有底线其实没拦住"）。
4. **挂起-恢复**（最该测，跨 ④/③）：动作=等待 → 挂起且 Plan 存住 → 拿回答重进 → 从存点续跑。

**两条回归守卫**（钉死双通道规则，防退化）：

- 断言内部进度事件**没出现**在下一次 LLM 调用的 messages 里（红线二自动守卫）。
- 断言喂给下一回合的 MCP 结果是 **summary 非全量**（全量只在事件/落盘）。

外加 **持久化往返**：挂起存 Plan → 重载 → 恢复，结果与不中断一致（守 ⑥/④.b 边界）。

##### ③ 接线（#2）：消息驱动的 dispatcher 怎么托管回合制循环

每个 agent 是一个 tokio 任务 + inbox。**inbox 只管"回合的开始点和恢复点"，PlanLoop 管"回合内部"**——一个回合内部那很多圈 LLM 调用是循环自驱的，不靠 inbox。两者不冲突。

相对现状的 delta 就一句话：现 runtime 已有退化版挂起/恢复（发 ASK → `WAITING_ANSWER` → 等 → 匹配 ANSWER → **从头 think**），把它升级成：

> 挂起 = 转 `WAITING_ANSWER` **+ 存住 `TurnState`**；恢复 = 拿存住的 `TurnState` 走 `PlanLoop.resume`（**接着上次跑，不从头**）。

任务结构：
```
loop {
  msg = inbox.recv()
  if msg 解掉我挂起的回合:  (outcome, ts) = plan_loop.resume(存的 ts, answer)
  else if should_respond:   (outcome, ts) = plan_loop.run_turn()
  match outcome {
    Done      → 发 DONE，转 IDLE，丢弃 ts
    Suspended → 发 ASK，转 WAITING_ANSWER，存住 ts
    Escalated → 告诉用户/PM 卡住，转 IDLE
  }
}
```

**outward Action 选 A（④ 保持 ③-中立）**：`react.rs` 的 `Wait/Speak` 只带中立字段（to/content…），**不 import `AgentMessageKind`**；`Action → AgentMessageKind` 的翻译放在 `agent/turn_bridge.rs`（③ 边界），跟 `llm_source` 的 `ChatResponse → Action` 对称。守住 ④ 不依赖 ③。

两个 MVP 取舍 + TODO：
- **挂起的 `TurnState` 先存内存**（agent 任务字段），不落盘。`TurnState` 已可序列化（测试验过往返），但"现在就落盘"不强求。> **TODO**：跨重启恢复挂起回合 = 待加固（落 ⑥）。
- **一个 agent 一次只跑一个回合**：回合在飞（活跃或挂起）时，新的触发消息进**待办队列**、先不开第二个回合，等当前回合 DONE/Escalated 再消费。代价：丢了"打断正在等待的 agent 插急活"。不引入回合栈。

#### ④.b 记忆分层
- **管**: 单 agent 的**私域**内部记忆 + 给 ④.a 提供 context bundle
- **不管**: 多 agent 之间的共享（③ 协作运行时的事）、跨 session 的会话管理（③ 会话生命周期）
- **接口**:
  - 入：incoming message、tool result、agent 自己产生的输出
  - 出：context bundle（给 ④.a 当一次 plan 的上下文）
- **现状**: 临时桩（`build_level_0_context` + scratchpad + `recall_topic`），没分层、没裁剪策略。**设计已冻结**（见下"本轮敲定"），未实现。
- **持久化承担方**: ⑥ 负责落盘，④.b 负责"层级语义"

##### 本轮敲定（混合分工 + 计划驱动 + 自报家门）—— 设计冻结，未实现

**1. 分工（谁读谁写）**：
- **程序（Rust）**：组装 context bundle 喂 ④.a（确定性、不烧 token）；在 `ActionExecutor` 口子上捕获 L3（每次 `fs__` 写成功后 git commit 到 `.aidock_git`）。
- **守护归纳 agent（LLM）**：把详情压成小结 / L2 打包。**缩水**——结构不靠它重建（见 2）；**MVP 可先不上**，结构+详情裸存先跑通。
- **主 agent**：只管干活 + 按需 `recall_detail(id)` 拉详情，不再手动记账（→ 现有 `scratchpad` 退役、`recall_topic` 演进成 `recall_detail`）。

**2. 结构跟着计划走（关键——接上 ④.a 的 `set_plan`）**：记忆结构不是事后扒流水账重建，而是 agent 规划时**实时、免费**长出来的：
- `set_plan` 的**目标** → **大主题**；每一**步** → **小主题**；每步实际干的（代码/决策/坑）→ **详情**；步骤标"完成"→ 小主题闭合。
- 推论：**大主题文件 ≈ 持久化的 Plan**（④.a 的 `TurnState.plan` 正好落成它）。

**3. context bundle（开始干活时给 agent 看啥）**：默认 = **当前大主题计划表（目标+各步状态）** + **当前步详情全文** + **已完成步的一句话小结**；其余大主题不看。

**4. 摘要必须"自报家门"（防 LLM 把精简版当全部）**：每条折叠笔记眼前必带 **小结 + id + `recall_detail(id)` 取回指针**；上下文放常驻提示"带 📎 的有全文"；**计划表永远列全步骤**（只折叠 done 步正文）。否则 recall 工具形同虚设——LLM 不知道自己不知道。与现有"MCP 结果摘要后跟 `[已截断,全文在日志]`"一脉相承。

##### 三层结构（按"粒度 / 保留时长"切）

- **L1 — 压缩层**：给 LLM 多轮对话用，按 3 粒度组织
  - **大主题**：完整任务的"章"，例"完成登录功能"
  - **小主题**：大主题下的有序里程碑，例"写登录技术文档" / "实现 login 接口" / "实现 JWT 验证"
  - **详情**：每个小主题展开的完整内容（决策记录 / 代码片段 / 踩坑笔记）
  - LLM 看长期 → 看大主题；看近期 → 看小主题；要细节 → 下沉到详情
- **L2 — 交接层**：L1 自身的大主题 + 小主题清单都撑不下时，开新文件、上文压成"交接摘要"作为新文件开头（类 Claude Code auto-compact）
  - 触发阈值（多大算满）：**TBD**，先 hand-wave 跑起来再校准
  - "交接摘要"格式：**TBD**
- **L3 — 详情/diff/回滚层**：agent 实际做了啥的完整留痕
  - agent 每次文件操作通过**隐藏 git repo**（`.aidock_git/`）commit 一次
  - 回滚 = `git reset --hard <commit>`；差异 = `git diff`
  - git 用 zlib + delta，大文件多次改不会暴涨

##### 归纳谁来做

**"守护归纳 agent"**——挂载在主 agent 任务上的内部 agent（非用户可见角色，不是 PM / 前端 / 后端）。**本轮缩水**（见"本轮敲定"1）：结构由 `set_plan` 实时给出、不靠它重建；它只负责把详情压成小结 + L2 打包，**MVP 可先不上**。触发时机：**实质回合**（写了文件/改了方案）结束后才跑，跳过纯一句话的小回合。

##### L1 存储方案：套文件夹（保结构）+ 内容混淆（A 档 — 防剖方案）

保留嵌套结构（目录层级 = 大→小→详情），但**为防开发剖存储方案做两件事**：
1. **文件/文件夹名去语义**——只留序号，不带目标/步骤 slug（否则目标与步骤名直接摆在文件名里，内容混淆也白搭）。
2. **内容存成自定义混淆块**——Rust 在读写边界 encode/decode，`cat` 出来是乱码。

```
~/.aidock/sessions/{session}/agents/{agent_id}/memory/
  topic-001/        # 大主题（序号即 id，无语义 slug）
    _topic          # 混淆块；解开后 = markdown(目标+步骤+状态, ≈ 持久化 Plan)
    01/             # 小主题（序号）
      detail        # 混淆块；解开后 = markdown(小结行 + 正文)
    02/
      detail
  topic-002/
    _topic
```

- **逻辑格式仍是 markdown**（解开后），给 LLM / 我们的代码用；**物理落盘是混淆块**。LLM 全程走 Rust（bundle / recall_detail），永远看明文、不碰原始文件。
- **混淆 ≠ 加密**：减速带不是墙（解码逻辑在二进制里，铁心开发反编译照样拿到）。**不上带本地钥匙的加密**（假安全）。`encode`/`decode` 可换可升级（MVP = dep-free keystream XOR + magic header）。
- **接受的代价**：① git L3 的"可读 diff + delta 压缩"失效（落盘是混淆块）——回滚(reset)仍可用，"看变化"+压缩没了；② 目录树形仍泄露"有个 3 层层级"这一点（A 档接受的泄露），但目标/步骤/详情这些**语义**藏住了；③ 人没法肉眼看记忆。
- 原"fs 工具友好 / 人类可读"理由作废（分工下 LLM 本就不碰原始文件，记忆由 Rust 管）。

##### 已知弱点 / 后续加固 backlog（不阻塞 MVP）

| # | 弱点 | 触发场景 | 加固方向 |
|---|------|---------|---------|
| 1 | 拿全图随主题数线性变慢 | 主题积累到几十个以上 | 内存里加 `Vec<TopicMeta>` 缓存，启动一次加载、写时增量更新 |
| 2 | 跨大主题的语义关联取不到 | 用户问"我之前是怎么处理鉴权的" | 加 embedding 索引（向量库），V0.1 不上 |
| 3 | 并发写同一 topic 文件会冲突 | 归纳 agent + 主 agent 并发 | 每文件加写锁 / 单线程归纳队列 |

#### ④.c 结果校验 / 反馈回路
- **管**: ④.a 执行完一个 action 后，agent 自己评估"这一步真的推进了 plan 吗？"，决定 continue / retry / escalate / abort
- **不管**: action 本身怎么执行（④.a），不管安全检查（④.d）
- **接口**:
  - 入：④.a 的当前 plan + 刚执行的 action + result
  - 出：信号 `continue | retry | escalate | abort` 给 ④.a
- **现状**: 没有。工具调用返回什么 agent 就吞什么，不会发现"咦这和我预期不一样"

#### ④.d 安全检查（工作区间 confinement + 危险分级 + 安全级别）

> **重大转向（2026-06-03）**：原"OS 沙箱（dry-run / temp 副本 / Seatbelt / ASRT）"方案**整个废弃**，`agent/shell.rs` 的 `SeatbeltSandbox` / `AsrtSandbox` 作废待删。改为**工程化的"安全检查"防线**——可信工作区间 + 路径校验 + 角色安全级别。理由：本地桌面、用户在场、威胁模型是"agent 犯错"而非"恶意攻击"，且对齐 CC 默认（CC 不开 OS 沙箱，底线靠权限系统）。

- **管**: 每个 **Call 类**工具执行**前**的一道关卡（= CC 的 PreToolUse 位置）。两步——先算**客观危险级**，再按**角色安全级别**决定 放行 / 问人 / 拒。
- **不管**: 决策为何选这工具（④.a/④.c）、工具怎么执行（backend）。

**① 客观危险分级（引擎算，不可配）** —— 每次 `(工具, 参数)` 算一个 `Level`：

| 级 | 是什么 | 例 |
|---|---|---|
| **L0 无害** | 只读 + 一切内部操作 | Read/Glob/Grep/LSP（可越界）、mem_recall、topic_*、write_todos、消息工具 |
| **L1 区内改** | 改/删，路径**在**工作区间 | Edit/Write/删除 → 区内 |
| **L2 需确认** | 影响无法静态判定 | Bash、`mcp__*`、装依赖/联网 |
| **L3 禁止** | 改/删，路径**越界** | Edit/Write/删除 → 区外（**永拒，任何安全级别都不能放行**） |

L3 红线的物理实现 = `level_for_path`：路径相对 workspace 解析 → `canonicalize`（解 `..` + 软链；不存在的新文件则**探父目录**）→ 是否 `starts_with` canonicalize 过的 workspace。**对结构化工具是铁的**（路径是参数）；**对 Bash 是尽力而为**（恒 L2，命令串无法静态判越界，靠规则/问人兜）。

**② 安全级别（可配，挂 `RoleConfig`，角色级）** —— 只挪 L1/L2 的"放行↔问人"，**碰不到 L3**：

| 安全级别 | L0 | L1 区内改 | L2 需确认 | L3 越界 |
|---|---|---|---|---|
| **严格** | 放行 | 问人 | 问人 | 拒 |
| **标准**（默认） | 放行 | 放行 | 问人 | 拒 |
| **宽松** | 放行 | 放行 | 放行 | 拒 |

"问人"复用 `APPROVAL_REQUEST_EVENT`(②)。比 CC 更紧：CC 的 `bypassPermissions` 连越界都放，AiDock 的 L3 永不松。

- **谁是哪一层**：工作区间 = 会话/工作室级（进工作室选，全员共享一条边界）；L3 红线 = 全局不变量；安全级别 = **角色级**（④ 内部架构）。边界共享、strictness 各角色各拧、红线谁都松不动。
- **specifier 规则**（`Bash(npm test)` 放行 / `Bash(rm *)` 拒）= 级别→策略**之前**的一道短路，**不在 `danger` 里**（danger 只算客观级）。
- **现状**: ✅ **已落地**（`agent/security.rs`：`Level`(L0-L3) + `classify_level(tool,args,ctx)` + `decide(level,security_level)`；`CompositeExecutor::execute` 在文件改/删 + Bash 前过这道关——Deny 直接 `DENIED`、Ask 复用 `McpExecutor::approve`→`APPROVAL_REQUEST_EVENT`、Allow 放行；L3 物理底线另由 `fs_tools::within_workspace` 在文件工具处再兜一层）。注：设计里 `Handling::Call.danger` 落地成集中函数 `classify_level`，不是每 spec 一个 fn 指针。✅ MCP 已收口进本道（`is_mcp_tool` → L2 → 同一道问人；`McpExecutor` 退成纯后端 `call_backend`）。✅ **specifier 规则**（`security.rs`：`PermissionRule{tool,pattern,action}` + `match_rules`，级别策略**之前**短路——Deny 优先、Allow 免问；挂 `RoleConfig.permission_rules`。默认：所有角色拒读密钥 `Read(*/.ssh/*)`，工程师常用验证命令免问 + `sudo`/`rm -rf /` 硬拒。Bash 命令串匹配 best-effort 可绕，非墙）。

> **历史（已废弃，留作脉络）**：本节曾基于"OS 沙箱"设计——`shell.rs` 里实测落地过 `SeatbeltSandbox`（`sandbox-exec` + 自写 profile）和 `AsrtSandbox`（`@anthropic-ai/sandbox-runtime`），shell 工具走 `CompositeExecutor`、工程师专属、前台命令能在盒里真跑。**2026-06-03 整体推翻**：改走上面的"工作区间 confinement + 安全检查"。`shell` 工具保留（更名 `Bash`、并入统一注册表的 Native backend），但**不再裹 OS 沙箱**——`SeatbeltSandbox`/`AsrtSandbox` 作废待删。原"shell 需先有沙箱才能放"的硬前置随之解除：Bash 现在靠 ④.d 的安全级别（L2 默认问人）+ 工作区间路径校验把关。

### ④ 子模块依赖与推进顺序
1. **④.a 是地基**——其他三个都假设有 plan / act 区分
2. **④.b 紧随 ④.a**——plan 步骤要靠谱上下文才能不瞎想；没有分层记忆，plan 退化成"猜"
3. **④.c 在 ④.a + ④.b 之上**——要有 plan 才能比对结果，要有记忆才能记下 retry 历史
4. **④.d 最后**——前 3 个稳了再加。安全检查是 hardening，不是地基（注：工作区间 confinement 的 L3 路径校验属底线，需随 Bash/文件工具一起上；可配的安全级别策略可后补）

### 跨块影响提醒
- **④.a 的 "plan" 要不要展现给 ①？** 默认是 ④ 的内部状态。如果以后做"agent 思考可见"——加 ② 的事件类型，**不允许** ④ 直接调 UI 或写 IPC
- **④.c 的 retry 是否要落 ⑥？** retry 历史属于"私人笔记层"（④.b 的语义），落盘时通过 ⑥ 的接口，但语义归 ④.b
- **④.d 安全检查不引入新 provider**——危险分级 / 路径校验 / 安全级别都是 ④ 内部逻辑，挂在 Call 工具执行前的关卡上，与 ⑤ 无关

## ⑤ LLM Provider
- **Files**: `src-tauri/src/llm/*` (`bailian.rs`, `mod.rs`)
- **Owns**: transporting `ChatRequest` → `ChatResponse` to/from the provider's HTTP API, error mapping
- **Pure transport layer**: does NOT decide what goes into `messages`, does NOT define tool schemas. Swapping providers should touch only this block.

## ⑥ 持久化
- **Files**: `messages.jsonl`, `scratchpads/*.json`, `mcp_calls.jsonl`, `persistence.rs`, `mcp_log.rs`
- **Owns**: write-on-event + reload-on-mount
- **Critical rule**: "restored on reload" ≠ "fed into the LLM". Persistence is for ①'s recovery on restart, **not** for ④'s context window. These are two different consumers of the same write.

---

## 双通道规则（最常踩的坑）

Two pieces of data each flow down **two independent pipes**. Mixing the
pipes is the bug source. State them explicitly:

### 工具调用结果（任何 Call 工具——fs/Bash/MCP/recall…，事件统一走 `TOOL_CALL_EVENT`）
- → 进 ④ (next LLM turn's `tool` message): **summary**, token-aware
- → 进 ⑥ + 通过 ② 给 ①: **full body**, no token cost
- Implication: shortening what the UI shows is a block-① concern and **must not** edit anything in block ④. Lengthening what the LLM sees is a block-④ concern and **must not** touch the event payload.

### Agent 输出消息
- → 进 ④ (the agent's own history for next turn) and → 进 ① (rendered as a chat bubble) may share the same `AgentMessage` value
- But reformatting how it shows in ① **must not** loop back into ④. If you find yourself rewriting an `AgentMessage` to make the UI prettier, that's a UI-side render concern, not a backend mutation.

---

## How to use this file

When taking a task:
1. Identify which block(s) the user wants changed (often only one).
2. If your fix touches a second block, that's a signal — pause and check whether the contract (block ②) is actually what needs fixing.
3. Never silently widen scope across blocks. Call it out and let the user decide.
