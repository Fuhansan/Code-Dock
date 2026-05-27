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

pub mod message;
pub mod role;
pub mod state;
pub mod topic;

pub use message::{AgentId, AgentMessage, AgentMessageKind, MessageId, TopicId};
pub use role::{Budget, LoopMode, ModelConfig, RoleConfig};
pub use state::AgentState;
pub use topic::{Topic, TopicIndexEntry, TopicStatus};
