//! Headless end-to-end multi-agent demo. Validates Sprint 2.3 + 2.4 +
//! the duplicate-ANSWER fix without needing the Tauri window.
//!
//! Run with:
//!     cd src-tauri && cargo run --example multi_agent_demo --release
//!
//! Uses a fresh temp session dir so it doesn't collide with the running
//! app's `~/.aidock/sessions/default/`. Requires `~/.aidock/api_keys.json`
//! to contain a `bailian` entry (set via the BYOK UI).

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use aidock_lib::agent::approval::{new_pending_approvals, ApprovalRegistry};
use aidock_lib::agent::Session;
use aidock_lib::keyring_store;
use serde_json::Value;
use tokio::time::sleep;

const SCENARIO: &str = "做一个简单的 todo Web 应用，要登录、新建、列表、勾选完成。纯 HTML/CSS/JS，不用 React。";

/// Stop polling after this much wall time, regardless of activity.
const MAX_WAIT: Duration = Duration::from_secs(180);
/// If no new message for this long, declare the conversation quiet.
const QUIET_THRESHOLD: Duration = Duration::from_secs(40);
const POLL_INTERVAL: Duration = Duration::from_secs(3);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "aidock=info,warn".into()),
        )
        .init();

    let api_key = keyring_store::load_api_key("bailian")?
        .ok_or_else(|| anyhow::anyhow!(
            "no 'bailian' key in ~/.aidock/api_keys.json — set one via the app's BYOK UI first"
        ))?;

    // Fresh session dir each run; preserves the previous run as a sibling
    // for offline inspection.
    let session_dir = std::env::temp_dir().join(format!(
        "aidock-multi-agent-demo-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&session_dir)?;
    let messages_path = session_dir.join("messages.jsonl");

    println!("┌─ Multi-agent demo ─────────────────────────────────");
    println!("│ session dir : {}", session_dir.display());
    println!("│ scenario    : {SCENARIO}");
    println!("└─");
    println!();

    // Headless: pass empty approval state. With app_handle = None the
    // runtime skips the approval gate entirely anyway, so this is just
    // bookkeeping.
    let session = Session::start(
        session_dir.clone(),
        api_key,
        None,
        ApprovalRegistry::new(),
        new_pending_approvals(),
    )
    .await?;

    session
        .submit_user_input(SCENARIO.to_string(), None)
        .await?;
    println!("(user input submitted; agents are thinking...)\n");

    // Poll for new messages until quiet or hard cap reached.
    let started = Instant::now();
    let mut last_count: usize = 0;
    let mut last_activity = Instant::now();

    while started.elapsed() < MAX_WAIT {
        sleep(POLL_INTERVAL).await;
        let count = count_lines(&messages_path).unwrap_or(0);
        if count > last_count {
            let elapsed_s = started.elapsed().as_secs();
            for i in last_count..count {
                if let Some(line) = read_line(&messages_path, i) {
                    print_one(elapsed_s, i + 1, &line);
                }
            }
            last_count = count;
            last_activity = Instant::now();
        }
        if last_activity.elapsed() >= QUIET_THRESHOLD && last_count > 1 {
            println!(
                "\n(no new message for {}s — declaring quiet)",
                QUIET_THRESHOLD.as_secs()
            );
            break;
        }
    }

    if started.elapsed() >= MAX_WAIT {
        println!("\n(hit MAX_WAIT={}s)", MAX_WAIT.as_secs());
    }

    drop(session); // releases the dispatcher cleanly

    println!();
    println!("┌─ Analysis ─────────────────────────────────────────");
    analyze(&messages_path);
    print_workspace(&session_dir);
    println!("└─");
    println!();
    println!("Full log: {}", messages_path.display());

    Ok(())
}

/// Sprint 4: enumerate everything the agents created under workspace/.
/// Lets us see at a glance whether they exercised the filesystem MCP
/// tools or stayed chat-only.
fn print_workspace(session_dir: &Path) {
    let ws = session_dir.join("workspace");
    if !ws.exists() {
        println!("│ workspace: (not created — MCP didn't initialise)");
        return;
    }
    let mut files = Vec::new();
    collect_files(&ws, &ws, &mut files);
    if files.is_empty() {
        println!("│ workspace: empty — agents didn't write any files this run");
        return;
    }
    println!("│ workspace: {} file(s) written", files.len());
    for (rel, size) in &files {
        println!("│   {} ({} bytes)", rel, size);
    }
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            collect_files(root, &p, out);
        } else {
            let rel = p
                .strip_prefix(root)
                .map(|r| r.display().to_string())
                .unwrap_or_else(|_| p.display().to_string());
            out.push((rel, md.len()));
        }
    }
}

fn count_lines(path: &Path) -> std::io::Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    Ok(std::fs::read_to_string(path)?.lines().count())
}

fn read_line(path: &Path, idx: usize) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content.lines().nth(idx).map(|s| s.to_string())
}

fn print_one(elapsed_s: u64, n: usize, raw: &str) {
    let v: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            println!("  [{elapsed_s:>3}s] #{n:<2} (malformed line)");
            return;
        }
    };
    let sender = v.get("sender").and_then(Value::as_str).unwrap_or("?");
    let topic = v.get("topic_id").and_then(Value::as_str).unwrap_or("?");
    let kind = v.get("kind").and_then(|k| k.get("type")).and_then(Value::as_str).unwrap_or("?");
    let body = preview(v.get("kind").unwrap_or(&Value::Null));
    println!(
        "  [{elapsed_s:>3}s] #{n:<2} {topic:<10} {sender:>13} {kind:<11} {body}"
    );
}

fn preview(kind: &Value) -> String {
    let t = kind.get("type").and_then(Value::as_str).unwrap_or("");
    let take = |s: &str, n: usize| -> String {
        s.chars().take(n).collect::<String>().replace('\n', " ⏎ ")
    };
    match t {
        "BROADCAST" | "USER_INPUT" => take(
            kind.get("content").and_then(Value::as_str).unwrap_or(""),
            120,
        ),
        "ASK_AGENT" => format!(
            "@{} {}",
            kind.get("to").and_then(Value::as_str).unwrap_or("?"),
            take(
                kind.get("content").and_then(Value::as_str).unwrap_or(""),
                100
            )
        ),
        "ANSWER" => {
            let rt = kind
                .get("reply_to")
                .and_then(Value::as_str)
                .map(|s| s.chars().take(8).collect::<String>())
                .unwrap_or_default();
            format!(
                "(re {rt}) {}",
                take(
                    kind.get("content").and_then(Value::as_str).unwrap_or(""),
                    100
                )
            )
        }
        "WORK_START" => take(
            kind.get("task").and_then(Value::as_str).unwrap_or(""),
            120,
        ),
        "DONE" => take(
            kind.get("summary").and_then(Value::as_str).unwrap_or(""),
            120,
        ),
        "PROGRESS" => format!(
            "{} {}% {}",
            kind.get("task").and_then(Value::as_str).unwrap_or(""),
            kind.get("percent").and_then(Value::as_u64).unwrap_or(0),
            kind.get("note").and_then(Value::as_str).unwrap_or("")
        ),
        "SUMMARY" => take(
            kind.get("summary").and_then(Value::as_str).unwrap_or(""),
            120,
        ),
        _ => String::new(),
    }
}

fn analyze(path: &Path) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            println!("│ could not read messages.jsonl: {e}");
            return;
        }
    };
    let mut total = 0;
    let mut by_kind: HashMap<String, usize> = HashMap::new();
    let mut by_sender: HashMap<String, usize> = HashMap::new();
    let mut answer_keys_seen: HashMap<(String, String), usize> = HashMap::new();
    let mut duplicate_answers: Vec<(usize, String, String)> = Vec::new();
    let mut topics_seen: std::collections::BTreeSet<String> = Default::default();

    for (idx, line) in content.lines().enumerate() {
        let n = idx + 1;
        total += 1;
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let kind = v
            .get("kind")
            .and_then(|k| k.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let sender = v
            .get("sender")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        if let Some(t) = v.get("topic_id").and_then(Value::as_str) {
            topics_seen.insert(t.to_string());
        }
        *by_kind.entry(kind.clone()).or_default() += 1;
        *by_sender.entry(sender.clone()).or_default() += 1;

        if kind == "ANSWER" {
            if let Some(reply_to) = v
                .get("kind")
                .and_then(|k| k.get("reply_to"))
                .and_then(Value::as_str)
            {
                let key = (sender.clone(), reply_to.to_string());
                if let Some(prev) = answer_keys_seen.get(&key) {
                    duplicate_answers.push((n, sender.clone(), reply_to.to_string()));
                    println!(
                        "│ ⚠ duplicate ANSWER #{n} by {sender} to {} (first seen at #{prev})",
                        &reply_to[..reply_to.len().min(12)]
                    );
                    let _ = prev;
                } else {
                    answer_keys_seen.insert(key, n);
                }
            }
        }
    }

    println!("│ total messages : {total}");
    println!("│ topics seen    : {}", topics_seen.len());
    for t in &topics_seen {
        println!("│   {t}");
    }
    println!("│ by sender:");
    let mut by_sender_vec: Vec<_> = by_sender.iter().collect();
    by_sender_vec.sort();
    for (s, c) in by_sender_vec {
        println!("│   {s:>13}: {c}");
    }
    println!("│ by kind:");
    let mut by_kind_vec: Vec<_> = by_kind.iter().collect();
    by_kind_vec.sort();
    for (k, c) in by_kind_vec {
        println!("│   {k:>11}: {c}");
    }
    if duplicate_answers.is_empty() {
        println!("│ duplicate ANSWERs: ✓ none (Sprint 2.4 fix verified)");
    } else {
        println!(
            "│ duplicate ANSWERs: ✗ {} (Sprint 2.4 fix REGRESSED)",
            duplicate_answers.len()
        );
    }
}
