# AiDock 项目设计文档

> 本文档整理 AiDock 项目从商业模式到 MVP 实施的全部关键设计决策。所有标记 **【关键】** 的内容是影响整体架构、不能轻易改动的核心；标记 **【硬约束】** 的是必须遵守的边界条件；标记 **【风险】** 的是需要在实施过程中重点关注的隐患。

---

## 第一部分 · 项目定位

### 1.1 是什么
AiDock 是一个 AI 工作室平台。用户在平台内**自由组建** AI 工作室（如代码开发工作室、短视频制作工作室），每个工作室由多个 AI 员工（Agent）组成，承接具体任务。

### 1.2 目标用户
使用 AI 完成实际任务的专业用户——AI 开发工程师、内容创作者、产品/运营等。**不是面向小白用户**。

### 1.3 核心卖点【关键】
- **完全自由组建**（非模板化）——这是和 Coze / Dify / ChatGPT GPTs 的根本差异
- 工作室性质完全由使用者决定
- 用户在平台上创造工作室、迭代调试、上架交易

---

## 第二部分 · 商业模式

### 2.1 模式：Open Core + 市场抽成

| 部分 | 策略 |
|---|---|
| **开源**（免费）| 工作室基础框架、Agent 运行时、插件系统、单用户本地版、基础编排能力 |
| **闭源/收费** | 工作室市场与交易系统、跨工作室记忆共享、团队协作 |

### 2.2 主要收入：**市场抽成**【关键】
卖工作室 = **卖 prompt + 卖创意**——一套精心调配的 AI 团队配置。

具体卖什么形态待定，倾向"**订阅 + 按调用**"组合（首版可以是免费分发，验证 PMF 后再加付费）。

### 2.3 核心护城河
**市场的网络效应**（买家和卖家之间形成生态），而不是技术本身。技术本身两个月就能被复刻，生态不行。

### 2.4 与同类产品差异化【关键】

| 产品 | 卖什么 |
|---|---|
| ChatGPT GPTs | 单 Agent prompt |
| Coze / Dify | 工作流 DAG |
| Hugging Face | 模型 |
| **AiDock** | **AI 团队配置 + 协作创意** |

### 2.5 个人开发者硬约束【硬约束】
- 零云端运维成本（早期能跑在 ¥100/月以下）
- 用户自带 API Key（BYOK），平台不代付 token
- 不做云端 Agent 执行（个人开发者扛不住一个用户跑一晚的算力成本）

---

## 第三部分 · 核心产品模型

### 3.1 心智隐喻：群聊 + 公司组织【关键】
- **工作室 = 一个聊天群**
- 群里有"领导 Agent" + 多个"员工 Agent"，每个 Agent 有明确职责定位
- 用户的核心创造价值：**安排哪些职员、定义各自的工作定位**
- 交互形式 = 聊天群（用户和 Agent、Agent 和 Agent 都在群里发消息）

### 3.2 关键概念分层：定义 vs 会话【关键】

两个层面必须分清，混在一起会导致系统设计混乱：

- **工作室定义 (Studio Definition)**：静态配置（角色、阶段链、工件结构、SOP）。被建设一次，可以复用、上架市场、买卖。**类比 Class**。
- **会话 (Session / Run)**：基于工作室定义开启的一次具体任务实例。每次新任务开新会话，内含完整的消息历史、产出工件、Agent 状态、决策记录。**类比 Instance**。

市场上卖的是**定义**，用户日常运行的是**会话**。

### 3.3 四个一等公民概念【关键】

| 概念 | 含义 |
|---|---|
| **角色 (Role)** | 职责定义、说话风格、工具集、关注的工件类型 |
| **工件 (Artifact)** | 阶段性产物，跨角色传递、独立可引用（PRD、设计稿、代码、测试报告）。**工件不是消息，是独立对象**——系统不是纯聊天，是"聊天 + 文档协作"。工件有 locked 状态，下阶段开始后默认锁定，回锅需显式解锁（高摩擦） |
| **阶段 (Stage)** | 协作的时间切片，每个阶段有"主持人 + 参与者 + 输出工件 + 叫停权"。**阶段链是用户可配置的，不是 hardcoded** |
| **相关性过滤** | Agent 自己判断要不要介入，不是物理隔离消息 |

### 3.4 运行机制【关键】

- **用户是上帝级别**：可以直接 @ 任何 Agent，不必经过领导
- **Agent 主动发言**：员工 Agent 之间需要不断讨论、质疑、联调，不只是被动派活
- **消息全可见，按相关性过滤**：每个 Agent 物理上能看到群里所有消息，但只关注与自己角色和对接方相关的内容
- **有 SOP 但自由协作**：典型流转是"用户→老板→产品经理→技术老大→工程师"这种公司式流程，但每个阶段都允许讨论质疑

---

## 第四部分 · Agent 协作机制

### 4.1 消息类型化【关键】

Agent 输出**不是自由文本，是结构化指令**（同 LLM tool use 范式）。这从根本上解决"礼貌循环"问题。

| 类型 | 含义 | 是否强制对方回复 |
|---|---|---|
| `BROADCAST` | 群广播 | 否（Agent 自判要不要接） |
| `ASK_AGENT` | 定向询问 | **是**（强制路由+回复） |
| `ANSWER` | 回答 ASK_AGENT | 否（闭环不再触发） |
| `WORK_START / PROGRESS / DONE` | 工作状态报告 | 否 |
| `SUMMARY` | 主题/阶段总结 | 否 |

每条消息必带 `topic` 字段——Agent 主动声明"我在讨论什么"，监控器按 topic 分组计数轮次。

### 4.2 Agent 状态机【关键】

`IDLE` / `WORKING` / `WAITING_ANSWER`

- WORKING 中的 Agent **不被 BROADCAST 打断**
- 只有 ASK_AGENT 能"叫醒"WORKING 中的 Agent
- 解决多 Agent 系统"礼貌循环 / 必须回复"的经典灾难

### 4.3 路由：通过 @ 进行【关键】
消息里 @ 谁 → 路由到那个 Agent。不需要每个 Agent 自己判断"要不要回"。

### 4.4 叫停机制【关键】

每个阶段有"主持人"，主持人有该阶段的叫停权：
- 需求阶段 = 老板
- 方案阶段 = 产品经理
- UI 阶段 = 设计师
- 开发阶段 = 技术老大

用户始终是上帝，任何时候可叫停。

**自动兜底**：两 Agent 反复对话超过 N 次 / 单 topic 轮次超阈值 → 触发轻量监控器 → 唤醒调解员（可以是主持人或独立中立角色）综合双方 SUMMARY 后定夺、广播决策。

---

## 第五部分 · 角色 (Role) 完整模型【关键】

### 5.1 **一个 Role 绝不只是 prompt + LLM**

这是一个曾被简化的关键认知——必须明确：

| 维度 | 说明 |
|---|---|
| 模型选择（含 fallback） | 不同角色用不同模型（技术总监 Opus，老板/测试 Haiku） |
| 采样参数（temperature/max_tokens/extended_thinking） | 创意 vs 严谨 |
| 工具集（MCP） | 职责决定能用啥工具 |
| 输出格式约束 | 产物形态不同（PRD / 严格代码块 / 设计稿） |
| 上下文策略 | **持续对话必须，不能省**（见下） |
| 工作循环模式（react / plan_execute / single） | 思考方式不同 |
| token / call 预算 | 防失控 |
| Few-shot 示例（动态注入） | 风格/格式锚定 |
| 知识源 / RAG | 专业领域知识 |
| 子 Agent | 复杂任务再分解 |

### 5.2 完整角色 YAML 骨架

```yaml
role:
  # 身份
  name, avatar, description
  
  # 提示工程
  system_prompt
  few_shot_examples
  
  # 模型 & 推理
  model:
    primary: "claude-sonnet-4-6"
    fallback: "claude-haiku-4-5"
    temperature: 0.2
    extended_thinking: false
    max_tokens: 8192
  
  # 工具
  tools: [filesystem, browser, npm, git]
  
  # 输出约束
  output_format:
    allowed_message_types: [BROADCAST, ASK_AGENT, ANSWER, DONE]
    custom_schemas
  
  # 上下文策略
  context:
    max_history_tokens
    rag_sources
  
  # 工作循环
  loop_mode: "react"
  max_iterations: 10
  
  # 预算
  budget:
    per_call_tokens
    per_session_tokens
    per_session_calls
  
  # 子 Agent（可选）
  sub_agents
```

### 5.3 架构影响【关键】

1. **Agent runtime 必须是"可配置执行器"**——按角色选模型 client、加载工具集、采用循环、检查预算
2. **工作室定义复杂度比预想高**——每个角色是多维配置对象，这才是"卖创意"真正的载体
3. **市场差异化空间被打开**：免费版 vs 专业版 vs 行业定制（模型/知识源/子 Agent 配置不同）

---

## 第六部分 · 记忆系统【关键】

### 6.1 硬约束【硬约束】
**全部记忆不跨工作室**。工作室之间完全隔离，每个工作室的员工记忆只属于本工作室。

### 6.2 四层记忆模型

| 层级 | 内容 | 作用 | 跨会话? |
|---|---|---|---|
| **共享层** | 群消息、工件（PRD/UI图）、进度报告 | 协作 | 否 |
| **会话状态层** | 当前阶段、待办、卡点、异常信息 | 中断恢复 | 否 |
| **Agent 工作台** | 任务拆分、草稿、中间产物 | 当前工作 | 否 |
| **Agent 私人笔记** | 经验沉淀（如"用户偏好简短回复"） | 经验复用 | **是**（工作室内） |

### 6.3 上下文构造：三级渐进式加载【关键】

核心理念：让 Agent 像人一样**按需翻笔记**，不要一次性把所有"相关消息"塞进 prompt。

**Level 0（默认塞进 prompt）**
- System Prompt（角色 + 私人笔记 + SOP）
- 当前 topic 的全部消息
- 已结束 topic 的"索引列表"：仅 title + summary + key_decisions + participants（**不是全部消息**）
- @我的最近 N 条消息（跨 topic 兜底）
- 当前直接引用的工件（全文）

**Level 1（Agent 主动用 tool 召回）**
- `recall_topic(topic_id)` → 该 topic 的关键消息
- `search_topic(topic_id, query)` → 主题内关键词搜索
- `get_artifact(artifact_id, version?)` → 拿工件（含历史版本）

**Level 2（很少用）**
- `get_full_topic_log(topic_id)` → 完整原始消息流

### 6.4 信息架构统一模式【关键】

用户原话："**先找目录、再找简介、不够再看具体内容**"——这是 AiDock **核心信息架构模式**，所有信息检索（消息/工件/会话/笔记）都套用：
- 消息：topic 索引 → SUMMARY → 全部消息
- 工件：标题摘要 → 当前版本全文 → 历史版本
- 会话：会话索引 → 会话摘要 → 具体内容
- 笔记：tag → 条目摘要 → 详情

### 6.5 Topic 必须是有"门面"的对象

```yaml
topic:
  id, title, summary, key_decisions, participants, status, closed_at
```

主持人/调解员在 topic 关闭时输出 `SUMMARY` 消息类型，自动填充门面字段。

### 6.6 跨会话学习的优雅整合

**不用做复杂的向量检索系统**（个人开发者扛不住）。**跨会话学习 = Agent 私人笔记的跨会话延续**：
- 会话过程中，Agent 自己决定哪些经验"提升"到私人笔记
- 新会话开始时，每个 Agent 加载自己在本工作室的私人笔记作为 system prompt 一部分

### 6.7 私人笔记机制
- 触发：显式 `PROMOTE_TO_NOTE` + 会话结束兜底 + 用户可问
- 分类：`preference / domain / lesson / collaboration / decision`
- 格式：结构化 YAML 条目
- 防膨胀：按 tag 加载 + LLM dedupe + 用户面板手动 review + 长期未用归档

### 6.8 工件版本管理【关键】

**写权限模型**：角色绑定 + 提议-审批
- 工件类型绑定角色（PRD→PM、设计稿→UI、代码→工程师）
- 其他人想改 → 发 `PROPOSAL` → owner 审批

**Version 状态机**：`draft` → `locked` → `superseded`
- **关键约束**：locked 版本**不可变**

**修改 locked 工件流程**：
1. 他人发 PROPOSAL 提议修改
2. owner 决定：拒绝 → BROADCAST 说明理由 / 接受 → 进入下一步
3. 解锁 v_current（变 superseded） + 新建 v_next (draft)
4. owner 编辑 v_next
5. 锁定 v_next
6. 会话状态层引用更新到 v_next
7. 触发相关 Agent 重新对齐

**Version 必须和会话状态层耦合**：
```yaml
session_state:
  current_stage: "development"
  stage_artifacts:        # 当前快照
    PRD: {artifact_id, version}
  stage_history:          # 阶段切换时的快照历史
    - {stage, entered_at, snapshot: {PRD: v2}}
    - {stage, entered_at, snapshot: {PRD: v3}}
```

支持"时间旅行"——可看任意阶段时的状态。

---

## 第七部分 · UI/UE 工件管理【关键】

### 7.1 UI 工件 ≠ Markdown 工件
这是一个曾被简化的盲点。UI 产物本质是视觉物，套用 markdown 工件管理跑不通。

### 7.2 核心方案：HTML/CSS/Tailwind 代码作为"设计稿"【关键】
不走 Figma。理由：
1. **AI 工作流现实**——v0.dev / bolt.new / lovable 已验证"代码即设计稿"
2. **天然版本管理**（跟代码一套机制）
3. **可预览**（iframe 渲染）
4. **跟前端 Agent 衔接零摩擦**

辅助：图像生成 MCP 生成 mood board / icon。可选：Figma MCP 作为高级模式。

### 7.3 UI 工件 schema

```yaml
artifact:
  type: ui_design
  owner_role: UI
  content_format: html_css        # 关键字段
  preview_renderer: iframe
  versions:
    v_n:
      files: [...html, ...css, assets/]
      design_tokens: { colors, typography, spacing }  # 抽出供前端对齐
      preview_screenshot               # 缓存截图，群聊气泡里显示
  references: [mood_board.png, ...]    # 灵感参考
```

### 7.4 UI 工件查看：多 Tab
Preview（iframe + 视口切换）/ Code / Tokens / Assets / Diff vs v_prev（左右并排预览）

### 7.5 UI → 前端的标准 SOP（写进工作室定义）
1. 前端 `get_artifact("ui-...")` 拿 HTML/CSS + design_tokens
2. 提取 design_tokens 写入 Tailwind config
3. HTML 结构转译为 React 组件
4. UI 工件作为"视觉真理源"，开发完成对比验证

---

## 第八部分 · 存储架构【关键】

### 8.1 决策：纯文件架构，零数据库
按目录结构隔离，按文件类型选格式。

### 8.2 目录结构

```
~/.aidock/
├── config.json                       # 全局配置
├── studios/{id}/                     # 工作室定义（可上架）
│   ├── definition.yaml
│   └── role_prompts/*.md
├── sessions/
│   ├── _index.json                   # 所有会话索引
│   └── {session-id}/
│       ├── meta.json
│       ├── state.json                # session_state
│       ├── messages.jsonl            # 单文件！所有消息，每行带 topic_id
│       ├── topics.json               # topic 元数据
│       ├── proposals/
│       └── artifacts/{id}/
│           ├── meta.json             # 版本列表 + current_locked
│           └── v{n}.md               # 不可变版本文件
└── agent_notes/{studio-id}/{role}.yaml    # 跨会话私人笔记
```

### 8.3 关键决策：消息单文件 + 行内 topic_id【关键】

用户明确纠正过的设计：
- 整个会话所有消息在**一个** `messages.jsonl`，每行一条 JSON
- 消息行内带 `topic_id` 字段
- 查询全靠 grep / ripgrep：`grep '"topic_id":"t-001"' messages.jsonl`
- 10 万条消息 grep 也是几十毫秒，**不需要按 topic 切文件，不需要数据库索引**

为什么单文件优于多文件：
1. 单一真相，备份/导出/上传市场一把抓
2. 跨 topic 天然时间序，便于回放/审计
3. append 极简，不需判断写到哪
4. 零索引同步问题

### 8.4 格式约定
- 消息流 → **JSONL**（一个文件，append-only）
- state / meta / 索引 → **JSON**（整体读写）
- 工作室定义、私人笔记 → **YAML**（人类可读编辑）
- 工件内容 → **Markdown / 代码原生格式**

### 8.5 关键工程细节【风险】
1. 并发写 state.json → 文件锁（fs2 crate）+ 原子 rename（写 tmp 再 rename）
2. JSONL 写入崩溃 → fsync + 启动时跳过损坏的最后一行
3. API Key → 用系统 keyring（macOS Keychain / Win Credential Manager），**不落文件**

---

## 第九部分 · 工具系统：MCP

### 9.1 协议选择【关键】
**采用 MCP (Model Context Protocol)**——Anthropic 开源标准，完全免费。
- 不自造协议
- 已有几十个开源 MCP server（filesystem / git / github / shell / sqlite 等）
- 用户可装新 server 扩展能力（这本身就是网络效应入口）

### 9.2 工作目录绑定
- 新建会话时用户选择项目目录
- session_state 记录 `project_root`
- 所有 Agent 在此目录范围内工作

### 9.3 角色级权限
工作室定义里每个角色声明能用哪些工具：
```yaml
PM:
  tools: [filesystem.read, web_search, artifact.write_prd]
前端工程师:
  tools: [filesystem.*, run_command, git.*, browser.preview]
```

### 9.4 危险操作审批
| 操作 | 默认行为 |
|---|---|
| 读文件 | 自动允许（在 project_root 内） |
| 写文件 | 自动允许（在 project_root 内） |
| 删除文件 | **弹窗审批** |
| 运行 shell 命令 | **弹窗审批**（首次） |
| git push | **弹窗审批** |
| 访问 project_root 外 | **弹窗审批** |

### 9.5 工具调用的 UI 透明性
Agent 用 MCP 工具时聊天里显示 `🔧 read_file('...')` 之类气泡，可点开看 diff/输出。**用户能完整看到、能审计**。

---

## 第十部分 · UI 设计

### 10.1 宏观布局：三栏类聊天工具
- 左栏：工作室列表 + 会话列表 + 市场入口
- 中栏：群聊主区（核心）
- 右栏：阶段进度 + 工件 + 成员状态 + 待审批提议

### 10.2 群聊核心交互【关键】

1. **非流式输出 + 后台并发思考 + 先到先现**【关键】
   - 多 Agent 同时思考，谁先完成谁先显示完整消息气泡
   - 反直觉但更优：多 Agent 场景流式输出反而是干扰
   - 需要"思考中..."状态占位避免显得卡死

2. **Topic 完全隐藏**【关键】
   - UI 上**不出现"topic"字眼**
   - 用户看到的是自然对话流
   - Topic 结束时显示分割线 + 一句话摘要（由 SUMMARY 消息自动生成）

3. **工件双层视图**
   - 主视图（locked 内容）+ 背后视图（Agent 思考过程、引用消息、修改历史）
   - 背后视图来自 Agent 工作台层记忆

4. **消息类型视觉区分**
   - BROADCAST：默认气泡
   - ASK_AGENT：带 @ 标签 + 箭头指向被问者
   - ANSWER：缩进 + 与 ASK 连接线
   - PROGRESS：带进度条卡片
   - WORK_START：浅色小条
   - DONE：高亮卡片 + 工件链接
   - SUMMARY：折叠灰底卡片，充当分割线

5. **工具调用全程可见**

### 10.3 新建会话 Onboarding（单屏对话框）
字段：工作室、会话名称、项目目录（含读写授权）、继承历史会话（可选）、初始 brief（可选）、是否让老板主动开口（默认开）

关键决策：
- 项目目录**根据工作室定义自动判断必填/可选**
- 历史会话引用：首版只支持单选某一次
- 空状态：**内置 1-2 个默认工作室**，零下载就能玩

### 10.4 提议审批的交互
- Agent 通过 PROPOSAL 消息发起；用户通过右栏点工件→"提议修改"发起
- 三处呈现：群聊特殊卡片消息 + 右栏"待审批"汇总 + owner 状态条高亮
- owner 接受 → 系统自动 superseded 旧版+创建 draft 新版+弹 split-view 编辑器+锁定+BROADCAST 通知

### 10.5 历史会话恢复 + 时间旅行
- 会话列表按状态分组（进行中/已完成/异常中断/归档）
- 异常中断打开时弹框：[继续/仅查看/新建分支]
- 会话内"时间轴控件"：顶部显示阶段进度，点已完成阶段进入"快照只读模式"
- 底层靠 `session_state.stage_history` 重构

### 10.6 市场 + 上架流程
- 市场浏览页 + 工作室详情页（含角色列表/阶段链可视化/MCP 列表/评论/版本历史）
- 购买后自动下载 `.aidock-pkg`→解压→进"我的工作室"
- 上架 5 步向导：基本信息→详细介绍→定价→法律声明→预览发布
- 创作者中心：销量/评论/版本管理/收益结算

---

## 第十一部分 · 工作室生命周期

### 11.1 流程【关键】
```
用户在 AiDock 平台内创建工作室
  ↓
保存到"我的工作室"（本地私人仓库）
  ↓
反复迭代调试
  ↓
用户主动选择"上架到市场"
  ↓
设置价格/介绍/试用条件 → 上传到云端
  ↓
他人购买 → 自动下载到他的"我的工作室"
```

### 11.2 用户视角的分类
- 我的工作室（自己创建的私有）
- 我的工作室（自己创建的已上架）
- 我的工作室（从市场购买的）
- 浏览市场

### 11.3 流转格式
底层 `.aidock-pkg`（zip 包，含 manifest/definition/role_prompts/artifact_templates/icon/README），**用户不直接接触**，只在平台上传/下载时由系统处理。

---

## 第十二部分 · 技术栈

### 12.1 架构方向：混合（C 方案）【关键】
- 本地客户端跑工作室和 Agent
- 云端做市场、登录、可选同步
- 用户数据/代码/API Key 留本地，市场和交易在云端

### 12.2 客户端
- **Tauri 2.x + Rust + Svelte 5 + TypeScript**（已落地）
- **关键 Rust crate**：tokio / reqwest / serde / rmcp (MCP) / keyring / fs2 / tracing
- **LLM Provider 抽象**：`LLMProvider` trait（chat_completion / supports_tool_use / provider_id），V0.1 只实现阿里百炼，后续 Anthropic / OpenAI / 本地模型按需加
  - **主 provider**：阿里百炼 (Bailian)，OpenAI 兼容端点 `https://dashscope.aliyuncs.com/compatible-mode/v1`
  - **主模型族**：Qwen
    - `qwen3-max-2026-01-23` — 旗舰推理（技术总监、调解员）
    - `qwen3.7-max` — 代码生成主力（前端 / 后端 / 移动端）
    - `qwen3.6-plus` — 对话/平衡（老板、PM、UI、测试、DevOps；V0.1 三角色统一用此模型作 baseline）
  - **非主推理模型**（角色可作为工具调用）：vision `qwen3-vl-plus` / 图像生成 `wan2.7-image-pro` / 视频 `happyhorse-1.0-t2v`
- Tool use 协议：Bailian 用 OpenAI function calling 格式（不是 Anthropic 风格）。Sprint 1 末必须做 tool use 稳定性测试，确认 Qwen 输出结构化 JSON 的可靠性，Sprint 2 多 Agent 机制依赖此。

### 12.3 云端
- **Next.js + Supabase**（Auth/Postgres/Storage 一站式）
- 支付：Stripe（海外）/ 微信+支付宝（国内）

### 12.4 工具协议
- **MCP**（filesystem/git/shell/...）

### 12.5 存储
- 纯文件（无数据库）
- 系统 keyring 管 API Key

---

## 第十三部分 · MVP V0.1【关键】

### 13.1 核心要验证的假设【关键】
> **多 Agent 在群聊里用结构化消息协作，能比单 Agent 完成更复杂任务**

如果这个假设不成立，整个产品不成立。MVP 必须验证 #1，其他假设可推迟。

### 13.2 V0.1 范围（修订后）

**必做**：
- 1 个写死的内置工作室（"软件开发轻量版"，3 角色：PM/前端/后端）
- 类型化消息（BROADCAST/ASK_AGENT/ANSWER/DONE/**SUMMARY**）
- IDLE/WORKING/WAITING_ANSWER 状态机
- 后台并发 + 先到先现
- **Topic 抽象 + 自动 SUMMARY + 三级渐进式上下文加载 + recall_topic/search_topic tool**【关键 — 不能省】
- **Agent 工作台**（运行时记录"我改了什么"）
- MCP filesystem 接入 + 危险操作弹窗审批
- BYOK（keyring 加密）
- 本地 messages.jsonl + state.json
- 单会话、单工作室、三栏极简 UI

**故意不做**：
工作室编辑器、阶段链 (Stages)、工件版本/提议审批、多会话/会话列表、时间旅行、私人笔记、跨会话引用、RAG codebase 检索、Few-shot 动态注入、云端、Onboarding 向导

### 13.3 为什么上下文策略必须 V0.1 就做【关键】
持续对话天然要求 Agent 记得自己刚改了什么。如果 Agent 是"金鱼记忆"，多 Agent 协作根本跑不起来——后端工程师无法持续工作。"先找目录、再找简介、不够再看具体内容"是核心信息架构模式，V0.1 必须建立，否则 V0.2 加上去要全面重构。

### 13.4 Sprint 拆分（11-14 周全职 / 6-7 个月业余）

| Sprint | 时长 | 范围 |
|---|---|---|
| **Sprint 0** | 1 周 | 脚手架：Tauri 项目初始化 + 三栏布局壳 + IPC 通信 |
| **Sprint 1** | 1-2 周 | 单 Agent 端到端闭环：BYOK + Bailian (Qwen) API + 单对单聊天 + tool use 稳定性测试 |
| **Sprint 2** ⚠️ 最难 | **4 周** | 多 Agent runtime + 上下文核心机制（类型化消息、状态机、路由、并发、3 角色、ASK_AGENT 接力、Topic、SUMMARY 自动生成、Level 0 上下文构造器、recall/search tool、Agent 工作台） |
| **Sprint 3** | 1-2 周 | 持久化：messages.jsonl + state.json 原子读写 + 重启恢复 |
| **Sprint 4** | 2 周 | MCP 集成 + filesystem server + 工具调用 UI + 危险操作审批 |
| **Sprint 5** | 1-2 周 | 打磨 + 跨平台打包 + 内测验证核心假设 |

### 13.5 关键风险【风险】

| 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|
| **多 Agent 协作效果不达预期** | 中 | **致命** | Sprint 2 完成立即自测，不要拖到 Sprint 5 |
| rmcp 库不够成熟 | 中 | 高 | Sprint 4 前先调研，必要时自己封 JSON-RPC |
| Tauri 2.x 学习成本 | 视经验 | 中 | Sprint 0 留 1 周专门踩坑 |
| 跨平台打包问题 | 中 | 中 | Sprint 5 留够时间 + CI 三平台 build |

### 13.6 V0.1 完成后必须做的事【关键】
1. 找 5-10 个 AI 工程师试用真实项目
2. 让他们用 AiDock 开发小项目，对照组用 Cursor / Claude Code
3. 观察：协作真的更强吗？哪些场景 Agent 会"打架"？用户什么时候被惹烦？
4. **判断**：多数说"协作有惊喜" → 继续 V0.2；多数说"不如直接用 Cursor" → 深度反思

### 13.7 演进路线
- **V0.2**：阶段链 + 工件版本（不带提议审批）
- **V0.3**：工作室定义编辑器 + 多内置工作室
- **V0.4**：多会话 + 会话列表 + 中断恢复
- **V0.5**：私人笔记 + 提议审批 + 时间旅行
- **V0.6**：云端账号 + 工作室上传/下载
- **V0.7**：市场页面 + 免费分发
- **V1.0**：支付 + 收益结算 + 完整上架流程

---

## 第十四部分 · V1.0 旗舰内置工作室

### 14.1 软件开发工作室【关键】

**默认启用 7 个角色**：
- 老板（跟用户对接需求）
- 产品总监（写 PRD）
- 技术总监（技术决策、叫停）
- UI 设计
- 后端开发
- 前端开发
- 测试

**可选启用 4 个角色**（新建会话时勾选）：
- 小程序开发
- iOS 开发
- Android 开发
- DevOps

**说明**：
- "测试开发"和"测试"合并为"测试"角色
- 移动端拆 3 个 + DevOps 设为可选，避免默认 11 角色的认知负担和 V1.0 上线时的 prompt 工程压力
- token 不是限制原因（已被设计 cover）

---

## 第十五部分 · 核心设计原则【关键】

这些是在讨论过程中沉淀下来的、必须贯穿后续所有决策的原则：

### 15.1 产品行为先于技术栈
讨论任何技术选型前，先把产品行为模型谈清楚：(1) 核心实体如何协作 (2) 状态/记忆如何流动 (3) 用户如何感知与操作。三件套不清晰之前，不要谈语言、框架、部署形态、数据库选型。

### 15.2 不要给出与已定设计自相矛盾的论据
给出反对/支持的论据前，先回顾已定的机制（消息类型化、状态机、渐进式加载、相关性过滤、工件锁定、角色绑定、MCP 工具系统、4 层记忆、纯文件存储）是否已 cover 该问题。

### 15.3 UI 样式由用户后期调整
讨论 UI 时聚焦"承载什么数据"和"用户怎么操作"，少在按钮位置、颜色、字号上展开。本质（数据 + 流程）正确，样式让用户自己迭代。

### 15.4 个人开发者的硬约束
- 零运维优先
- 不扛云端算力（用户 BYOK）
- 不依赖复杂基础设施（不用向量库 / 不用 SQLite）
- 用成熟开源（Tauri / MCP / Supabase）

### 15.5 "完全自由组建"是卖点底线
任何技术决策不能往"模板化"方向带。阶段链、角色、工件、工具都必须是用户可配置的。

---

## 附录 · 关键决策一览表

| 决策点 | 选择 | 理由 |
|---|---|---|
| 商业模式 | Open Core + 市场抽成 | 个人开发者起步、市场形成网络效应 |
| 部署形态 | 混合（本地客户端 + 云端市场） | 隐私 + 零执行成本 + 网络效应 |
| 心智隐喻 | 群聊 + 公司组织 | 零学习成本 + 多 Agent 协作隐喻清晰 |
| Agent 协作 | 管家-工人 + 用户自由编排 | 既有秩序又有自由度 |
| 路由 | @ 触发 | 简单清晰，零歧义 |
| 消息形态 | 类型化结构指令 | 解决礼貌循环 |
| 状态管理 | IDLE/WORKING/WAITING_ANSWER | 防打断、保证专注 |
| 叫停权 | 阶段主持人 + 用户上帝 + 自动监控兜底 | 多层防失控 |
| 上下文 | 三级渐进式加载 | 不需向量库、按需翻笔记 |
| 跨工作室 | 完全隔离 | 用户硬约束 |
| 跨会话学习 | 整合进 Agent 私人笔记 | 不做向量检索 |
| 工件写权限 | 角色绑定 + 提议-审批 | 模拟真实公司 |
| 存储 | 纯文件，无数据库 | 个人开发者友好、备份简单 |
| 消息存储 | 单 messages.jsonl + grep 查询 | 简单粗暴够用 |
| 工具协议 | MCP | 标准、免费、生态好 |
| UI 形态 | 三栏类聊天工具 | 用户熟悉、信息密度合适 |
| 输出模式 | 非流式 + 后台并发 + 先到先现 | 多 Agent 场景反直觉但更优 |
| Topic 显示 | UI 完全隐藏 | 用户不应感知抽象概念 |
| UI 工件 | HTML/CSS 代码 | 跟前端零摩擦衔接 |
| 客户端栈 | Tauri 2.x + Rust + Svelte | 包小、性能好、跨平台 |
| 云端栈 | Next.js + Supabase | 个人开发者神器 |
| 加密 Key | 系统 keyring | 不落文件 |
| V0.1 范围 | 3 角色 + 完整上下文机制 + MCP filesystem | 验证核心假设 + 上下文是基础设施 |
| V1.0 旗舰 | 软件开发工作室 7+4 角色 | 默认 7 控制复杂度，可选 4 保留扩展 |

---

**文档结束。** 此文档应作为后续实施过程中的设计真理源（Source of Truth）。任何偏离本文档关键决策的改动，都应该回到本文档讨论清楚再修改。
