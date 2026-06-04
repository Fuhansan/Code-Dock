//! ④ 原生文件工具（CC 对齐）：Read / Edit / Write / Glob / Grep —— step 2。
//!
//! `Backend::Native`，取代旧的动态 fs__ MCP 文件工具。
//!   - 只读（Read/Glob/Grep）可越界（L0）；改/删（Edit/Write）经路径 confinement
//!     锁死在工作区间内（L3 红线，CLAUDE.md ④.d）。
//!   - **read-before-edit**：Read 登记文件内容指纹，Edit / 覆盖式 Write 据此校验
//!     "读过且没变"，否则拒。
//!
//! 范围说明：这里只落"必带的 L3 硬底线 + 读状态"。完整危险分级 + 可配安全级别策略
//! （放行/问人）是 step 4 的安全检查管线。Grep 暂用子串匹配（正则/ripgrep 后补）。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

use crate::agent::react::ExecResult;
use crate::llm::Tool;

pub const TOOL_READ: &str = "Read";
pub const TOOL_EDIT: &str = "Edit";
pub const TOOL_WRITE: &str = "Write";
pub const TOOL_GLOB: &str = "Glob";
pub const TOOL_GREP: &str = "Grep";

/// True iff `name` is one of the native file tools (executor routing).
pub fn is_file_tool(name: &str) -> bool {
    matches!(name, TOOL_READ | TOOL_EDIT | TOOL_WRITE | TOOL_GLOB | TOOL_GREP)
}

/// Default line cap for Read (CC parity).
const READ_DEFAULT_LIMIT: usize = 2000;
/// Per-line char cap; longer lines are truncated with `…`.
const READ_MAX_LINE: usize = 2000;
/// Result caps for Glob / Grep so a huge tree can't flood the LLM context.
const GLOB_MAX: usize = 200;
const GREP_MAX: usize = 100;

// ---------------------------------------------------------------------------
// Tool schemas (the LLM-facing front; registered in `tools::static_registry`).
// ---------------------------------------------------------------------------

pub fn read_tool() -> Tool {
    Tool::function(
        TOOL_READ,
        "Read a text file from the workspace (or an absolute path). Returns the content with \
         line numbers. Use `offset`/`limit` to page through large files. You MUST Read a file \
         before you Edit it or overwrite it with Write.",
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "Path, relative to the workspace or absolute." },
                "offset": { "type": "integer", "description": "1-based line to start at (default 1)." },
                "limit": { "type": "integer", "description": "Max lines to return (default 2000)." }
            },
            "required": ["file_path"]
        }),
    )
}

pub fn edit_tool() -> Tool {
    Tool::function(
        TOOL_EDIT,
        "Replace an exact string in a file. `old_string` must match EXACTLY and be unique \
         (otherwise pass replace_all=true to change every occurrence). You must Read the file \
         first. Confined to the workspace — edits outside are refused.",
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string" },
                "old_string": { "type": "string", "description": "Exact text to replace." },
                "new_string": { "type": "string", "description": "Replacement text." },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)." }
            },
            "required": ["file_path", "old_string", "new_string"]
        }),
    )
}

pub fn write_tool() -> Tool {
    Tool::function(
        TOOL_WRITE,
        "Create a file or overwrite an existing one. Parent directories are created \
         automatically. Overwriting an existing file requires having Read it first. Confined \
         to the workspace — writes outside are refused.",
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["file_path", "content"]
        }),
    )
}

pub fn glob_tool() -> Tool {
    Tool::function(
        TOOL_GLOB,
        "Find files by name pattern (e.g. \"**/*.rs\", \"src/*.ts\"). Supports * (any chars in \
         a segment), ** (any directories), ? (one char). Read-only.",
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob, e.g. \"**/*.rs\"." },
                "path": { "type": "string", "description": "Base dir to search (default workspace)." }
            },
            "required": ["pattern"]
        }),
    )
}

pub fn grep_tool() -> Tool {
    Tool::function(
        TOOL_GREP,
        "Search file contents for a substring (case-insensitive). Returns matching \
         path:line:text. Read-only.",
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Substring to find." },
                "path": { "type": "string", "description": "Base dir to search (default workspace)." }
            },
            "required": ["pattern"]
        }),
    )
}

// ---------------------------------------------------------------------------
// The executor backend: holds the workspace root + per-agent read state.
// ---------------------------------------------------------------------------

/// Native file-tool backend. One per agent (lives on the `CompositeExecutor`).
/// `read` is the read-before-edit ledger: canonical path → content fingerprint.
pub struct FileTools {
    workspace: PathBuf,
    read: HashMap<PathBuf, u64>,
}

impl FileTools {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            read: HashMap::new(),
        }
    }

    /// Route one native file tool call.
    pub fn dispatch(&mut self, tool: &str, args: &str) -> ExecResult {
        match tool {
            TOOL_READ => self.read_file(args),
            TOOL_EDIT => self.edit_file(args),
            TOOL_WRITE => self.write_file(args),
            TOOL_GLOB => self.glob(args),
            TOOL_GREP => self.grep(args),
            other => err(format!("'{other}' is not a native file tool")),
        }
    }

    // --- path helpers ------------------------------------------------------

    /// Resolve a tool path: relative → joined onto the workspace, absolute kept.
    fn resolve(&self, p: &str) -> PathBuf {
        let p = Path::new(p);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.workspace.join(p)
        }
    }

    /// Stable key for the read ledger: canonical path if it exists, else the
    /// resolved path verbatim.
    fn key(&self, p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    /// L3 red line: a mutation's target must resolve inside the workspace.
    /// Read/Glob/Grep skip this. Delegates to the shared [`within_workspace`].
    fn mutation_in_workspace(&self, target: &Path) -> bool {
        within_workspace(target, &self.workspace)
    }

    // --- Read --------------------------------------------------------------

    fn read_file(&mut self, args: &str) -> ExecResult {
        #[derive(Deserialize)]
        struct A {
            file_path: String,
            offset: Option<usize>,
            limit: Option<usize>,
        }
        let a: A = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => return err(format!("Read bad args: {e}")),
        };
        let path = self.resolve(&a.file_path);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return err(format!("Read '{}' failed: {e}", a.file_path)),
        };
        // Register for read-before-edit (fingerprint of full content).
        self.read.insert(self.key(&path), fingerprint(&content));

        let start = a.offset.unwrap_or(1).max(1); // 1-based
        let limit = a.limit.unwrap_or(READ_DEFAULT_LIMIT);
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let mut out = String::new();
        for (i, line) in lines.iter().enumerate().skip(start - 1).take(limit) {
            let n = i + 1;
            let shown = if line.chars().count() > READ_MAX_LINE {
                let t: String = line.chars().take(READ_MAX_LINE).collect();
                format!("{t}…")
            } else {
                (*line).to_string()
            };
            out.push_str(&format!("{n:>6}\t{shown}\n"));
        }
        if out.is_empty() {
            out = "(empty file or offset past end)".to_string();
        }
        let shown_end = (start - 1 + limit).min(total);
        if shown_end < total {
            out.push_str(&format!(
                "… [{} more lines; use offset={} to continue]",
                total - shown_end,
                shown_end + 1
            ));
        }
        ok(out)
    }

    // --- Edit --------------------------------------------------------------

    fn edit_file(&mut self, args: &str) -> ExecResult {
        #[derive(Deserialize)]
        struct A {
            file_path: String,
            old_string: String,
            new_string: String,
            #[serde(default)]
            replace_all: bool,
        }
        let a: A = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => return err(format!("Edit bad args: {e}")),
        };
        let path = self.resolve(&a.file_path);
        if !self.mutation_in_workspace(&path) {
            return denied(format!(
                "Edit '{}' escapes the workspace (L3 red line)",
                a.file_path
            ));
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return err(format!("Edit '{}' failed to read: {e}", a.file_path)),
        };
        // read-before-edit
        match self.read.get(&self.key(&path)) {
            None => {
                return err(format!(
                    "must Read '{}' before editing it",
                    a.file_path
                ))
            }
            Some(&h) if h != fingerprint(&content) => {
                return err(format!(
                    "'{}' changed since you Read it — Read it again before editing",
                    a.file_path
                ))
            }
            _ => {}
        }
        let count = content.matches(&a.old_string).count();
        if count == 0 {
            return err(format!("old_string not found in '{}'", a.file_path));
        }
        if count > 1 && !a.replace_all {
            return err(format!(
                "old_string is not unique in '{}' ({count} matches) — add surrounding context or pass replace_all=true",
                a.file_path
            ));
        }
        let new_content = if a.replace_all {
            content.replace(&a.old_string, &a.new_string)
        } else {
            content.replacen(&a.old_string, &a.new_string, 1)
        };
        if let Err(e) = std::fs::write(&path, &new_content) {
            return err(format!("Edit '{}' failed to write: {e}", a.file_path));
        }
        self.read.insert(self.key(&path), fingerprint(&new_content));
        ok(format!(
            "Edited {} ({} replacement{})",
            a.file_path,
            count.min(if a.replace_all { count } else { 1 }),
            if a.replace_all && count != 1 { "s" } else { "" }
        ))
    }

    // --- Write -------------------------------------------------------------

    fn write_file(&mut self, args: &str) -> ExecResult {
        #[derive(Deserialize)]
        struct A {
            file_path: String,
            content: String,
        }
        let a: A = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => return err(format!("Write bad args: {e}")),
        };
        let path = self.resolve(&a.file_path);
        if !self.mutation_in_workspace(&path) {
            return denied(format!(
                "Write '{}' escapes the workspace (L3 red line)",
                a.file_path
            ));
        }
        let existed = path.exists();
        if existed {
            // Overwrite protection: must have Read the current content.
            let cur = std::fs::read_to_string(&path).unwrap_or_default();
            match self.read.get(&self.key(&path)) {
                None => {
                    return err(format!(
                        "'{}' exists — Read it before overwriting with Write",
                        a.file_path
                    ))
                }
                Some(&h) if h != fingerprint(&cur) => {
                    return err(format!(
                        "'{}' changed since you Read it — Read it again before overwriting",
                        a.file_path
                    ))
                }
                _ => {}
            }
        }
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return err(format!("Write '{}' failed to create dirs: {e}", a.file_path));
            }
        }
        if let Err(e) = std::fs::write(&path, &a.content) {
            return err(format!("Write '{}' failed: {e}", a.file_path));
        }
        self.read.insert(self.key(&path), fingerprint(&a.content));
        ok(format!(
            "{} {} ({} bytes)",
            if existed { "Overwrote" } else { "Created" },
            a.file_path,
            a.content.len()
        ))
    }

    // --- Glob --------------------------------------------------------------

    fn glob(&self, args: &str) -> ExecResult {
        #[derive(Deserialize)]
        struct A {
            pattern: String,
            path: Option<String>,
        }
        let a: A = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => return err(format!("Glob bad args: {e}")),
        };
        let base = a
            .path
            .as_deref()
            .map(|p| self.resolve(p))
            .unwrap_or_else(|| self.workspace.clone());
        let pat_segs: Vec<&str> = a.pattern.split('/').filter(|s| !s.is_empty()).collect();

        let mut files = Vec::new();
        walk(&base, &mut files);
        let mut hits: Vec<String> = files
            .iter()
            .filter_map(|f| {
                let rel = f.strip_prefix(&base).unwrap_or(f);
                let segs: Vec<&str> = rel
                    .to_str()?
                    .split(std::path::MAIN_SEPARATOR)
                    .filter(|s| !s.is_empty())
                    .collect();
                if glob_match(&pat_segs, &segs) {
                    Some(rel.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .collect();
        hits.sort();
        let total = hits.len();
        hits.truncate(GLOB_MAX);
        if hits.is_empty() {
            return ok(format!("(no files match {})", a.pattern));
        }
        let mut out = hits.join("\n");
        if total > GLOB_MAX {
            out.push_str(&format!("\n… [{} more; narrow the pattern]", total - GLOB_MAX));
        }
        ok(out)
    }

    // --- Grep --------------------------------------------------------------

    fn grep(&self, args: &str) -> ExecResult {
        #[derive(Deserialize)]
        struct A {
            pattern: String,
            path: Option<String>,
        }
        let a: A = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => return err(format!("Grep bad args: {e}")),
        };
        let base = a
            .path
            .as_deref()
            .map(|p| self.resolve(p))
            .unwrap_or_else(|| self.workspace.clone());
        let needle = a.pattern.to_lowercase();

        let mut files = Vec::new();
        walk(&base, &mut files);
        files.sort();

        let mut out = String::new();
        let mut count = 0usize;
        let mut truncated = false;
        'outer: for f in &files {
            let Ok(content) = std::fs::read_to_string(f) else {
                continue; // skip binary / unreadable
            };
            let rel = f.strip_prefix(&base).unwrap_or(f);
            for (i, line) in content.lines().enumerate() {
                if line.to_lowercase().contains(&needle) {
                    if count >= GREP_MAX {
                        truncated = true;
                        break 'outer;
                    }
                    let shown = if line.chars().count() > 200 {
                        let t: String = line.chars().take(200).collect();
                        format!("{t}…")
                    } else {
                        line.to_string()
                    };
                    out.push_str(&format!("{}:{}:{}\n", rel.to_string_lossy(), i + 1, shown));
                    count += 1;
                }
            }
        }
        if count == 0 {
            return ok(format!("(no matches for {:?})", a.pattern));
        }
        if truncated {
            out.push_str(&format!("… [stopped at {GREP_MAX} matches; narrow the search]"));
        }
        ok(out)
    }
}

// ---------------------------------------------------------------------------
// Free helpers.
// ---------------------------------------------------------------------------

fn ok(s: impl Into<String>) -> ExecResult {
    ExecResult {
        raw: s.into(),
        success: true,
    }
}
fn err(s: impl Into<String>) -> ExecResult {
    ExecResult {
        raw: format!("ERROR: {}", s.into()),
        success: false,
    }
}
fn denied(s: impl Into<String>) -> ExecResult {
    ExecResult {
        raw: format!("DENIED: {}", s.into()),
        success: false,
    }
}

/// Shared L3 confinement check (used by [`FileTools`] and the ④.d security
/// classifier). Canonicalizes the nearest existing ancestor of `target` — this
/// defeats `..` and symlink escapes and tolerates not-yet-created nested dirs —
/// and tests that it sits inside `workspace`.
pub fn within_workspace(target: &Path, workspace: &Path) -> bool {
    let ws = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut probe: Option<&Path> = Some(target);
    while let Some(p) = probe {
        if p.exists() {
            return std::fs::canonicalize(p)
                .map(|c| c.starts_with(&ws))
                .unwrap_or(false);
        }
        probe = p.parent();
    }
    false
}

fn fingerprint(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Recursively collect files under `root`, skipping noisy/irrelevant dirs.
fn walk(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if matches!(name, ".git" | ".aidock_git" | "node_modules" | "target" | "dist") {
                continue;
            }
            walk(&p, out);
        } else if p.is_file() {
            out.push(p);
        }
    }
}

/// Segment-based glob: `**` matches zero+ path segments; within a segment `*`
/// matches any run of non-`/` chars and `?` matches one char.
fn glob_match(pat: &[&str], path: &[&str]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|i| glob_match(rest, &path[i..])),
        Some((seg, rest)) => {
            !path.is_empty() && seg_match(seg, path[0]) && glob_match(rest, &path[1..])
        }
    }
}

/// Wildcard match for ONE segment (`*` = any non-slash run, `?` = one char).
fn seg_match(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    // Classic backtracking wildcard matcher.
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn tmp_ws() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("aidock-fs-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn seg_and_glob_match() {
        assert!(seg_match("*.rs", "main.rs"));
        assert!(!seg_match("*.rs", "main.ts"));
        assert!(seg_match("a?c", "abc"));
        assert!(glob_match(&["**", "*.rs"], &["src", "a", "b.rs"]));
        assert!(glob_match(&["src", "*.ts"], &["src", "x.ts"]));
        assert!(!glob_match(&["src", "*.ts"], &["lib", "x.ts"]));
    }

    #[test]
    fn write_then_read_roundtrip() {
        let ws = tmp_ws();
        let mut ft = FileTools::new(&ws);
        let r = ft.write_file(r#"{"file_path":"sub/a.txt","content":"hello\nworld"}"#);
        assert!(r.success, "{}", r.raw);
        let r = ft.read_file(r#"{"file_path":"sub/a.txt"}"#);
        assert!(r.success);
        assert!(r.raw.contains("1\thello"));
        assert!(r.raw.contains("2\tworld"));
    }

    #[test]
    fn edit_requires_prior_read() {
        let ws = tmp_ws();
        std::fs::write(ws.join("f.txt"), "alpha beta").unwrap();
        let mut ft = FileTools::new(&ws);
        // Edit without Read → refused.
        let r = ft.edit_file(r#"{"file_path":"f.txt","old_string":"alpha","new_string":"ALPHA"}"#);
        assert!(!r.success);
        assert!(r.raw.contains("must Read"));
        // Read, then Edit works.
        assert!(ft.read_file(r#"{"file_path":"f.txt"}"#).success);
        let r = ft.edit_file(r#"{"file_path":"f.txt","old_string":"alpha","new_string":"ALPHA"}"#);
        assert!(r.success, "{}", r.raw);
        assert_eq!(std::fs::read_to_string(ws.join("f.txt")).unwrap(), "ALPHA beta");
    }

    #[test]
    fn edit_rejects_non_unique_without_replace_all() {
        let ws = tmp_ws();
        std::fs::write(ws.join("d.txt"), "x x x").unwrap();
        let mut ft = FileTools::new(&ws);
        ft.read_file(r#"{"file_path":"d.txt"}"#);
        let r = ft.edit_file(r#"{"file_path":"d.txt","old_string":"x","new_string":"y"}"#);
        assert!(!r.success);
        assert!(r.raw.contains("not unique"));
        let r = ft.edit_file(r#"{"file_path":"d.txt","old_string":"x","new_string":"y","replace_all":true}"#);
        assert!(r.success, "{}", r.raw);
        assert_eq!(std::fs::read_to_string(ws.join("d.txt")).unwrap(), "y y y");
    }

    #[test]
    fn write_outside_workspace_is_denied() {
        let ws = tmp_ws();
        let mut ft = FileTools::new(&ws);
        // Absolute path outside the workspace.
        let outside = std::env::temp_dir().join("aidock-escape-should-not-exist.txt");
        let args = format!(r#"{{"file_path":{:?},"content":"x"}}"#, outside.to_string_lossy());
        let r = ft.write_file(&args);
        assert!(!r.success);
        assert!(r.raw.starts_with("DENIED:"), "{}", r.raw);
        assert!(!outside.exists(), "must NOT have written outside workspace");
    }

    #[test]
    fn write_traversal_escape_is_denied() {
        let ws = tmp_ws();
        let mut ft = FileTools::new(&ws);
        let r = ft.write_file(r#"{"file_path":"../escape.txt","content":"x"}"#);
        assert!(!r.success);
        assert!(r.raw.starts_with("DENIED:"), "{}", r.raw);
    }

    #[test]
    fn glob_and_grep() {
        let ws = tmp_ws();
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/main.rs"), "fn main() { todo!() }").unwrap();
        std::fs::write(ws.join("src/lib.rs"), "pub fn lib() {}").unwrap();
        std::fs::write(ws.join("README.md"), "docs").unwrap();
        let ft = FileTools::new(&ws);

        let r = ft.glob(r#"{"pattern":"**/*.rs"}"#);
        assert!(r.success);
        assert!(r.raw.contains("main.rs") && r.raw.contains("lib.rs"));
        assert!(!r.raw.contains("README.md"));

        let r = ft.grep(r#"{"pattern":"fn"}"#);
        assert!(r.success);
        assert!(r.raw.contains("main.rs") && r.raw.contains("lib.rs"));
    }
}
