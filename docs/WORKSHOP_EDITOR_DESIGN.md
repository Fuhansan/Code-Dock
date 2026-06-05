# 工作室 / 角色编辑器 — 设计稿

> 目标：把"工作室由用户自由组建、角色由用户定义"从愿景落成可操作的功能。
> 本稿只定**设计**（数据 + UI + 契约 + 阶段），不含实现代码。

## 0. 对齐既有设计（先读）

- `AIDOCK_DESIGN.md §3.2`：**定义 vs 会话** = Class vs Instance。本编辑器编的是**工作室定义（Studio Definition）**——静态配置，可复用、可上架市场。
- `AIDOCK_DESIGN.md §5`：**一个 Role 绝不只是 prompt + LLM**，是多维配置对象（模型/参数/工具/输出约束/上下文/循环/预算/few-shot/知识源/子 Agent）。这是"卖创意"的真正载体。
- `CLAUDE.md ③`：当前已落地的三层容纳 `~/.aidock/users/{user}/workshops/{ws}/sessions/{session}/`（比 `AIDOCK_DESIGN §8.2` 的旧布局新，以此为准）。
- `CLAUDE.md ④ / role.rs`：`RoleConfig` 数据模型已大体齐备；本轮刚把 `is_coordinator`（协调者）做成一等字段。
- `CLAUDE.md ④.d`：进工作室选一个**可信工作区间**，改/删工具的路径不得越界（L3 红线）。

## 1. 已敲定的产品决策（用户确认）

1. **角色全属性可编辑**（身份/模型/工具/安全/预算/高级）。
2. **两个场景**：
   - **个人工作室工作台（Workbench）**：管理**所有**工作室与角色，做各种编辑（顶层管理页）。
   - **工作室内（In-workshop）**：现有聊天界面，只面对**当前**工作室。
3. **立即生效**：保存后热应用到正在跑的会话。
4. **prompt 拆分**：`人设（可编）` + `协作协议铁律（运行时注入、不可编）`——防止用户把多 agent 协议改坏。
5. **工作区间 = 工作室级设置**（进工作室选一次，全员共享一条边界）。

## 2. 术语

| 词 | 含义 |
|---|---|
| 工作室定义 Studio Definition | 一个工作室的静态配置：名称/图标/工作区间 + 一组角色。Class。 |
| 角色 Role (`RoleConfig`) | 工作室里一个 AI 成员的多维配置。 |
| 协调者 Coordinator | 承接用户对话、调度团队的那个角色（全室唯一，`is_coordinator`）。 |
| 工作台 Workbench | 管理所有工作室/角色的顶层页。 |
| 会话 Session | 基于工作室定义开启的一次任务实例。Instance（已实现）。 |

---

## 3. 数据模型与存储

### 3.1 落盘布局（沿用现三层，新增 `workshop.json`）

```
~/.aidock/users/{user}/workshops/{ws_id}/
    workshop.json        ← 新增：工作室定义
    sessions/{id}/       ← 不变（会话）
    agents/.../memory/   ← 不变（④.b 记忆，随会话/工作室隔离）
```

### 3.2 `workshop.json` schema

```jsonc
{
  "id": "ws-default",
  "name": "Coding 工作室",
  "icon": "🧭",
  "workspace_path": "/Users/me/projects/foo",   // ④.d 可信工作区间（全室共享）
  "roles": [ RoleConfig, RoleConfig, ... ]        // 顺序即展示顺序
}
```

- 存 **JSON**（与现有 `persistence.rs`/`session_store` 一致，零新依赖；`RoleConfig` 已 `Serialize/Deserialize`）。
  > 注：`AIDOCK_DESIGN §8.4` 偏好工作室定义用 YAML（便于手改/上架）。因编辑全走 UI，V1 用 JSON；将来要"导出上架"再加 YAML 导出（P4）。

### 3.3 `RoleConfig` 字段 → 编辑器暴露面

现有字段（`role.rs`）：

| 字段 | 编辑器分段 | 控件 |
|---|---|---|
| `display_name` / `description` | 基础 | 文本 |
| `avatar`（图标+底色，**新增**） | 基础 | emoji/字形 + 取色 |
| `is_coordinator` | 基础 | 单选（全室唯一） |
| `persona`（**人设**，由 `system_prompt` 拆出） | 基础 | 多行文本 |
| `model.{primary,temperature,max_tokens}` | 基础 | 下拉/滑块/数字 |
| `tools`（Vec<String>） | 基础 | **按族勾选**（见 3.5） |
| `security_level` | 基础 | 严格/标准/宽松 单选 |
| `budget.{per_call_tokens,per_session_tokens,per_session_calls}` | 高级 | 数字 |
| `model.fallback` / `model.extended_thinking` | 高级 | 下拉/开关 |
| `loop_mode` | 高级 | 下拉（single/react/plan_execute） |
| `max_history_tokens` | 高级 | 数字 |
| `permission_rules`（specifier） | 高级 | 规则列表编辑 |
| `teammates` | 高级 | 多选（其余角色），亦可由"同室成员"自动派生 |

> `AIDOCK_DESIGN §5.2` 还列了 `few_shot_examples` / `output_format` / `rag_sources` / `sub_agents` / `max_iterations`——`RoleConfig` 暂无，列入 **P4 未来字段**，不阻塞 V1。

### 3.4 加载 / 种子 / 迁移

- `load_workshop(user, ws)`：读 `workshop.json`；**不存在则用内置 `roles::default_workshop()` 做种子写入**（现有 `ws-default` 首次加载自动迁移，行为不变）。
- `roles.rs::default_workshop()` 由"运行时来源"退成"**新建工作室的种子模板**"。
- `Session::start` 改为吃 `load_workshop(...).roles` 而非硬编码。

### 3.5 工具勾选的来源

工具目录来自 `tools.rs` 的注册表，按族分组给 UI 勾选：

| 族 | 成员 |
|---|---|
| 文件 | Read / Edit / Write / Glob / Grep |
| 终端 | Bash / BashOutput / KillShell |
| 导航 | LSP |
| 记忆 | mem_recall / topic_recall / topic_browse / update_scratchpad |
| 协作 | BROADCAST/ASK_AGENT/ANSWER/WORK_START/PROGRESS/DONE/SUMMARY/set_plan（**始终开**，非可选——协议工具） |

> 勾选即"三层防御 layer 2"：工具不在目录里就 advertise 不出去，结构上够不着。改工具 = 直接改角色能力边界。

---

## 4. prompt 拆分设计

```
喂给 LLM 的 system =  persona(用户可编)  +  shared_rules()(协议铁律, 注入)  +  team_block(同室成员, 注入)
                     └ 存在 RoleConfig    └ 运行时拼, 用户看不到也改不了
```

- 编辑器里"人设 Prompt"= `persona`：只写该角色的职责/风格/口吻。
- `shared_rules()`（ReAct 回合、BROADCAST/ASK/DONE 用法、topic 纪律…）+ `team_block` 仍在运行时拼装注入，用户改不动 → 协作协议永不被污染（对齐 `CLAUDE.md ④` 身份三层防御 layer 1「身份稳、不被污染」）。
- **迁移**：把现有 `pm_system_prompt()` 等里"角色专属那段"抽成 `persona`，公共规则继续由 `shared_rules()` 注入（P1 重构）。

---

## 5. UI 设计

### 5.1 信息架构（新增顶层导航）

```
启动 → 工作台(Workbench)  ──[进入工作室]──▶  工作室内(现聊天界面)
        ▲                                         │
        └──────────────[返回工作台]───────────────┘
```

当前 app = "工作室内"。新增"工作台"顶层页 + 两者切换。

### 5.2 工作台（顶层首页）

```
┌──────────────────────────────────────────────────────────┐
│ AiDock                                  [🔍] [⚙] [👤]      │
│                                                            │
│ 我的工作室                               [＋ 新建工作室]    │
│ ┌───────────┐ ┌───────────┐ ┌───────────┐                │
│ │🧭 Coding   │ │🎬 短视频   │ │   ＋       │                │
│ │3 名成员    │ │4 名成员    │ │ 从模板/   │                │
│ │PM·前·后    │ │…          │ │ 市场创建  │                │
│ │[进入][编辑]│ │[进入][编辑]│ │           │                │
│ └───────────┘ └───────────┘ └───────────┘                │
│                                                            │
│ 市场（即将）  精选工作室模板…                               │
└──────────────────────────────────────────────────────────┘
```

### 5.3 工作室编辑页

```
┌─ 编辑工作室：Coding ───────────────────────[取消][保存]─┐
│ 工作室设置                                               │
│   名称 [Coding 工作室   ]   图标 [🧭]                    │
│   工作区间(④.d) [/Users/me/projects/foo  ] [选择…]      │
│   协调者(对接用户)  (●)PM  ( )…    ← 全室唯一           │
│ 成员 (3)                              [＋ 添加角色 ▾]    │
│  ┌─────────────────────────────────────────────────┐  │
│  │🧭 项目经理 PM  [对接]  协调·只读工具    [编辑][×] │  │
│  │</> 前端工程师          读写+Bash+LSP    [编辑][×] │  │
│  │🗄 后端工程师           读写+Bash+LSP    [编辑][×] │  │
│  └─────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

### 5.4 角色编辑器（抽屉/弹窗，基础/高级分段）

```
┌─ 编辑角色：项目经理 (PM) ──────────────[取消][保存]─┐
│ ▾ 基础                                              │
│   头像[🧭][底色●]  名称[项目经理]                    │
│   描述[统筹规划，承接需求…]                           │
│   ☑ 设为协调者（承接用户对话，全室唯一）             │
│   人设 Prompt（你的职责/风格；协作协议由系统注入）    │
│   ┌──────────────────────────────────────────────┐│
│   │ You are the Product Manager…                  ││
│   └──────────────────────────────────────────────┘│
│   模型[qwen3.6-plus ▾]  温度[───●──]0.3  max[4096]  │
│   工具(能力)                                        │
│     文件 ☑Read ☐Edit ☐Write ☑Glob ☑Grep           │
│     终端 ☐Bash   导航 ☐LSP   记忆 ☑recall          │
│     协作 (始终开)                                   │
│   安全级别 (●)标准 ( )严格 ( )宽松                  │
│ ▸ 高级                                              │
│   预算 per_call[8000] sess_tok[400000] calls[200]   │
│   fallback[无▾]  循环[single▾]  history_tok[32000]  │
│   权限规则 [+添加]  Read(*/.ssh/*)→拒  Bash(npm*)→免问│
│   teammates ☑前端 ☑后端                            │
└─────────────────────────────────────────────────────┘
```

### 5.5 工作室内「管理成员」

右栏「管理成员」→ 打开**同一个**工作室编辑页/角色编辑器，但只编**当前**工作室；保存 = **热应用**（见 §6）。

---

## 6. 立即生效（热应用）

复用已有的 `switch_to(同一个 session_dir)`：

```
保存角色/工作室配置  →  若该工作室有在跑会话  →  对当前会话重新 switch_to 自身
                       （drop 旧 Session → 按新角色 rehydrate 重启，从历史重建）
```

- 代价：丢"在飞的半圈回合"（与续接一致，可接受）。
- 无在跑会话时：下次进入自然加载最新，无需"应用"按钮。

---

## 7. ② 命令契约（新增）

| 命令 | 签名 | 说明 |
|---|---|---|
| `list_workshops` | `() -> Vec<WorkshopMeta>` | 工作台卡片墙（id/name/icon/member_count/workspace_path） |
| `get_workshop` | `(id) -> WorkshopDef` | 完整定义（含 roles） |
| `create_workshop` | `(name, icon, from_template?) -> WorkshopMeta` | 新建（可从种子/模板） |
| `delete_workshop` | `(id)` | 删除（含其会话，需确认） |
| `save_workshop_settings` | `(id, name, icon, workspace_path, coordinator_id)` | 工作室级设置 |
| `save_role` | `(workshop_id, role: RoleConfig)` | upsert 角色 + 热应用 |
| `delete_role` | `(workshop_id, role_id)` | 删角色 + 清 teammates 引用 |
| `enter_workshop` | `(id)` | 切换活跃工作室 + 启/续其会话 |
| `available_tools` | `() -> Vec<ToolCatalogEntry>` | 工具勾选目录（name/group/desc） |
| `model_catalog` | `() -> Vec<String>` | 可选模型 |
| `role_templates` | `() -> Vec<RoleConfig>` | 内置角色模板（P4） |

`②` 锁步 `ipc.ts`。

---

## 8. 责任块归位

| 块 | 改动 |
|---|---|
| ① UI | 工作台页、工作室编辑页、角色编辑器、顶层导航 |
| ② 契约 | 上表命令 + `ipc.ts` |
| ③ 运行时/生命周期 | `load_workshop` 取代硬编码；`AppState.active_workshop`（取代写死 `WORKSHOP`）；`enter_workshop` 切换；热应用 = `switch_to` 自身 |
| ④ 单 agent | `RoleConfig`（角色本体）；`persona/protocol` 拆分；工具门控仍是 layer 2 |
| ⑥ 持久化 | `workshop.json` 读写（原子写 + 种子迁移） |

> 跨块多——但这是"工作室编辑器"这一内聚特性的固有形态；按块分别改、契约（②）先对齐。

---

## 9. 风险 / 边界（实现时必须守）

1. **协调者全室唯一且必存在**：保存时若设了新协调者则取消其余；不允许零协调者；删协调者前必须改派。
2. **工作区间校验**（④.d）：`workspace_path` 须存在 + `canonicalize`；非法则拦保存或警告。改/删工具仍受 L3 路径校验。
3. **热应用丢在飞回合**：与续接一致，UI 给一句轻提示。
4. **删角色**：清掉其他角色 `teammates` 里的引用；若该角色正被某会话引用，热应用后从历史重建（不再 spawn 它）。
5. **persona 改不动协议**：靠 §4 拆分保证。
6. **多工作室隔离**：记忆/会话已随工作室目录隔离；切换活跃工作室走唯一口子（类比 `switch_to`）。
7. **删工作室**：连其 `sessions/` 一并删，需二次确认。

---

## 10. 分阶段实施

| 阶段 | 内容 | 产出 |
|---|---|---|
| **P1 存储层** | `WorkshopDef` + `workshop.json` + `load_workshop`（种子迁移）+ `AppState.active_workshop` 取代写死 `WORKSHOP` + `Session::start` 吃 `load_workshop` + **persona/protocol 拆分重构**。带测试，后端无感。 | 工作室从文件加载，行为不变 |
| **P2 工作室内角色编辑** | `get_workshop`/`save_role`/`delete_role`/`available_tools`/`model_catalog` 命令；「管理成员」→ 角色编辑器（全字段）→ 保存**热应用**。 | **编当前工作室角色闭环** |
| **P3 工作台** | `list/create/delete/enter_workshop`；工作台页 + 工作室编辑页 + 工作区间选择 + 顶层导航。 | 多工作室管理 |
| **P4 进阶** | 模板库/市场、`few_shot/rag/sub_agents/output_format`（§5.2 未来字段）、permission_rules 富编辑、头像自定义、YAML 导出上架。 | 对齐完整愿景 |

---

## 11. 未来（不阻塞 V1）

- 角色/工作室**市场**（Open Core 抽成）：导出 YAML/包，上架交易。
- §5.2 角色未来字段：few-shot、输出格式约束、RAG 知识源、子 Agent。
- 工作室"阶段链 / SOP"（`AIDOCK_DESIGN §7.5`）写进定义。
