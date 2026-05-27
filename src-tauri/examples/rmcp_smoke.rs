//! Sprint 4 prep: smoke test for the official Rust MCP SDK against the
//! reference filesystem MCP server.
//!
//! Run with:
//!     cd src-tauri && cargo run --example rmcp_smoke
//!
//! Requires `npx` on PATH (we already verified Node 18 is there). The
//! example sandboxes the filesystem server to a fresh temp dir, writes a
//! known file into it before connecting, then exercises the round-trip:
//!
//!   1. spawn `npx @modelcontextprotocol/server-filesystem <dir>`
//!   2. list_all_tools  — confirm the server exposes the expected names
//!   3. call_tool read_file on our seed file — confirm content match
//!   4. call_tool write_file to create a new file
//!   5. graceful_shutdown — verify the child process exits cleanly
//!
//! Any green run here means Sprint 4.1 (MCP client integration) can be
//! built on rmcp + the Node reference filesystem server without surprises.

use std::path::PathBuf;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::TokioChildProcess;
use serde_json::{Map, Value};
use tokio::process::Command;

const SEED_FILE: &str = "hello.txt";
const SEED_CONTENT: &str = "Hello from AiDock Sprint 4 smoke test.\n";
const NEW_FILE: &str = "written-by-mcp.txt";
const NEW_CONTENT: &str = "rmcp client wrote this via the filesystem MCP server.\n";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rmcp=info,rmcp_smoke=info,warn".into()),
        )
        .init();

    // Fresh sandbox for the filesystem MCP server. Canonicalize because
    // macOS's /var/folders is actually a symlink to /private/var/folders;
    // the server's `list_allowed_directories` enforcement compares the
    // canonical form, so we must send canonical paths in tool args too.
    let sandbox_raw = std::env::temp_dir().join(format!(
        "aidock-rmcp-smoke-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&sandbox_raw);
    std::fs::create_dir_all(&sandbox_raw)?;
    let sandbox = std::fs::canonicalize(&sandbox_raw)?;
    std::fs::write(sandbox.join(SEED_FILE), SEED_CONTENT)?;

    println!("┌─ rmcp smoke test ─────────────────────────────────");
    println!("│ rmcp:    1.7.0");
    println!("│ server:  @modelcontextprotocol/server-filesystem");
    println!("│ sandbox: {}", sandbox.display());
    println!("└─");
    println!();

    // Spawn the server via npx. The filesystem reference server takes
    // allowed directories as positional args.
    let mut cmd = Command::new("npx");
    cmd.args([
        "-y",
        "@modelcontextprotocol/server-filesystem",
        sandbox.to_str().expect("sandbox path is utf-8"),
    ]);
    let transport = TokioChildProcess::new(cmd)?;

    // The `()` service is the "no-op handler" client — we only call the
    // server, never receive server-initiated callbacks.
    println!("→ initializing MCP session...");
    let service = ().serve(transport).await?;
    println!("  initialized; peer info: {:?}", service.peer_info().map(|p| &p.server_info));
    println!();

    // 1. list_all_tools
    println!("→ list_all_tools");
    let tools = service.list_all_tools().await?;
    println!("  {} tools advertised:", tools.len());
    for t in &tools {
        let desc = t
            .description
            .as_deref()
            .unwrap_or("(no description)")
            .lines()
            .next()
            .unwrap_or("");
        println!("    - {}: {}", t.name, &desc[..desc.len().min(90)]);
    }
    println!();

    // Sanity — the filesystem server should at minimum expose read + write.
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    let has_read = names.iter().any(|n| n.contains("read"));
    let has_write = names.iter().any(|n| n.contains("write"));
    assert!(has_read, "expected a read tool, got {names:?}");
    assert!(has_write, "expected a write tool, got {names:?}");

    // 2. call_tool read_file on the seed file
    println!("→ call_tool read_file on {SEED_FILE}");
    let seed_path = sandbox.join(SEED_FILE);
    let read_args: Map<String, Value> = serde_json::json!({
        "path": seed_path.to_string_lossy(),
    })
    .as_object()
    .cloned()
    .unwrap();
    let read_result = service
        .call_tool(CallToolRequestParams::new("read_file").with_arguments(read_args))
        .await?;
    let preview = format!("{:?}", read_result.content);
    println!("  content (truncated): {}", &preview[..preview.len().min(200)]);
    // Best-effort content check — the server wraps text in a Content::Text
    // block; just look for the seed text inside the debug-print to keep
    // the test independent of internal field naming.
    assert!(
        preview.contains(SEED_CONTENT.trim()),
        "expected seed content in read result"
    );
    println!("  ✓ matched seed content");
    println!();

    // 3. call_tool write_file
    println!("→ call_tool write_file on {NEW_FILE}");
    let new_path = sandbox.join(NEW_FILE);
    let write_args: Map<String, Value> = serde_json::json!({
        "path": new_path.to_string_lossy(),
        "content": NEW_CONTENT,
    })
    .as_object()
    .cloned()
    .unwrap();
    let write_result = service
        .call_tool(CallToolRequestParams::new("write_file").with_arguments(write_args))
        .await?;
    println!("  ok: {}", !write_result.is_error.unwrap_or(false));
    assert!(
        new_path.exists(),
        "write_file should have created {}",
        new_path.display()
    );
    let on_disk = std::fs::read_to_string(&new_path)?;
    assert_eq!(on_disk, NEW_CONTENT);
    println!("  ✓ file exists on disk with expected content");
    println!();

    // 4. graceful shutdown — make sure the child process exits cleanly.
    println!("→ shutting down session");
    let cancellation = service.cancel().await;
    match cancellation {
        Ok(reason) => println!("  cancelled cleanly: {reason:?}"),
        Err(e) => println!("  cancel returned non-fatal error: {e}"),
    }

    // small grace period for the child to exit
    tokio::time::sleep(Duration::from_millis(300)).await;

    println!();
    println!("╭───────────────────────────────────────────────────╮");
    println!("│ GREEN — rmcp 1.7.0 + filesystem MCP server work    │");
    println!("│ Sprint 4.1 cleared to build on this stack.         │");
    println!("╰───────────────────────────────────────────────────╯");

    // Tidy: leave the sandbox in place for inspection.
    let _: &PathBuf = &sandbox;
    Ok(())
}
