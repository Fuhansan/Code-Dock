# AiDock

> 自由组建的 AI 工作室平台。用户在平台内创建由多个 AI 员工（Agent）组成的工作室，通过群聊形式协作完成代码开发、内容创作等真实任务。

## 当前状态

**V0.1 开发中**。核心目标：验证「多 Agent 在群聊里用结构化消息协作能比单 Agent 完成更复杂任务」这一核心假设。

## 设计文档

完整设计与开发计划在 `docs/`：

- [`docs/AIDOCK_DESIGN.md`](docs/AIDOCK_DESIGN.md) — 项目设计文档（商业模式 / 产品行为 / 记忆系统 / UI / 技术栈 / V1.0 路线）
- [`docs/DEVELOPMENT_PLAN.md`](docs/DEVELOPMENT_PLAN.md) — V0.1 开发计划（6 个 Sprint / 11-14 周）

任何与设计文档冲突的实施决策，必须先回到设计文档讨论清楚再修改。

## 技术栈

- **客户端**：Tauri 2.x + Rust + Svelte 5 + TypeScript + Tailwind
- **云端**（后续阶段）：Next.js + Supabase
- **协议**：Model Context Protocol (MCP)
- **存储**：纯文件（无数据库），系统 keyring 管 API Key

## 核心设计原则

1. 产品行为先于技术栈
2. 群聊 + 公司组织的心智隐喻
3. 工作室定义 vs 会话实例分层
4. 全部记忆不跨工作室
5. 「先找目录、再找简介、不够再看具体内容」的渐进信息架构
6. 个人开发者硬约束：零云端运维、BYOK、不扛云端算力

## 开发

(待补——Tauri 项目初始化完成后填写)
