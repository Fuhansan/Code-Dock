//! Multi-Agent runtime building blocks.
//!
//! Sprint 2 lands here piece by piece:
//! - `message` — typed kinds flowing through the group chat (Sprint 2.1)
//! - `topic`   — coarse conversation segments (Sprint 2.1)
//! - `state`   — IDLE / WORKING / WAITING_ANSWER state machine (Sprint 2.1)
//! - `role`    — RoleConfig multi-dim definition (Sprint 2.1, populated 2.2)
//! - `roles`   — hardcoded V0.1 roles + their tool schemas (Sprint 2.2)
//! - `dispatcher` — central message router (Sprint 2.3)
//! - `runtime` — per-Agent tokio task driving the state machine (Sprint 2.4)
//! - `context` — Level 0 prompt builder (Sprint 2.5)
//! - `scratchpad` — per-Agent working memory (Sprint 2.8)

pub mod approval;
pub mod context;
pub mod dispatcher;
pub mod fs_tools;
pub mod llm_source;
pub mod lsp;
pub mod lsp_client;
pub mod lsp_pool;
pub mod memory;
pub mod mcp;
pub mod mcp_executor;
pub mod mcp_log;
pub mod message;
pub mod persistence;
pub mod protocol;
pub mod react;
pub mod recall;
pub mod role;
pub mod roles;
pub mod runtime;
pub mod scratchpad;
pub mod security;
pub mod session;
pub mod shell;
pub mod state;
pub mod tools;
pub mod topic;
pub mod turn_bridge;

pub use dispatcher::{Dispatcher, DispatcherError, DispatcherHandle, MESSAGE_EVENT};
pub use message::{AgentId, AgentMessage, AgentMessageKind, MessageId, TopicId};
pub use protocol::{message_tools, parse_tool_call, ParsedToolCall, ProtocolError};
pub use role::{Budget, LoopMode, ModelConfig, RoleConfig};
pub use roles::{default_workshop, role_by_id, BACKEND_ID, FRONTEND_ID, PM_ID};
pub use session::{Session, SessionError, DEFAULT_TOPIC_ID};
pub use state::AgentState;
pub use topic::{Topic, TopicIndexEntry, TopicStatus};
