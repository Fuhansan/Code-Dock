//! Per-agent scratchpad — private notes pinned to the LLM context every
//! turn.
//!
//! Per AIDOCK_DESIGN.md §6.2 row "Agent 工作台", this is the structured
//! short-term memory the runtime keeps on behalf of each agent. The agent
//! itself decides what's worth recording (via `update_scratchpad`); the
//! context builder injects the rendered form into the system prompt so the
//! agent always sees its own breadcrumbs.
//!
//! V0.1 scope: in-memory only, scoped to the agent task's lifetime. When
//! the session ends the scratchpad evaporates. Sprint 3 will serialize to
//! `sessions/{id}/scratchpads/{role}.json` for cross-session persistence.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::persistence::{atomic_write_json, read_json};
use crate::llm::Tool;

pub const TOOL_UPDATE_SCRATCHPAD: &str = "update_scratchpad";

/// What the agent has accumulated for itself this session. Default = empty
/// in every slot; rendering is a no-op until the agent puts something in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scratchpad {
    /// What the agent is focused on right now. Single string, replace on
    /// update (not append).
    pub current_focus: String,
    /// Sub-tasks the agent has carved out of the larger work.
    pub task_breakdown: Vec<String>,
    /// Files the agent has read or written, each with a short note.
    pub files_modified: Vec<FileChange>,
    /// Concrete decisions the agent has committed to.
    pub decisions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub note: String,
}

impl Scratchpad {
    /// Persist to disk via atomic_write_json. Sprint 3 — agent runtime
    /// calls this after every successful update so a crash never loses
    /// more than the in-flight LLM call.
    pub fn save(&self, path: &Path) -> Result<(), crate::agent::persistence::PersistError> {
        atomic_write_json(path, self)
    }

    /// Load from disk. Returns `Default` on missing/corrupt — Sprint 3
    /// deliberately never refuses to start an agent because of a broken
    /// scratchpad file; the agent gets a fresh notepad and we log loudly.
    pub fn load(path: &Path) -> Self {
        match read_json::<Scratchpad>(path) {
            Ok(Some(pad)) => pad,
            Ok(None) => Scratchpad::default(),
            Err(e) => {
                tracing::warn!(
                    target: "aidock::scratchpad",
                    path = %path.display(),
                    error = %e,
                    "could not load scratchpad — starting fresh"
                );
                Scratchpad::default()
            }
        }
    }
}

pub fn is_scratchpad_tool(name: &str) -> bool {
    name == TOOL_UPDATE_SCRATCHPAD
}

/// Tool definitions to advertise to the model alongside action + query
/// tools. Currently just one — `update_scratchpad`.
pub fn scratchpad_tools() -> Vec<Tool> {
    vec![Tool::function(
        TOOL_UPDATE_SCRATCHPAD,
        "Record progress in your private scratchpad. The scratchpad is yours alone — invisible to teammates, pinned to your prompt every turn so you don't lose track between thinks. Set current_focus, append tasks/files/decisions. At least one field is required. Always pair with an action tool on the NEXT turn — this call alone is not a response.",
        json!({
            "type": "object",
            "properties": {
                "current_focus": {
                    "type": "string",
                    "description": "What you're working on right now. Replaces any prior focus."
                },
                "add_tasks": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "New sub-tasks to append to your task breakdown."
                },
                "add_files": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "note": { "type": "string" }
                        },
                        "required": ["path", "note"]
                    },
                    "description": "Files you read or wrote this turn, each with a short note describing what."
                },
                "add_decisions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Concrete decisions you've committed to. Phrase as a single declarative sentence each."
                }
            }
        }),
    )]
}

#[derive(Debug, thiserror::Error)]
pub enum ScratchpadError {
    #[error("invalid scratchpad args: {0}")]
    InvalidArgs(#[source] serde_json::Error),
    #[error("empty update — set at least one of current_focus / add_tasks / add_files / add_decisions")]
    EmptyUpdate,
}

#[derive(Debug, Deserialize)]
struct UpdateArgs {
    #[serde(default)]
    current_focus: Option<String>,
    #[serde(default)]
    add_tasks: Vec<String>,
    #[serde(default)]
    add_files: Vec<FileChange>,
    #[serde(default)]
    add_decisions: Vec<String>,
}

/// Apply one `update_scratchpad` call to `pad`. Returns a one-line
/// confirmation suitable for echoing back to the model as the tool result.
pub fn apply_update(pad: &mut Scratchpad, raw_args: &str) -> Result<String, ScratchpadError> {
    let args: UpdateArgs = serde_json::from_str(raw_args).map_err(ScratchpadError::InvalidArgs)?;
    let mut changes: Vec<String> = Vec::new();

    if let Some(focus) = args.current_focus.filter(|s| !s.is_empty()) {
        pad.current_focus = focus.clone();
        changes.push(format!("focus set: {focus}"));
    }
    if !args.add_tasks.is_empty() {
        let n = args.add_tasks.len();
        pad.task_breakdown.extend(args.add_tasks);
        changes.push(format!("{n} task(s) appended"));
    }
    if !args.add_files.is_empty() {
        let n = args.add_files.len();
        pad.files_modified.extend(args.add_files);
        changes.push(format!("{n} file(s) logged"));
    }
    if !args.add_decisions.is_empty() {
        let n = args.add_decisions.len();
        pad.decisions.extend(args.add_decisions);
        changes.push(format!("{n} decision(s) recorded"));
    }

    if changes.is_empty() {
        return Err(ScratchpadError::EmptyUpdate);
    }

    Ok(format!("scratchpad updated — {}", changes.join("; ")))
}

/// Render the scratchpad into a section suitable for the system prompt.
/// Empty string when the scratchpad has nothing in it — the context builder
/// then skips the section entirely so prompts stay tight.
pub fn render_for_prompt(pad: &Scratchpad) -> String {
    if pad.current_focus.is_empty()
        && pad.task_breakdown.is_empty()
        && pad.files_modified.is_empty()
        && pad.decisions.is_empty()
    {
        return String::new();
    }

    let mut s = String::from("## Your scratchpad\n");
    s.push_str("(Private notes — only you see these. Maintained across your turns this session.)\n\n");

    if !pad.current_focus.is_empty() {
        s.push_str(&format!("Current focus: {}\n", pad.current_focus));
    }
    if !pad.task_breakdown.is_empty() {
        s.push_str("\nTasks:\n");
        for t in &pad.task_breakdown {
            s.push_str(&format!("- {t}\n"));
        }
    }
    if !pad.files_modified.is_empty() {
        s.push_str("\nFiles touched:\n");
        for f in &pad.files_modified {
            s.push_str(&format!("- {}: {}\n", f.path, f.note));
        }
    }
    if !pad.decisions.is_empty() {
        s.push_str("\nDecisions:\n");
        for d in &pad.decisions {
            s.push_str(&format!("- {d}\n"));
        }
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scratchpad_renders_nothing() {
        let pad = Scratchpad::default();
        assert!(render_for_prompt(&pad).is_empty());
    }

    #[test]
    fn focus_only_renders_focus_section() {
        let mut pad = Scratchpad::default();
        pad.current_focus = "building login page".into();
        let s = render_for_prompt(&pad);
        assert!(s.contains("Current focus: building login page"));
        // No empty section headers when other fields are empty.
        assert!(!s.contains("Tasks:"));
        assert!(!s.contains("Files touched"));
        assert!(!s.contains("Decisions:"));
    }

    #[test]
    fn apply_update_sets_focus_and_appends_tasks() {
        let mut pad = Scratchpad::default();
        let confirmation = apply_update(
            &mut pad,
            r#"{"current_focus":"login page","add_tasks":["form","submit handler"]}"#,
        )
        .unwrap();
        assert_eq!(pad.current_focus, "login page");
        assert_eq!(pad.task_breakdown, vec!["form", "submit handler"]);
        assert!(confirmation.contains("focus set"));
        assert!(confirmation.contains("2 task"));
    }

    #[test]
    fn add_files_round_trips() {
        let mut pad = Scratchpad::default();
        apply_update(
            &mut pad,
            r#"{"add_files":[{"path":"src/login.html","note":"created form skeleton"}]}"#,
        )
        .unwrap();
        assert_eq!(pad.files_modified.len(), 1);
        assert_eq!(pad.files_modified[0].path, "src/login.html");
        assert!(render_for_prompt(&pad).contains("src/login.html"));
    }

    #[test]
    fn decisions_append_and_render() {
        let mut pad = Scratchpad::default();
        apply_update(&mut pad, r#"{"add_decisions":["use localStorage for token"]}"#).unwrap();
        apply_update(&mut pad, r#"{"add_decisions":["redirect to /home after login"]}"#).unwrap();
        assert_eq!(pad.decisions.len(), 2);
        let s = render_for_prompt(&pad);
        assert!(s.contains("Decisions:"));
        assert!(s.contains("localStorage"));
        assert!(s.contains("redirect"));
    }

    #[test]
    fn empty_payload_errors() {
        let mut pad = Scratchpad::default();
        let err = apply_update(&mut pad, "{}").unwrap_err();
        match err {
            ScratchpadError::EmptyUpdate => {}
            other => panic!("expected EmptyUpdate, got {other:?}"),
        }
    }

    #[test]
    fn empty_focus_string_does_not_overwrite() {
        // An empty current_focus shouldn't *unset* an existing focus. Treat
        // it like "not provided" — the agent has to send a real value to
        // change focus.
        let mut pad = Scratchpad {
            current_focus: "previous focus".into(),
            ..Default::default()
        };
        let err = apply_update(&mut pad, r#"{"current_focus":""}"#).unwrap_err();
        assert!(matches!(err, ScratchpadError::EmptyUpdate));
        assert_eq!(pad.current_focus, "previous focus");
    }

    #[test]
    fn current_focus_replaces_prior() {
        let mut pad = Scratchpad::default();
        apply_update(&mut pad, r#"{"current_focus":"first"}"#).unwrap();
        apply_update(&mut pad, r#"{"current_focus":"second"}"#).unwrap();
        assert_eq!(pad.current_focus, "second");
    }

    #[test]
    fn scratchpad_tool_is_singleton_set() {
        let tools = scratchpad_tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, TOOL_UPDATE_SCRATCHPAD);
        assert!(is_scratchpad_tool(TOOL_UPDATE_SCRATCHPAD));
        assert!(!is_scratchpad_tool("BROADCAST"));
    }
}
