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
- **Files**: `src/lib/ipc.ts` ↔ `src-tauri/src/commands.rs`, Tauri event names/payloads (`MESSAGE_EVENT`, `MCP_CALL_EVENT`, `APPROVAL_REQUEST_EVENT`)
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

### ④ 子模块拆分

V0.1 把整个 ④ 当成一个"reflex"实现：一条入消息 → 一次 LLM call → 一个出动作。下个阶段把它拆成四个**可独立设计的子模块**。任何 ④ 的改动都该明确说自己改的是哪个子模块；跨子模块的改动要先调它们之间的接口、再分别改。

#### ④.a ReAct 计划循环
- **管**: 单 agent 一回合的内部状态机，把"接消息 → 直接出动作"改成"plan → act → 观察 → 必要时再 plan → ..."
- **不管**: 上下文是哪里来的（④.b 管）、动作的结果是否符合预期（④.c 管）、不可逆操作的预演（④.d 管）
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
- **④.d 插座（沙箱）**：动作不许直接调工具，必须过一个**唯一执行口子** `执行(动作)`。MVP = 直接真调；④.d = 口子里先 dry-run/temp、验过再 commit。注意沙箱 ≠ 审批门（审批=问人，沙箱=agent 自验），两条独立通道可同时裹在这口子上。
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
- **现状**: 临时桩。scratchpad/{role}.json + 闭合 topic 索引 + `recall_topic` 工具混在一起，没分层、没裁剪策略
- **持久化承担方**: ⑥ 负责落盘，④.b 负责"层级语义"

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

**"守护归纳 agent"**——挂载在主 agent 任务上的内部 agent（非用户可见角色，不是 PM / 前端 / 后端），专门把 L3 详情往 L1 三粒度灌。归纳是 ④.b 架构里**固有的一环**，不是可选项。

##### L1 三粒度存储方案：分层文件系统

```
~/.aidock/sessions/default/agents/{agent_id}/memory/
  topics/
    topic-001-login.md      # 大主题：标题、状态、小主题清单（链接）
    topic-002-cart.md
  subtopics/
    sub-001-tech-doc.md     # 小主题：标题、归属大主题、简介、详情链接
    sub-002-jwt.md
  details/
    detail-001-tech-doc.md  # 详情：完整内容
```

文件格式：**markdown + YAML frontmatter**（`id` / `parent` / `status` / `created_at` / `updated_at`）。

**为什么选这个**（vs 嵌套 JSON / SQLite / 事件流）：LLM 用现成的 `fs__read_file` / `fs__write_file` 工具直接操作；每文件大小可控；git tracking 天然；跟人类项目笔记直觉一致。

##### 已知弱点 / 后续加固 backlog（不阻塞 MVP）

| # | 弱点 | 触发场景 | 加固方向 |
|---|------|---------|---------|
| 1 | 拿全图随主题数线性变慢 | 主题积累到几十个以上 | 内存里加 `Vec<TopicMeta>` 缓存，启动一次加载、写时增量更新 |
| 2 | 跨大主题的语义关联取不到 | 用户问"我之前是怎么处理鉴权的" | 加 embedding 索引（向量库），V0.1 不上 |
| 3 | 并发写同一 topic 文件会冲突 | 归纳 agent + 主 agent 并发 | 每文件加写锁 / 单线程归纳队列 |

#### ④.c 结果校验 / 反馈回路
- **管**: ④.a 执行完一个 action 后，agent 自己评估"这一步真的推进了 plan 吗？"，决定 continue / retry / escalate / abort
- **不管**: action 本身怎么执行（④.a），不管沙箱（④.d）
- **接口**:
  - 入：④.a 的当前 plan + 刚执行的 action + result
  - 出：信号 `continue | retry | escalate | abort` 给 ④.a
- **现状**: 没有。工具调用返回什么 agent 就吞什么，不会发现"咦这和我预期不一样"

#### ④.d 沙箱
- **管**: 不可逆工具调用前的预演 / 临时副本。让 agent 自己先验一遍再 commit
- **不管**: 决策为何选这个工具（④.a + ④.c 管）、用户是否批准（已有 approval gate）
- **接口**:
  - 入：要执行的工具调用
  - 出：routing through dry-run / temp 目录；最终 commit 或 rollback
- **现状**: 没有。approval gate 只是问用户"能不能干"，干就直接动真盘
- **与 approval 的区别**: approval gate 是"问人"，沙箱是"agent 自己先验"。两条独立通道

> **shell / 命令执行能力 = ④.d 的硬前置，不得提前放出。** dev-test 时用户提出"让 agent 跑 `npm run dev` 启动项目"。结论：**推迟到 ④.d**。理由：shell 是地基阶段最危险的不可逆能力，只靠 approval gate（问人）远远不够。而且它需要的沙箱**比本节原设想的"temp 副本 / dry-run"更强**——那针对文件操作；任意进程要的是 **OS 级隔离**（容器 / `sandbox-exec` / chroot+seccomp / 资源限制 / 断网），挡住 `rm -rf /`、外联、挖矿。设计要点：① 工具 `shell__run(command, background?)`，一次性等结果 / 长驻后台 spawn+登记 PID ② 路由进 `CompositeExecutor`（新 `ShellExecutor` 分支，原生 `tokio::process`，不走 MCP）③ `classify()` 标 Destructive，过审批门 ④ 仅工程师角色，PM 无 ⑤ 跑在 OS 沙箱内。在 ④.d OS 沙箱就绪前，agent 只写文件 + 把启动命令告诉用户，由用户手动跑。

##### 设计基线（对标 Claude Code 的沙箱，2026-06 查证）

CC 的沙箱不是容器/VM，是 **OS 原语**：**macOS = Seatbelt（`sandbox-exec`/SBPL）**，**Linux/WSL2 = bubblewrap + namespaces + seccomp**。Anthropic 把它开源成 npm 包 **`@anthropic-ai/sandbox-runtime`**（包住一个进程 + 网络代理 + 可选 seccomp）。对 AiDock 这种本地桌面工具，OS 原语优于容器（不依赖 Docker、启动快、无小白门槛）；容器/远程沙箱留作"更强更重"的产品级选项。

落到我们架构的 5 条基线：
1. **机制**：OS 原语（mac Seatbelt / Linux bwrap+seccomp）。优先评估直接 *adopt* `@anthropic-ai/sandbox-runtime` 而非从零造（AiDock 已在用 npx 跑 Node 子进程，接得上）。
2. **挂载点**：④.a 的 `ActionExecutor::execute()` **唯一执行口子**——"开沙箱"=工具在盒子里跑，`CompositeExecutor` 路由一行不改，只换执行后端。
3. **文件**：写限 workspace；读默认宽——但**必须默认 `denyRead` `~/.ssh`、`~/.aws/credentials` 等敏感路径**（CC 默认不挡，这是它的坑，别抄）。
4. **网络**：deny-by-default + 主机名白名单代理（代理在盒**外**，不解 TLS → 宽白名单有 domain-fronting 风险）。dev 刚需域（npm/pypi/cargo registry 等）进默认白名单，否则 `npm install` 直接挂。
5. **沙箱 × 审批关系（修正旧"两条独立通道"说法）**：**沙箱是强制底线，审批只在"越界"（写出界 / 连新域名）时才触发**。界内操作直接跑、不烦用户——顺带治掉"每个写文件都要点批准"的痛（dev-test 实测的痛点）。

##### 施工方案（混合，分阶段）—— 已定，实现排在 ④.b/④.c 之后

**ASRT 评估结论（spike 2026-06）**：`@anthropic-ai/sandbox-runtime`（`srt` CLI，Apache-2.0，v0.0.52，实验性 0.0.x）—— ✅ 有限命令完美（自带 Seatbelt + 网络白名单代理，JSON 配 `denyRead/allowWrite/allowedDomains`）；❌ **长驻 dev server 不支持**（`allowLocalBinding` 默认关、端口宿主可达性无文档、代理生命周期绑被包进程、无 daemon）。Node 依赖对 AiDock 是 free（已靠 npx 跑 MCP）。

**所以劈成两半（都走 `ShellExecutor` → ④.a `execute()` 口子，`classify()` 标 Destructive 过审批，仅工程师角色）**：

| 用途 | 选型 | 理由 |
|---|---|---|
| 有限命令（install/build/test/lint） | **adopt ASRT**（`srt --settings x.json <cmd>`） | 出站网络白名单是难点，ASRT 白送 |
| 长驻 dev server（npm run dev） | **自建 Seatbelt 包装**（Rust shell→`sandbox-exec` + 自定义 profile） | ASRT 搞不定；自定义 profile 允许绑端口 + detached spawn + 进程登记表（列/杀），宿主浏览器直访 localhost:port |

**分阶段**：
- **P0 spike**：✅ ASRT 调研完；剩 PoC——验证自建 Seatbelt profile 能否让宿主访问 dev server 端口（~半天）。
- **P1 有限命令沙箱（MVP）**：`shell__run(command)`→ASRT；workspace 写限制 + `denyRead ~/.ssh ~/.aws` + dev registry 白名单；接 CompositeExecutor + 审批 + 工程师专属；`Sandbox` trait 做成可 fake 接缝，测试驱动。
- **P2 长驻服务**：`shell__serve`/background → 自建 Seatbelt + 进程登记表 + 端口暴露（解"看界面"）。
- **P3 加固/跨平台**：Linux bwrap、沙箱=底线/审批=越界才触发的 UX、资源限制。

### ④ 子模块依赖与推进顺序
1. **④.a 是地基**——其他三个都假设有 plan / act 区分
2. **④.b 紧随 ④.a**——plan 步骤要靠谱上下文才能不瞎想；没有分层记忆，plan 退化成"猜"
3. **④.c 在 ④.a + ④.b 之上**——要有 plan 才能比对结果，要有记忆才能记下 retry 历史
4. **④.d 最后**——前 3 个稳了再加。沙箱是 hardening，不是地基

### 跨块影响提醒
- **④.a 的 "plan" 要不要展现给 ①？** 默认是 ④ 的内部状态。如果以后做"agent 思考可见"——加 ② 的事件类型，**不允许** ④ 直接调 UI 或写 IPC
- **④.c 的 retry 是否要落 ⑥？** retry 历史属于"私人笔记层"（④.b 的语义），落盘时通过 ⑥ 的接口，但语义归 ④.b
- **④.d 沙箱实现可能挂载 ⑤ 之外的工具**（dry-run executor / temp workspace mount）——这些是 ④.d 内部细节，不是新的 provider

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

### MCP 工具调用结果
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
