# AiDock V0.1 开发计划

> 本计划基于 `AIDOCK_DESIGN.md`。任何与设计文档冲突的实施决策，必须先回到设计文档讨论清楚。
> 标记 **【关键路径】** 的任务延期会导致整体延期。**【风险】** 标记的任务有不确定性，需要预留 buffer。**【验收】** 标记的是该任务做完的标志。

---

## 总览

| 阶段 | 时长 | 内容 | 累计周数 |
|---|---|---|---|
| Phase 0 — 准备 | 半周 | 环境搭建、依赖调研、仓库建立 | 0.5 |
| Sprint 0 — 脚手架 | 1 周 | Tauri 骨架 + 三栏布局 + IPC | 1.5 |
| Sprint 1 — 单 Agent 闭环 | 1-2 周 | LLM + BYOK + 单对单聊天 | 3 |
| **Sprint 2 — 多 Agent + 上下文核心 ⚠️** | **4 周** | 类型化消息 + 状态机 + 路由 + 并发 + Topic + SUMMARY + 三级加载 + 召回 tool + 工作台 | 7 |
| Sprint 3 — 持久化 | 1-2 周 | messages.jsonl + state.json + 重启恢复 | 9 |
| Sprint 4 — MCP + 真实文件操作 | 2 周 | MCP 客户端 + filesystem + 工具调用 UI + 审批弹窗 | 11 |
| Sprint 5 — 打磨 + 内测 | 1-2 周 | 错误处理 + 跨平台打包 + 找 5-10 人内测 | 13 |

**全职**：11-14 周 / **业余（每周 15-20h）**：6-7 个月

---

## Phase 0 — 准备阶段（半周）

> **目标**：所有开发前置工作就绪。这一步偷懒，后面 Sprint 0 会塞车。

### P0.1 环境搭建【关键路径】

- [ ] 安装 Rust stable（建议 1.80+）：`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- [ ] 安装 Node.js LTS（建议 20+）+ pnpm
- [ ] 安装 Tauri 2.x CLI：`cargo install create-tauri-app --locked`
- [ ] 验证：`rustc --version && node --version && pnpm --version`
- [ ] IDE：VS Code + rust-analyzer + Tauri 扩展（或 RustRover）

**【验收】**：能跑通 Tauri 官方 hello world demo

### P0.2 关键依赖调研【风险】

**rmcp**（Rust MCP SDK）—— Sprint 4 的关键依赖，必须提前验证：
- [ ] 在 `~/scratch/rmcp-test/` 写一个最简 demo，调用 filesystem MCP server
- [ ] 验证：能列目录、能读文件、能写文件
- [ ] **如果 rmcp 不成熟**：评估自己用 JSON-RPC 封一层的工作量，给 Sprint 4 留出 buffer

**Anthropic API**：
- [ ] 拿一个 API Key（用自己的账号）
- [ ] curl 直接调一次 messages 接口，验证 tool use 模式 work

**Keyring**（用于存 API Key）：
- [ ] `keyring = "3"` crate 在 macOS 上跑通存取（注意 Keychain 弹框）

### P0.3 仓库 & CI

- [ ] GitHub 私有仓库 `aidock` 建立
- [ ] `.gitignore`（Rust + Node 双模板）
- [ ] README 写清楚 V0.1 目标（一段话足矣）
- [ ] GitHub Actions：macOS + Windows + Linux 三平台 build CI 模板（先不实际用，模板就位）

### P0.4 设计文档锚定

- [ ] 把 `AIDOCK_DESIGN.md` 和本计划放进仓库的 `docs/` 目录
- [ ] 仓库 README 链接到 `AIDOCK_DESIGN.md`

**Phase 0 验收**：能跑 hello world Tauri / rmcp demo / Anthropic curl，仓库结构就位。

---

## Sprint 0 — 脚手架（Week 1）

> **目标**：能跑起来，能从前端调到后端。不实现任何业务逻辑。

### S0.W1.D1-2 — Tauri 项目初始化

- [ ] `pnpm create tauri-app aidock`（选 Svelte/React + TS，推荐 Svelte 5）
- [ ] 删除模板示例代码，保留最简骨架
- [ ] 配置 `tauri.conf.json`：应用名、bundle identifier、窗口默认尺寸
- [ ] 第一次 `pnpm tauri dev` 跑起来

### S0.W1.D3-4 — 三栏布局壳子

- [ ] CSS Grid 或 Flex 实现三栏（左 240px / 中 1fr / 右 320px）
- [ ] 三栏各填一个 placeholder 组件（不需要任何逻辑）
- [ ] 配置 Tailwind（如果用），确保深色模式 token 可用

### S0.W1.D5 — IPC 通信打通【关键路径】

- [ ] Rust 侧定义第一个 Tauri command：`hello_from_rust(name: String) -> String`
- [ ] 前端调用：`invoke('hello_from_rust', { name: '...' })`
- [ ] 验证：能看到 Rust 返回的字符串显示在前端

### S0.W1.D5 — CI 模板就绪

- [ ] GitHub Actions workflow：`pnpm install + pnpm tauri build`（不发布，只测能 build）
- [ ] 三平台 matrix：macos-latest + windows-latest + ubuntu-latest

### Sprint 0 验收

**【验收】**：
- ✅ 应用能在本地启动，看到三栏空壳
- ✅ 前端能调到 Rust 后端，拿到返回值
- ✅ 三平台 CI build 都能过
- ✅ Repo 结构清晰，模块切分预留

### Sprint 0 风险

| 风险 | 缓解 |
|---|---|
| Tauri 2.x 配置踩坑（特别是 Windows）| 留够时间，参考官方 docs 不要走捷径 |
| 前端框架不熟（如选 Svelte 但只懂 React）| 如果不熟，先花半天写个 Svelte 教程 demo 找手感 |

---

## Sprint 1 — 单 Agent 闭环（Week 2-3）

> **目标**：能像 Claude.ai 一样跟 Claude 对话。**没有多 Agent，没有工作室概念，就是个聊天工具**。

### S1.W2 — LLM 接入 + BYOK

#### Rust 侧

- [ ] `src-tauri/src/llm/anthropic.rs`：用 reqwest 封 Anthropic messages API
  - 支持 system / messages / tools 参数
  - 返回结构化 `LLMResponse { content, tool_uses, stop_reason }`
- [ ] `src-tauri/src/keyring.rs`：用 keyring crate 存取 API Key
  - `save_api_key(provider, key)`
  - `load_api_key(provider) -> Option<String>`
- [ ] Tauri commands 暴露：`save_api_key` / `send_message_to_llm`

#### 前端侧

- [ ] `ApiKeySetup.svelte`：首次启动检测无 key 时弹出设置页
  - 输入 Anthropic API Key
  - 调 `save_api_key` 保存
- [ ] `ChatPanel.svelte`：输入框 + 消息列表（最简单的 user/assistant 气泡）

**【验收】**：
- ✅ 首次打开应用，引导设置 API Key
- ✅ 设置完成后能跟 Claude 对话，消息一来一回
- ✅ 重启应用，API Key 还在（keyring 持久化生效）

### S1.W3 — 消息持久化（提前做一点 Sprint 3 的）

- [ ] `src-tauri/src/storage/messages.rs`：messages.jsonl append 写入 + 启动时加载
- [ ] 路径：`~/.aidock/sessions/default/messages.jsonl`（V0.1 固定单会话）
- [ ] 重启后能看到历史消息

### Sprint 1 验收

**【验收】**：
- ✅ 用户能完成"设置 Key → 聊天 → 关闭 → 重开 → 看到历史"完整闭环
- ✅ 第一行真正的"AI 应用价值"已经体现

---

## Sprint 2 — 多 Agent Runtime + 上下文核心【关键路径，最难】（Week 4-7）

> **目标**：3 个 Agent 在群里协作完成一个简单任务。**这是验证核心假设的 Sprint**——做不出来，整个产品方向要重新审视。

### S2.W4 — 类型化消息 + 状态机

#### 类型定义

- [ ] `src-tauri/src/message/types.rs`：
  ```rust
  enum MessageType { BROADCAST, ASK_AGENT, ANSWER, WORK_START, PROGRESS, DONE, SUMMARY }
  struct Message {
    id, sender, type, content, topic_id, timestamp,
    // ASK_AGENT 字段
    to: Option<String>, question: Option<String>, expected_format: Option<String>,
    // ANSWER 字段
    reply_to: Option<String>,
  }
  ```
- [ ] 让 LLM 输出符合这个结构——通过 system prompt + tool use 强约束
  - 角色 prompt 里说明可用的消息类型和必填字段
  - 用 Anthropic tool use 让 LLM "调用" message 工具，确保输出结构化

#### Agent 状态机

- [ ] `src-tauri/src/agent/state.rs`：
  ```rust
  enum AgentState { IDLE, WORKING { task: String }, WAITING_ANSWER { for: MessageId } }
  ```
- [ ] 状态转移规则：
  - IDLE → WORKING：自己输出 WORK_START 时
  - WORKING → IDLE：自己输出 DONE 时
  - * → WAITING_ANSWER：自己输出 ASK_AGENT 时
  - WAITING_ANSWER → IDLE：收到对应 ANSWER 时

### S2.W5 — 路由 + 并发 + 先到先现

- [ ] `src-tauri/src/agent/router.rs`：消息路由器
  - 解析消息里的 @ 提到的角色
  - 把消息塞进对应 Agent 的"信箱"
  - ASK_AGENT 类型：强制路由到 `to` 字段指定的 Agent
  - BROADCAST：每个 Agent 自己判断是否回应（V0.1 简化：只有满足"我的关注列表"的关键词时才回）

- [ ] `src-tauri/src/agent/runtime.rs`：Agent 执行循环
  - 每个 Agent 一个 tokio task
  - 信箱 = `tokio::sync::mpsc::Receiver<Message>`
  - 收到消息 → 检查状态机 → 决定要不要思考 → 调 LLM → 输出消息

- [ ] **先到先现机制**：
  - 不要等所有 Agent 思考完
  - 每个 Agent 输出后立即广播到前端（Tauri event）
  - 前端按到达顺序追加消息气泡

### S2.W6 — Topic + SUMMARY + Level 0 上下文构造【关键】

- [ ] **Topic 抽象**：
  - 每条消息必须带 `topic_id`
  - Agent 输出消息时声明 topic_id（要么用已有的，要么开新的并取名）
  - `topics.json` 维护：id / title / status (active|closed) / participants / start_time

- [ ] **SUMMARY 自动生成**：
  - 当一个 topic 上的 ASK_AGENT 都被 ANSWER 闭环、且无新消息超过 N 秒 → 系统触发"调解员"角色（V0.1 可以让技术老大兼任）做 SUMMARY
  - SUMMARY 消息写回 messages.jsonl，并更新 topic 的 status + summary 字段

- [ ] **Level 0 上下文构造器**（`src-tauri/src/agent/context.rs`）：
  ```rust
  fn build_context(role, current_topic, all_topics, recent_messages) -> Vec<LLMMessage> {
    // 1. System Prompt = role.system_prompt + 角色定义
    // 2. 当前 topic 的全部消息
    // 3. 已结束 topic 的索引（仅 title + summary）
    // 4. @我的最近 N 条消息（跨 topic）
    // 5. 当前直接引用的工件（V0.1 暂无工件，跳过）
  }
  ```

### S2.W7 — 召回 tool + Agent 工作台

- [ ] 给 Agent 提供两个 tool（通过 LLM tool use 暴露）：
  - `recall_topic(topic_id)` → 该 topic 全部消息
  - `search_topic(topic_id, query)` → 主题内关键词搜索（用 grep 实现）

- [ ] Agent 工作台（运行时记录）：
  - `src-tauri/src/agent/scratchpad.rs`：每个 Agent 一个内存中的 scratchpad
  - 字段：`task_breakdown`, `files_modified`, `decisions_made`, `current_focus`
  - Agent 可以通过 tool 读写自己的 scratchpad
  - 退出/异常时序列化到 `sessions/{id}/scratchpads/{role}.json`

- [ ] **写死的 3 个角色 system prompt**：
  - `src-tauri/src/studio/builtin.rs`
  - PM / 前端 / 后端 各一份 prompt
  - 在 prompt 里明确：你的职责、能用什么工具、输出消息类型规范、什么时候开 topic、什么时候发 SUMMARY

### Sprint 2 验收【关键】

**【验收】**：用户输入"做一个 todo 应用"，3 个 Agent 协作产出：
- ✅ PM 跟用户对齐需求（一两轮 ASK_AGENT 给用户）
- ✅ PM 把需求 ASK 给前端和后端
- ✅ 前端和后端各自工作（WORK_START → PROGRESS → DONE）
- ✅ 前端和后端之间有至少一次接口字段对齐（互相 ASK_AGENT）
- ✅ 群聊里能看到完整协作流程，topic 切换时有 SUMMARY 分割
- ✅ Agent 没有陷入无限讨论
- ✅ Agent 能用 recall_topic 召回之前 topic 的细节（手动测试）

**【关键里程碑】Sprint 2 结束后立即做一次"假设验证 mini demo"**：
- 用 V0.1 alpha 版做一个真实小项目（如写一个计算器、todo 应用）
- 同时用 Claude Code / Cursor 单独做同样任务作为对照
- **自问**：多 Agent 协作真的更好吗？哪里更好？哪里更差？
- 如果觉得"还不如单 Agent"，**立即停下来重新审视**，不要往后做了。

### Sprint 2 风险【风险】

| 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|
| **LLM 不按结构化消息输出** | 高 | 高 | 用 tool use 强约束；prompt 里反复强调；解析失败时给 Agent 重试机会 |
| **Agent 陷入无限讨论** | 中 | 高 | 实现 max_rounds_per_topic 兜底（V0.1 简化版：超过 N 轮就强制 SUMMARY） |
| **Topic 切换识别不准** | 中 | 中 | V0.1 简化：每次用户发新消息默认开新 topic，Agent 接力时延续 |
| **多 Agent 并发的 race condition** | 中 | 中 | 用 channel 串行化关键状态更新，state.json 用文件锁 |
| **token 成本失控** | 低（设计已 cover）| 中 | per_call_tokens 上限兜底 |

---

## Sprint 3 — 持久化（Week 8-9）

> **目标**：关闭重开，群聊和状态都在。

### S3.W8 — messages.jsonl + state.json

- [ ] `messages.jsonl` append 写入：
  - 每条消息生成时立即 append
  - fsync 保证持久化（性能换可靠性，V0.1 OK）
- [ ] `state.json` 原子读写：
  - 写时先写 `state.json.tmp` → rename → 原子替换
  - 用 fs2 加文件锁防并发
- [ ] `topics.json` 更新（增量）

### S3.W9 — 启动恢复 + 边界情况

- [ ] 启动时读取 `messages.jsonl` 重建群聊视图
- [ ] 启动时读取 `state.json` 恢复各 Agent 状态
- [ ] **崩溃恢复**：
  - JSONL 写一半崩了 → 启动时跳过损坏的最后一行
  - state.json 半写 → 用 .tmp 文件兜底，没有 .tmp 就用最后正确的 state.json
- [ ] Agent scratchpad 持久化（保存到 `scratchpads/{role}.json`）

### Sprint 3 验收

**【验收】**：
- ✅ Sprint 2 的 todo 应用 demo 跑到一半 → 关闭应用 → 重开 → 继续干，状态全在
- ✅ 杀进程模拟崩溃 → 重开能恢复（最多丢最后 1-2 条消息）

---

## Sprint 4 — MCP + 真实文件操作（Week 10-11）

> **目标**：Agent 能在用户的真实项目目录里读写文件、跑命令。

### S4.W10 — MCP 客户端集成

- [ ] `src-tauri/src/mcp/client.rs`：用 rmcp 实现 MCP client
  - 启动 MCP server 子进程（filesystem server）
  - stdio 通信
  - 把 server 提供的 tools 注册到 LLM 调用的 tool list
- [ ] **bundle filesystem MCP server**：
  - 在 Tauri 打包时附带 `@modelcontextprotocol/server-filesystem` 的 Node 版本（或找 Rust 实现）
  - 应用启动时从 bundle 目录解压并运行
- [ ] 新建会话时让用户选 project_root，传给 MCP server

### S4.W11 — 工具调用 UI + 危险操作审批

- [ ] **工具调用气泡**：
  - 当 Agent 输出 tool_use 时，UI 显示 `🔧 read_file('/path/to/file')` 卡片
  - 卡片可展开看完整参数 + 工具返回值
  - 写文件操作：显示 diff（用 similar crate 算 diff）
- [ ] **危险操作审批弹窗**：
  - `delete_file` / `run_shell_command` / 越界访问 project_root 之外 → 弹窗等用户确认
  - 用户可选"本次允许 / 本会话内允许 / 永久允许（持久化到 config）"
- [ ] **路径越界检查**：所有 filesystem 操作前检查 path 在 project_root 之内（防 path traversal）

### Sprint 4 验收

**【验收】**：
- ✅ 用户给 AiDock 一个真实项目目录
- ✅ Agent 能读项目文件理解上下文
- ✅ Agent 能写代码到项目目录（用户能在 VS Code 里看到改动）
- ✅ Agent 跑 `npm test` 这种命令时弹窗审批
- ✅ Agent 试图删除文件或访问 `~/.ssh/` 这种危险路径时被拦截

---

## Sprint 5 — 打磨 + 内测（Week 12-13）

> **目标**：交付一个能让 5-10 个真实用户用的版本。

### S5.W12 — 错误处理 + 性能调优

- [ ] **错误处理 UI**：
  - LLM API 限流：友好提示 + 自动重试
  - 网络中断：保留输入，等恢复后重发
  - MCP server 崩溃：自动重启 + 通知用户
  - keyring 读取失败：引导重新输入
- [ ] **性能**：
  - 大消息列表（>1000 条）的渲染优化（虚拟滚动）
  - messages.jsonl > 50MB 时的加载策略（只 tail 最后 N MB）

### S5.W13 — 跨平台打包

- [ ] macOS：`.dmg` + 公证（如有 Apple Developer 账号）/ 没有就 ad-hoc 签名
- [ ] Windows：`.msi` + 自签名（用户首次启动会有 SmartScreen 警告，文档说明）
- [ ] Linux：`.AppImage`（最简单）
- [ ] GitHub Releases 自动发布

### S5.W14（可选缓冲周）— 内测

- [ ] 选 5-10 个 AI 工程师朋友 / 社区用户
- [ ] 准备 onboarding 文档（5 分钟搞清楚怎么用）
- [ ] 提供反馈渠道（Discord / Telegram / 一个简单的反馈表单）
- [ ] **核心假设验证清单**：
  - 用户用 AiDock 完成了真实小项目吗？
  - 主观感受：多 Agent 协作有"惊喜"吗？还是觉得"还不如单 Agent"？
  - 哪些场景 Agent 会"打架"或"无限讨论"？
  - 用户什么时候被惹烦？
  - 用户主动提出"我要改 PM 的 prompt"了吗？（V0.2 工作室编辑器的需求信号）

### Sprint 5 验收【关键里程碑】

**【验收】**：
- ✅ 三平台都有可下载的安装包
- ✅ 5+ 真实用户跑通了"从安装到完成一个项目"全流程
- ✅ 内测反馈整理出 V0.2 的 Top 3 优先级

---

## 关键里程碑汇总

| 里程碑 | 时间点 | 标志 | 决策 |
|---|---|---|---|
| **M1** | Phase 0 结束 | 环境就绪、依赖调研完成 | 是否调整 rmcp 方案 |
| **M2** | Sprint 1 结束 | 单 Agent 聊天闭环 | 进入多 Agent 阶段 |
| **M3** | **Sprint 2 结束** ⚠️ 最关键 | 多 Agent 协作 demo work | **核心假设验证：继续 / 调整 / 停止** |
| **M4** | Sprint 3 结束 | 持久化稳定 | 进入真实文件操作 |
| **M5** | Sprint 4 结束 | 能真正干活 | 进入打磨阶段 |
| **M6** | **Sprint 5 结束** | 5+ 用户跑通真实项目 | **决定是否进入 V0.2 开发** |

---

## 整体风险登记

| 风险 | 影响 | 当前缓解 | 触发条件 |
|---|---|---|---|
| **多 Agent 协作效果不达预期**（最大风险）| 致命 | Sprint 2 结束立即 demo 自测 | 主观感觉不如单 Agent |
| rmcp 不成熟 | 高 | Phase 0 调研、必要时自封 JSON-RPC | rmcp demo 跑不通基本场景 |
| LLM 不按结构化输出 | 中 | tool use 强约束 + prompt 反复强调 | Sprint 2 测试时 >20% 输出不规范 |
| 跨平台打包问题 | 中 | Sprint 5 留够时间 | Windows / Linux build 失败 |
| 进度延期 | 中 | 业余开发者预留 30% buffer | 任一 Sprint 超期 >50% |
| 个人开发者动力衰减 | 中 | 每个 Sprint 后给自己 1-2 天休息+复盘 | 连续两周低效 |

---

## 不允许的行为（自我约束）

为了 V0.1 准时交付：

1. **不要在 V0.1 加任何"故意不做"清单里的东西**（工作室编辑器、多会话、阶段链等）
2. **不要在 UI 视觉上反复纠结**（按设计文档原则，本质对了就行）
3. **不要陷入完美主义**——能 work、能验证假设就是 V0.1 的成功
4. **不要跳过 Sprint 2 结束的假设验证**——这是项目存续判断点
5. **不要为了"完整性"提前做 V0.2 的东西**——会拖累 V0.1

---

## 业余开发者的时间规划建议

如果按业余 15-20h/周计算：

- 工作日晚上：3-4 天 × 2-3h = 6-12h
- 周末：1 天 × 5-8h = 5-8h
- **每周必须留 1 天完全不碰代码**（避免烧坏）

**节奏建议**：
- 第 1 个月：Phase 0 + Sprint 0 + Sprint 1（基础打牢）
- 第 2-3 个月：Sprint 2（最难，给足时间）
- 第 4 个月：Sprint 3 + Sprint 4
- 第 5-6 个月：Sprint 5 + 内测

**关键自律**：
- 每周写一个 1 行的进度总结，记录在仓库的 `PROGRESS.md`
- 每个 Sprint 结束做一次 retrospective（什么有效、什么不有效）

---

**文档结束。** 这份计划是动态的——随着实施进展，每个 Sprint 结束后回顾并调整下一个 Sprint 的细节。但 **关键路径、核心假设、Sprint 顺序、Sprint 2 的假设验证里程碑** 是硬性的，不能省。
