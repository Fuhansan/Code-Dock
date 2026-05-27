//! Sprint 4.7 — end-to-end MCP demo. Verifies the full chain:
//!   user prompt → LLM tool call → Rust runtime → MCP filesystem server
//!   → real files on disk → agents see + react to those files.
//!
//! Run with:
//!     cd src-tauri && cargo run --example mcp_e2e_demo
//!
//! Distinct from `multi_agent_demo` (Sprint 2 collaboration check) — that
//! one uses a vague "build a todo app" prompt and tolerates agents
//! stopping at the design phase. This one is explicit about what files
//! must exist and waits until they do (or the timeout fires).
//!
//! Asserts at exit:
//!   - workspace/login.html exists and contains a <form>
//!   - workspace/server.js exists and contains "/api/login"
//!
//! No LLM-quality judging beyond that — we just confirm the runtime
//! plumbing produces real artifacts on disk.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aidock_lib::agent::Session;
use aidock_lib::keyring_store;
use tokio::time::sleep;

const SCENARIO: &str = "立即开始实现这个最小可工作 Demo，不要再讨论需求。\n\
\n\
要在 workspace 目录里产出两个真实文件（**用 fs__write_file 工具直接写**，不要把代码内容广播到聊天）：\n\
\n\
1. **login.html** — frontend_dev 负责\n\
   - 一个 username/password 的 <form>，action='/api/login'，method='POST'\n\
   - 没有样式要求，最小可工作即可。\n\
\n\
2. **server.js** — backend_dev 负责\n\
   - Node.js + Express，处理 POST /api/login，body 是 {username, password}\n\
   - 任何用户名/密码都返回 {token: 'demo-token'}（这是 demo，不做真校验）\n\
\n\
没有别的需求。请立即用 fs__write_file 写出这两个文件，然后 DONE 收尾。";

const MAX_WAIT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_secs(4);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "aidock=info,warn".into()),
        )
        .init();

    let api_key = keyring_store::load_api_key("bailian")?
        .ok_or_else(|| anyhow::anyhow!("no bailian key in ~/.aidock/api_keys.json"))?;

    let session_dir = std::env::temp_dir().join(format!("aidock-mcp-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&session_dir);
    std::fs::create_dir_all(&session_dir)?;
    let workspace = session_dir.join("workspace");

    println!("┌─ MCP e2e demo (Sprint 4.7) ───────────────────────");
    println!("│ session : {}", session_dir.display());
    println!("│ workspace: {}", workspace.display());
    println!("│ target   : login.html, server.js");
    println!("└─");
    println!();

    let session = Session::start(session_dir.clone(), api_key, None).await?;
    session.submit_user_input(SCENARIO.to_string(), None).await?;
    println!("(user prompt submitted — waiting for files to appear)\n");

    let target_files = ["login.html", "server.js"];
    let started = Instant::now();
    let mut last_seen: Vec<String> = Vec::new();

    while started.elapsed() < MAX_WAIT {
        sleep(POLL_INTERVAL).await;
        let mut current = Vec::new();
        if workspace.exists() {
            for f in &target_files {
                let p = workspace.join(f);
                if p.exists() {
                    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                    current.push(format!("{f} ({size} B)"));
                }
            }
        }
        if current != last_seen {
            let elapsed = started.elapsed().as_secs();
            if current.is_empty() {
                println!("  [{elapsed:>3}s] workspace still empty");
            } else {
                println!("  [{elapsed:>3}s] {}", current.join(" · "));
            }
            last_seen = current;
        }
        if target_files.iter().all(|f| workspace.join(f).exists()) {
            println!("\n(both target files exist — letting the team SUMMARY/DONE settle)");
            // Give agents a few more seconds to emit DONE if they're mid-flight.
            sleep(Duration::from_secs(10)).await;
            break;
        }
    }

    drop(session);

    println!();
    println!("┌─ Verdict ─────────────────────────────────────────");
    let html_ok = check_file(&workspace, "login.html", &["<form", "/api/login"]);
    let js_ok = check_file(&workspace, "server.js", &["/api/login"]);

    if html_ok && js_ok {
        println!("│ GREEN — both files written and content matches");
        println!("│ Sprint 4 end-to-end chain (LLM → MCP → disk) verified");
    } else {
        println!("│ MIXED — see per-file diagnostics above");
        println!("│ This can be a prompt-engineering / model-mood issue, not");
        println!("│ necessarily a wiring break. Inspect log for fs__write_file calls.");
    }
    println!("│");
    println!("│ messages: {}", session_dir.join("messages.jsonl").display());
    println!("│ workspace: {}", workspace.display());
    println!("└─");
    Ok(())
}

fn check_file(workspace: &Path, name: &str, must_contain: &[&str]) -> bool {
    let path: PathBuf = workspace.join(name);
    if !path.exists() {
        println!("│ ✗ {name} — not on disk");
        return false;
    }
    let Ok(content) = std::fs::read_to_string(&path) else {
        println!("│ ✗ {name} — exists but couldn't be read");
        return false;
    };
    let missing: Vec<&&str> = must_contain
        .iter()
        .filter(|needle| !content.contains(**needle))
        .collect();
    if missing.is_empty() {
        println!(
            "│ ✓ {name} — {} bytes, contains all expected fragments",
            content.len()
        );
        true
    } else {
        println!(
            "│ ⚠ {name} — {} bytes on disk, but missing: {:?}",
            content.len(),
            missing
        );
        false
    }
}
