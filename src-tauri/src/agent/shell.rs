//! ④ Bash 工具 + 命令执行（CC 对齐的 Bash）—— step 3。
//!
//! **去沙箱**（CLAUDE.md ④.d 2026-06-03 转向）：命令直接用 `tokio::process` 跑、
//! cwd = 工作区间，**不再裹 OS 沙箱**（`SeatbeltSandbox`/`AsrtSandbox` 已删）。底线
//! 靠 step 4 安全检查（Bash 默认 L2 问人）+ 文件工具的工作区间 confinement。
//!
//!   - **前台**：等结果、超时杀；合并 stdout/stderr；输出超量**落盘**、只回预览+路径
//!     （双通道的 ⑥ 侧；UI 侧 `TOOL_CALL_EVENT` 是 step 5）。
//!   - **后台**（dev server 等）：`run_in_background` spawn detached + **进程登记表** +
//!     `BashOutput`（拉增量输出）/ `KillShell`（按 id 停）。登记表 per-agent、跨回合存活。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;

use crate::agent::react::ExecResult;
use crate::llm::Tool;

pub const TOOL_BASH: &str = "Bash";
pub const TOOL_BASH_OUTPUT: &str = "BashOutput";
pub const TOOL_KILL_SHELL: &str = "KillShell";

/// True iff `name` is one of the Bash-family tools (executor routing).
pub fn is_bash_tool(name: &str) -> bool {
    matches!(name, TOOL_BASH | TOOL_BASH_OUTPUT | TOOL_KILL_SHELL)
}

const TIMEOUT_DEFAULT_MS: u64 = 120_000;
const TIMEOUT_MAX_MS: u64 = 600_000;
/// Chars of combined output returned inline before we spill the full body to a
/// file and return only a head+tail preview (two-pipe ⑥ side).
const OUTPUT_PREVIEW_MAX: usize = 16_000;
const PREVIEW_HEAD: usize = 10_000;
const PREVIEW_TAIL: usize = 4_000;

// ---------------------------------------------------------------------------
// Tool schemas.
// ---------------------------------------------------------------------------

pub fn bash_tool() -> Tool {
    Tool::function(
        TOOL_BASH,
        "Run a shell command in the workspace (cwd = workspace). Use it to VERIFY your work: \
         run scripts (e.g. \"python3 fib.py\"), tests, builds, installs. Foreground waits for \
         completion and returns combined stdout/stderr (very large output is written to a file \
         and only previewed here). Set run_in_background=true for long-running processes (e.g. \
         a dev server); it returns immediately with a shell_id you poll with BashOutput and \
         stop with KillShell.",
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command." },
                "run_in_background": { "type": "boolean", "description": "Run detached; returns a shell_id. Default false." },
                "timeout_ms": { "type": "integer", "description": "Foreground timeout (default 120000, max 600000)." }
            },
            "required": ["command"]
        }),
    )
}

pub fn bash_output_tool() -> Tool {
    Tool::function(
        TOOL_BASH_OUTPUT,
        "Fetch new output from a background command started with Bash(run_in_background=true). \
         Returns output produced since your last BashOutput call for that shell_id.",
        json!({
            "type": "object",
            "properties": { "shell_id": { "type": "string" } },
            "required": ["shell_id"]
        }),
    )
}

pub fn kill_shell_tool() -> Tool {
    Tool::function(
        TOOL_KILL_SHELL,
        "Stop a background command by its shell_id.",
        json!({
            "type": "object",
            "properties": { "shell_id": { "type": "string" } },
            "required": ["shell_id"]
        }),
    )
}

// ---------------------------------------------------------------------------
// Background process registry (per-agent, survives across turns).
// ---------------------------------------------------------------------------

struct BgHandle {
    child: tokio::process::Child,
    /// Combined stdout/stderr, appended to by the reader tasks.
    output: Arc<Mutex<String>>,
    /// Byte offset already returned via BashOutput (incremental reads).
    cursor: usize,
}

/// Registry of background commands. `Clone` is cheap (Arc) so each turn's
/// [`BashTool`] shares the same table.
#[derive(Clone, Default)]
pub struct BashRegistry {
    inner: Arc<Mutex<HashMap<String, BgHandle>>>,
    seq: Arc<AtomicU32>,
}

impl BashRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    async fn spawn(&self, command: &str, workspace: &PathBuf) -> Result<String, String> {
        let mut child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to launch background command: {e}"))?;

        let buf = Arc::new(Mutex::new(String::new()));
        if let Some(out) = child.stdout.take() {
            tokio::spawn(drain(out, buf.clone()));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(drain(err, buf.clone()));
        }

        let id = format!("bash_{}", self.seq.fetch_add(1, Ordering::SeqCst) + 1);
        self.inner.lock().await.insert(
            id.clone(),
            BgHandle {
                child,
                output: buf,
                cursor: 0,
            },
        );
        Ok(id)
    }

    async fn read_new(&self, id: &str) -> Option<String> {
        let mut g = self.inner.lock().await;
        let h = g.get_mut(id)?;
        let s = h.output.lock().await;
        // `cursor` is always a previous full length → a valid UTF-8 boundary.
        let new = s.get(h.cursor..).unwrap_or("").to_string();
        h.cursor = s.len();
        Some(new)
    }

    async fn kill(&self, id: &str) -> bool {
        let mut g = self.inner.lock().await;
        if let Some(mut h) = g.remove(id) {
            let _ = h.child.start_kill();
            true
        } else {
            false
        }
    }
}

/// Pump a child's stdout/stderr into the shared buffer until EOF.
async fn drain<R: AsyncReadExt + Unpin>(mut r: R, buf: Arc<Mutex<String>>) {
    let mut tmp = [0u8; 4096];
    loop {
        match r.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.lock().await.push_str(&String::from_utf8_lossy(&tmp[..n])),
        }
    }
}

// ---------------------------------------------------------------------------
// The Bash backend (held by the CompositeExecutor, one per turn).
// ---------------------------------------------------------------------------

pub struct BashTool {
    workspace: PathBuf,
    /// Whether this role may run commands (engineers yes, PM no). Belt-and-
    /// braces: the catalog already withholds the tool from PM, but if a call
    /// somehow arrives we refuse.
    enabled: bool,
    registry: BashRegistry,
}

impl BashTool {
    pub fn new(workspace: impl Into<PathBuf>, enabled: bool, registry: BashRegistry) -> Self {
        Self {
            workspace: workspace.into(),
            enabled,
            registry,
        }
    }

    pub async fn dispatch(&self, tool: &str, args: &str) -> ExecResult {
        if !self.enabled {
            return err("shell/Bash is not available for this role");
        }
        match tool {
            TOOL_BASH => self.run(args).await,
            TOOL_BASH_OUTPUT => self.bash_output(args).await,
            TOOL_KILL_SHELL => self.kill_shell(args).await,
            other => err(format!("'{other}' is not a Bash-family tool")),
        }
    }

    async fn run(&self, args: &str) -> ExecResult {
        #[derive(Deserialize)]
        struct A {
            command: String,
            #[serde(default)]
            run_in_background: bool,
            timeout_ms: Option<u64>,
        }
        let a: A = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => return err(format!("Bash bad args: {e}")),
        };

        if a.run_in_background {
            return match self.registry.spawn(&a.command, &self.workspace).await {
                Ok(id) => ok(format!(
                    "started in background as {id} — poll with BashOutput(shell_id=\"{id}\"), stop with KillShell."
                )),
                Err(e) => err(e),
            };
        }

        let dur = std::time::Duration::from_millis(
            a.timeout_ms.unwrap_or(TIMEOUT_DEFAULT_MS).min(TIMEOUT_MAX_MS),
        );
        let fut = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(&a.command)
            .current_dir(&self.workspace)
            .output();

        match tokio::time::timeout(dur, fut).await {
            Ok(Ok(o)) => {
                let mut combined = String::from_utf8_lossy(&o.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&o.stderr);
                if !stderr.trim().is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str("[stderr]\n");
                    combined.push_str(&stderr);
                }
                ExecResult {
                    raw: finalize_output(combined),
                    success: o.status.success(),
                }
            }
            Ok(Err(e)) => err(format!("failed to launch command: {e}")),
            Err(_) => err(format!(
                "command timed out after {}ms",
                dur.as_millis()
            )),
        }
    }

    async fn bash_output(&self, args: &str) -> ExecResult {
        #[derive(Deserialize)]
        struct A {
            shell_id: String,
        }
        let a: A = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => return err(format!("BashOutput bad args: {e}")),
        };
        match self.registry.read_new(&a.shell_id).await {
            Some(s) if s.trim().is_empty() => ok("(no new output)".to_string()),
            Some(s) => ok(finalize_output(s)),
            None => err(format!("no background shell {:?}", a.shell_id)),
        }
    }

    async fn kill_shell(&self, args: &str) -> ExecResult {
        #[derive(Deserialize)]
        struct A {
            shell_id: String,
        }
        let a: A = match serde_json::from_str(args) {
            Ok(a) => a,
            Err(e) => return err(format!("KillShell bad args: {e}")),
        };
        if self.registry.kill(&a.shell_id).await {
            ok(format!("killed {}", a.shell_id))
        } else {
            err(format!("no background shell {:?}", a.shell_id))
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers.
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

/// Two-pipe ⑥ side: oversized output goes to a temp file; only a head+tail
/// preview + the path comes back inline. (The UI gets the full body via the
/// future `TOOL_CALL_EVENT` — step 5.)
fn finalize_output(raw: String) -> String {
    if raw.trim().is_empty() {
        return "(no output)".to_string();
    }
    if raw.chars().count() <= OUTPUT_PREVIEW_MAX {
        return raw;
    }
    let chars: Vec<char> = raw.chars().collect();
    let total = chars.len();
    let head: String = chars.iter().take(PREVIEW_HEAD).collect();
    let tail: String = chars.iter().skip(total - PREVIEW_TAIL).collect();

    let path = std::env::temp_dir()
        .join("aidock-bash")
        .join(format!("out-{}.log", uuid::Uuid::new_v4()));
    let spilled = path
        .parent()
        .map(|d| std::fs::create_dir_all(d))
        .transpose()
        .and_then(|_| std::fs::write(&path, &raw).map(|_| ()))
        .is_ok();

    let pointer = if spilled {
        format!("full {total} chars at {}", path.display())
    } else {
        format!("{total} chars total (spill failed)")
    };
    format!("{head}\n\n… [output truncated; {pointer}] …\n\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(enabled: bool) -> BashTool {
        BashTool::new(std::env::temp_dir(), enabled, BashRegistry::new())
    }

    #[test]
    fn tool_predicates() {
        assert!(is_bash_tool("Bash"));
        assert!(is_bash_tool("BashOutput"));
        assert!(is_bash_tool("KillShell"));
        assert!(!is_bash_tool("Read"));
        assert_eq!(bash_tool().function.name, TOOL_BASH);
    }

    #[tokio::test]
    async fn disabled_role_is_refused() {
        let res = bash(false).dispatch("Bash", r#"{"command":"echo hi"}"#).await;
        assert!(!res.success);
        assert!(res.raw.contains("not available"));
    }

    #[tokio::test]
    async fn foreground_runs_and_returns_output() {
        let res = bash(true).dispatch("Bash", r#"{"command":"echo hello-aidock"}"#).await;
        assert!(res.success, "{}", res.raw);
        assert!(res.raw.contains("hello-aidock"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_failure() {
        let res = bash(true).dispatch("Bash", r#"{"command":"exit 3"}"#).await;
        assert!(!res.success);
    }

    #[tokio::test]
    async fn timeout_trips() {
        let res = bash(true)
            .dispatch("Bash", r#"{"command":"sleep 5","timeout_ms":150}"#)
            .await;
        assert!(!res.success);
        assert!(res.raw.contains("timed out"));
    }

    #[tokio::test]
    async fn background_spawn_poll_kill() {
        let bt = bash(true);
        let started = bt
            .dispatch("Bash", r#"{"command":"echo bg-line","run_in_background":true}"#)
            .await;
        assert!(started.success, "{}", started.raw);
        // Extract the shell id (format: "started in background as bash_N").
        let id = started
            .raw
            .split("as ")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .unwrap()
            .to_string();
        // Give the reader task a moment to capture the echo.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let out = bt.dispatch("BashOutput", &format!(r#"{{"shell_id":"{id}"}}"#)).await;
        assert!(out.success, "{}", out.raw);
        assert!(out.raw.contains("bg-line"), "got: {}", out.raw);
        // Second poll → no new output.
        let again = bt.dispatch("BashOutput", &format!(r#"{{"shell_id":"{id}"}}"#)).await;
        assert!(again.raw.contains("no new output"));
        // Kill succeeds, then unknown.
        let killed = bt.dispatch("KillShell", &format!(r#"{{"shell_id":"{id}"}}"#)).await;
        assert!(killed.success, "{}", killed.raw);
        let gone = bt.dispatch("KillShell", &format!(r#"{{"shell_id":"{id}"}}"#)).await;
        assert!(!gone.success);
    }
}
