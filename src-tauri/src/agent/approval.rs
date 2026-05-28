//! User-approval gate for destructive MCP tool calls.
//!
//! Sprint 4.6 design. The filesystem MCP server already enforces an
//! allowed-directory boundary, which catches the truly dangerous case
//! (writes outside the workspace). The approval gate adds a finer layer:
//! the user gets to vet writes / edits / moves / dir-creates **inside**
//! the workspace too, so an agent can't silently overwrite their work.
//!
//! Layering:
//! - `Danger` classifies a tool by intent — read / mutating / destructive.
//!   Read-only tools never gate.
//! - `ApprovalRegistry` is per-session in-memory state of which
//!   (agent, tool) combinations the user has already approved or rejected,
//!   with a `Scope` (once vs. session). The runtime queries the registry
//!   before each call; on `Unknown` it raises a request to the frontend
//!   and awaits the answer on a oneshot channel.
//! - V0.1 deliberately doesn't persist approvals across restarts. Each
//!   fresh session re-asks. Sprint V0.x can add persistence.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex};

/// Tauri event name the runtime emits when it needs the user's vote on
/// a destructive tool call. Frontend should respond via the
/// `respond_to_approval` Tauri command with the matching `id`.
pub const APPROVAL_REQUEST_EVENT: &str = "aidock:approval_request";

/// How long a pending approval waits before being auto-rejected. The user
/// AFK shouldn't leave an agent task wedged forever.
pub const APPROVAL_TIMEOUT_SECS: u64 = 60;

/// Payload the frontend sees for an approval prompt.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub agent: String,
    pub tool: String,
    pub args_preview: String,
    /// Unix milliseconds; UI may show "Xs ago" or auto-dim stale prompts.
    pub timestamp: i64,
    pub danger: Danger,
}

/// Map shared between the runtime (registers `oneshot::Sender`s) and the
/// `respond_to_approval` Tauri command (fulfils them). Cheap to clone.
pub type PendingApprovals = Arc<Mutex<HashMap<String, oneshot::Sender<Decision>>>>;

pub fn new_pending_approvals() -> PendingApprovals {
    Arc::new(Mutex::new(HashMap::new()))
}

/// What kind of risk a tool carries. Mapping is per-tool, hard-coded for
/// the V0.1 filesystem server. Adding new MCP servers later means
/// extending `classify`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Danger {
    /// Pure read or information — never gated.
    ReadOnly,
    /// Mutating but recoverable / scoped: create or write a file inside
    /// the workspace. The bulk of code-writing falls here.
    Mutating,
    /// Destructive: rename, move, delete. Treated identically to mutating
    /// in V0.1 (the user is asked) but kept separate so future policies
    /// can differentiate.
    Destructive,
}

impl Danger {
    pub fn requires_approval(self) -> bool {
        !matches!(self, Danger::ReadOnly)
    }
}

/// Classify a tool by its LLM-side name (already prefixed with `fs__`
/// by `agent::mcp::project_to_llm_tool`).
///
/// The reference filesystem MCP server (`@modelcontextprotocol/server-filesystem`)
/// advertises 14 tools. Everything not explicitly listed here defaults
/// to `Mutating` — better to over-ask than to silently let a future
/// tool through.
pub fn classify(tool_name: &str) -> Danger {
    let bare = tool_name.strip_prefix("fs__").unwrap_or(tool_name);
    match bare {
        // Reads — never gated.
        "read_file"
        | "read_text_file"
        | "read_media_file"
        | "read_multiple_files"
        | "list_directory"
        | "list_directory_with_sizes"
        | "directory_tree"
        | "search_files"
        | "get_file_info"
        | "list_allowed_directories" => Danger::ReadOnly,

        // Mutations: create / write content.
        "write_file" | "edit_file" | "create_directory" => Danger::Mutating,

        // Destructive: rename or relocate. (delete_file is not in the
        // current reference server set but we keep the slot for when it
        // is.)
        "move_file" | "delete_file" => Danger::Destructive,

        // Default for anything we don't know yet: ask the user.
        _ => Danger::Mutating,
    }
}

/// User's verdict on a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Approve just this call. Future calls of the same tool by the same
    /// agent re-prompt.
    AllowOnce,
    /// Approve this and remember for the rest of the session.
    AllowSession,
    /// Reject this call. The runtime feeds the rejection back to the
    /// agent as the tool result; future calls re-prompt.
    Reject,
}

impl Decision {
    pub fn allows_execution(self) -> bool {
        matches!(self, Decision::AllowOnce | Decision::AllowSession)
    }
    pub fn is_remembered(self) -> bool {
        matches!(self, Decision::AllowSession)
    }
}

/// What the registry tells the runtime about a pending call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupResult {
    /// No gating needed — proceed.
    NoGate,
    /// Previously remembered approval; proceed.
    Approved,
    /// Previously remembered rejection; reject this call too.
    Rejected,
    /// No prior decision — caller must ask the user.
    AskUser,
}

/// Per-session memory of which (agent, tool) pairs have been
/// session-approved or session-rejected. Reset when the session restarts.
///
/// `Arc<Mutex<…>>` wrapper so the dispatcher + agent runtimes can share
/// one instance without ownership wrangling.
#[derive(Debug, Default, Clone)]
pub struct ApprovalRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Debug, Default)]
struct RegistryInner {
    /// (agent_id, tool_name) → remembered decision.
    /// Only `AllowSession` and `Reject` ever land here; `AllowOnce` is
    /// not stored (one-shot semantics).
    decisions: HashMap<(String, String), Decision>,
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// What should happen for an upcoming call from `agent` to `tool`?
    pub async fn lookup(&self, agent: &str, tool: &str) -> LookupResult {
        match classify(tool) {
            Danger::ReadOnly => return LookupResult::NoGate,
            _ => {}
        }
        let inner = self.inner.lock().await;
        match inner.decisions.get(&(agent.to_string(), tool.to_string())) {
            Some(Decision::AllowSession) => LookupResult::Approved,
            Some(Decision::Reject) => LookupResult::Rejected,
            // AllowOnce isn't supposed to land in the map, but be defensive.
            Some(Decision::AllowOnce) => LookupResult::AskUser,
            None => LookupResult::AskUser,
        }
    }

    /// Record a remembered decision for `(agent, tool)`. `AllowOnce`
    /// is intentionally a no-op since it's a per-call decision.
    pub async fn remember(&self, agent: &str, tool: &str, decision: Decision) {
        if !decision.is_remembered() && !matches!(decision, Decision::Reject) {
            return;
        }
        let mut inner = self.inner.lock().await;
        inner
            .decisions
            .insert((agent.to_string(), tool.to_string()), decision);
    }

    /// Test/debug — clears everything. Not exposed via Tauri command.
    #[allow(dead_code)]
    pub async fn clear(&self) {
        self.inner.lock().await.decisions.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_tools_are_read_only() {
        for n in [
            "fs__read_file",
            "fs__list_directory",
            "fs__search_files",
            "fs__list_allowed_directories",
        ] {
            assert_eq!(classify(n), Danger::ReadOnly, "{n}");
            assert!(!classify(n).requires_approval(), "{n}");
        }
    }

    #[test]
    fn write_tools_are_mutating() {
        for n in ["fs__write_file", "fs__edit_file", "fs__create_directory"] {
            assert_eq!(classify(n), Danger::Mutating, "{n}");
            assert!(classify(n).requires_approval(), "{n}");
        }
    }

    #[test]
    fn move_and_delete_are_destructive() {
        assert_eq!(classify("fs__move_file"), Danger::Destructive);
        assert_eq!(classify("fs__delete_file"), Danger::Destructive);
    }

    #[test]
    fn unknown_tool_defaults_to_mutating() {
        // Better-safe-than-sorry: future unknown tools require approval.
        assert_eq!(classify("fs__some_future_tool"), Danger::Mutating);
        assert_eq!(classify("no_prefix_at_all"), Danger::Mutating);
    }

    #[tokio::test]
    async fn lookup_read_only_never_gates() {
        let r = ApprovalRegistry::new();
        assert_eq!(
            r.lookup("PM", "fs__read_file").await,
            LookupResult::NoGate
        );
    }

    #[tokio::test]
    async fn lookup_mutating_unknown_asks_user() {
        let r = ApprovalRegistry::new();
        assert_eq!(
            r.lookup("frontend_dev", "fs__write_file").await,
            LookupResult::AskUser
        );
    }

    #[tokio::test]
    async fn remembered_approval_short_circuits() {
        let r = ApprovalRegistry::new();
        r.remember("frontend_dev", "fs__write_file", Decision::AllowSession)
            .await;
        assert_eq!(
            r.lookup("frontend_dev", "fs__write_file").await,
            LookupResult::Approved
        );
        // But the same call by a *different* agent still asks.
        assert_eq!(
            r.lookup("backend_dev", "fs__write_file").await,
            LookupResult::AskUser
        );
    }

    #[tokio::test]
    async fn remembered_rejection_short_circuits() {
        let r = ApprovalRegistry::new();
        r.remember("backend_dev", "fs__move_file", Decision::Reject)
            .await;
        assert_eq!(
            r.lookup("backend_dev", "fs__move_file").await,
            LookupResult::Rejected
        );
    }

    #[tokio::test]
    async fn allow_once_is_not_remembered() {
        let r = ApprovalRegistry::new();
        r.remember("PM", "fs__write_file", Decision::AllowOnce).await;
        // Next time round, we still ask.
        assert_eq!(
            r.lookup("PM", "fs__write_file").await,
            LookupResult::AskUser
        );
    }
}
