//! ④ LSP 工具 —— 代码语义（跳定义/找引用/诊断/hover）。实施计划见 CLAUDE.md
//! ④「LSP（路2）」。本文件 = 工具门面 + 操作执行 + 降级；客户端在 `lsp_client.rs`，
//! 会话级 server 池在 `lsp_pool.rs`。
//!
//! **进度**：✅ P0 工具/路由 · ✅ P2 客户端内核 · ✅ P3 池 · ✅ P4 操作接线 ·
//! ✅ P5 降级（无 server/未配语言 → 回落 Grep + Bash 检查器引导）。
//!   真 server 查询路径靠 dev 机 smoke 验；格式化 + run_operation（duplex 假 client）已测。
//!
//! 危险级 L0（只读语义）。位置参数对工具是 1-based（对齐 Read 的行号），内部转 LSP 的 0-based。

use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::Duration;

use crate::agent::lsp_client::LspClient;
use crate::agent::lsp_pool::{path_to_uri, server_for_path, LspPool};
use crate::agent::react::ExecResult;
use crate::llm::Tool;

pub const TOOL_LSP: &str = "LSP";

/// True iff `name` is the LSP tool (executor routing).
pub fn is_lsp_tool(name: &str) -> bool {
    name == TOOL_LSP
}

/// The `LSP` tool schema (CC-aligned, operation-parameterized).
pub fn lsp_tool() -> Tool {
    Tool::function(
        TOOL_LSP,
        "Code intelligence from a language server: jump to a symbol's definition, find all \
         references, get type/compile errors (diagnostics), or hover type info. Semantic, not \
         text search — prefer it over Grep for precise navigation. line/character are 1-based \
         (like Read's line numbers). Needs the language server installed (rust-analyzer, \
         typescript-language-server, pyright, gopls); if unavailable it tells you to fall back \
         to Grep + a type-checker via Bash.",
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["definition", "references", "diagnostics", "hover"]
                },
                "file_path": { "type": "string", "description": "Target file (workspace-relative or absolute)." },
                "line": { "type": "integer", "description": "1-based line (required for definition/references/hover)." },
                "character": { "type": "integer", "description": "1-based column (default 1)." }
            },
            "required": ["operation", "file_path"]
        }),
    )
}

#[derive(Debug, Deserialize)]
pub struct LspArgs {
    pub operation: String,
    pub file_path: String,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub character: Option<u32>,
}

/// Execute an LSP tool call against the session's [`LspPool`]. Routes:
/// unconfigured language / server-not-installed → [`degraded`] (P5).
pub async fn execute_lsp(pool: &LspPool, args: &str) -> ExecResult {
    let a: LspArgs = match serde_json::from_str(args) {
        Ok(a) => a,
        Err(e) => return err(format!("LSP bad args: {e}")),
    };

    let Some(spec) = server_for_path(&a.file_path) else {
        return degraded(
            &a.operation,
            &format!("no language server configured for '{}'", a.file_path),
        );
    };

    // Validate args BEFORE starting a server (fail fast; don't spawn on bad input).
    let needs_pos = matches!(a.operation.as_str(), "definition" | "references" | "hover");
    if needs_pos && a.line.is_none() {
        return err(format!(
            "LSP {} needs `line` (1-based) and `character`",
            a.operation
        ));
    }
    // tool 1-based → LSP 0-based.
    let line0 = a.line.unwrap_or(1).saturating_sub(1) as u64;
    let char0 = a.character.unwrap_or(1).saturating_sub(1) as u64;

    let client = match pool.get_or_start(spec).await {
        Ok(c) => c,
        Err(reason) => return degraded(&a.operation, &reason),
    };

    let p = Path::new(&a.file_path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        pool.workspace().join(p)
    };
    let abs = std::fs::canonicalize(&abs).unwrap_or(abs);

    run_operation(&client, spec.language_id, &a.operation, &abs, line0, char0).await
}

/// Given a connected client, didOpen the file then run one operation. Split out
/// so it's testable with a `duplex` fake client (the pool's real spawn isn't).
async fn run_operation(
    client: &LspClient,
    language_id: &str,
    op: &str,
    abs: &Path,
    line0: u64,
    char0: u64,
) -> ExecResult {
    let uri = path_to_uri(abs);
    let text = std::fs::read_to_string(abs).unwrap_or_default();
    let _ = client
        .notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": language_id, "version": 1, "text": text } }),
        )
        .await;

    let doc = json!({ "uri": uri });
    let pos = json!({ "line": line0, "character": char0 });

    match op {
        "definition" => match client
            .request("textDocument/definition", json!({ "textDocument": doc, "position": pos }))
            .await
        {
            Ok(v) => ok(format_locations(&v)),
            Err(e) => err(e),
        },
        "references" => match client
            .request(
                "textDocument/references",
                json!({ "textDocument": doc, "position": pos, "context": { "includeDeclaration": true } }),
            )
            .await
        {
            Ok(v) => ok(format_locations(&v)),
            Err(e) => err(e),
        },
        "hover" => match client
            .request("textDocument/hover", json!({ "textDocument": doc, "position": pos }))
            .await
        {
            Ok(v) => ok(format_hover(&v)),
            Err(e) => err(e),
        },
        "diagnostics" => {
            // Diagnostics are pushed async after didOpen; poll briefly.
            for _ in 0..30 {
                if let Some(p) = client.diagnostics_for(&uri).await {
                    return ok(format_diagnostics(&p));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            ok("(no diagnostics reported yet — the server may still be analyzing; run \
                `cargo check` / `tsc --noEmit` / `pyright` via Bash for a definitive check)"
                .to_string())
        }
        other => err(format!("unknown LSP operation '{other}'")),
    }
}

// ---------------------------------------------------------------------------
// Pure formatters: LSP response JSON → concise text for the LLM. Testable.
// ---------------------------------------------------------------------------

fn uri_to_path(uri: &str) -> &str {
    uri.strip_prefix("file://").unwrap_or(uri)
}

fn pos_line_col(range: &Value) -> (u64, u64) {
    let l = range.pointer("/start/line").and_then(|x| x.as_u64()).unwrap_or(0);
    let c = range.pointer("/start/character").and_then(|x| x.as_u64()).unwrap_or(0);
    (l + 1, c + 1) // back to 1-based for display
}

/// `Location` | `Location[]` | `LocationLink[]` → `path:line:col` lines.
fn format_locations(v: &Value) -> String {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Null => vec![],
        obj => vec![obj],
    };
    let mut lines = Vec::new();
    for it in items {
        let (uri, range) = if let Some(u) = it.get("uri").and_then(|x| x.as_str()) {
            (Some(u), it.get("range"))
        } else if let Some(u) = it.get("targetUri").and_then(|x| x.as_str()) {
            (
                Some(u),
                it.get("targetSelectionRange").or_else(|| it.get("targetRange")),
            )
        } else {
            (None, None)
        };
        if let (Some(uri), Some(range)) = (uri, range) {
            let (l, c) = pos_line_col(range);
            lines.push(format!("{}:{}:{}", uri_to_path(uri), l, c));
        }
    }
    if lines.is_empty() {
        "(no results)".to_string()
    } else {
        lines.join("\n")
    }
}

/// Hover `contents`: MarkupContent | MarkedString | array → plain text.
fn format_hover(v: &Value) -> String {
    let extract = |e: &Value| -> Option<String> {
        match e {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => o.get("value").and_then(|x| x.as_str()).map(|s| s.to_string()),
            _ => None,
        }
    };
    match v.get("contents") {
        Some(Value::Array(a)) => {
            let parts: Vec<String> = a.iter().filter_map(extract).collect();
            if parts.is_empty() {
                "(no hover info)".to_string()
            } else {
                parts.join("\n")
            }
        }
        Some(other) => extract(other).unwrap_or_else(|| "(no hover info)".to_string()),
        None => "(no hover info)".to_string(),
    }
}

/// `publishDiagnostics` params → `path:line:col: severity message` lines.
fn format_diagnostics(params: &Value) -> String {
    let path = uri_to_path(params.get("uri").and_then(|x| x.as_str()).unwrap_or(""));
    let empty = Vec::new();
    let diags = params
        .get("diagnostics")
        .and_then(|d| d.as_array())
        .unwrap_or(&empty);
    if diags.is_empty() {
        return "(no diagnostics)".to_string();
    }
    let mut out = Vec::new();
    for d in diags {
        let range = d.get("range").cloned().unwrap_or(Value::Null);
        let (l, c) = pos_line_col(&range);
        let sev = match d.get("severity").and_then(|x| x.as_u64()) {
            Some(1) => "error",
            Some(2) => "warning",
            Some(3) => "info",
            Some(4) => "hint",
            _ => "note",
        };
        let msg = d.get("message").and_then(|x| x.as_str()).unwrap_or("");
        out.push(format!("{path}:{l}:{c}: {sev}: {msg}"));
    }
    out.join("\n")
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn ok(s: String) -> ExecResult {
    ExecResult { raw: s, success: true }
}
fn err(s: impl Into<String>) -> ExecResult {
    ExecResult {
        raw: format!("ERROR: {}", s.into()),
        success: false,
    }
}

/// P5 degradation: LSP unavailable → guide to the fallbacks (Grep + Bash
/// checker) and how to enable it. `success: true` (informational, not a failure
/// that should trip the loop's backstop). The message also reaches the UI via
/// the unified TOOL_CALL_EVENT, so the "install X" hint surfaces there too.
fn degraded(op: &str, reason: &str) -> ExecResult {
    ok(format!(
        "LSP ({op}) unavailable: {reason}.\nFallback: navigate with Grep (find a symbol's \
         uses by name); for type/compile errors run the project's checker via Bash \
         (`cargo check` / `tsc --noEmit` / `pyright`). Install the matching language server \
         to enable real LSP."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::lsp_client::{read_message, write_message};
    use serde_json::json;
    use tokio::io::BufReader;

    #[test]
    fn tool_and_predicate() {
        assert_eq!(lsp_tool().function.name, TOOL_LSP);
        assert!(is_lsp_tool("LSP"));
        assert!(!is_lsp_tool("Read"));
    }

    #[test]
    fn format_locations_single_and_array_and_link() {
        let single = json!({ "uri": "file:///ws/a.rs", "range": { "start": { "line": 9, "character": 4 } } });
        assert_eq!(format_locations(&single), "/ws/a.rs:10:5");

        let arr = json!([
            { "uri": "file:///ws/a.rs", "range": { "start": { "line": 0, "character": 0 } } },
            { "uri": "file:///ws/b.rs", "range": { "start": { "line": 41, "character": 7 } } }
        ]);
        assert_eq!(format_locations(&arr), "/ws/a.rs:1:1\n/ws/b.rs:42:8");

        let link = json!([
            { "targetUri": "file:///ws/c.rs", "targetSelectionRange": { "start": { "line": 2, "character": 3 } } }
        ]);
        assert_eq!(format_locations(&link), "/ws/c.rs:3:4");

        assert_eq!(format_locations(&Value::Null), "(no results)");
    }

    #[test]
    fn format_hover_variants() {
        assert_eq!(
            format_hover(&json!({ "contents": { "kind": "markdown", "value": "fn foo() -> u8" } })),
            "fn foo() -> u8"
        );
        assert_eq!(format_hover(&json!({ "contents": "plain string" })), "plain string");
        assert_eq!(
            format_hover(&json!({ "contents": ["a", { "value": "b" }] })),
            "a\nb"
        );
    }

    #[test]
    fn format_diagnostics_lines() {
        let p = json!({
            "uri": "file:///ws/x.rs",
            "diagnostics": [
                { "range": { "start": { "line": 3, "character": 0 } }, "severity": 1, "message": "mismatched types" },
                { "range": { "start": { "line": 9, "character": 2 } }, "severity": 2, "message": "unused var" }
            ]
        });
        assert_eq!(
            format_diagnostics(&p),
            "/ws/x.rs:4:1: error: mismatched types\n/ws/x.rs:10:3: warning: unused var"
        );
        assert_eq!(
            format_diagnostics(&json!({ "uri": "file:///ws/x.rs", "diagnostics": [] })),
            "(no diagnostics)"
        );
    }

    #[tokio::test]
    async fn degraded_for_unconfigured_language() {
        // .md has no configured server → degraded (no real pool spawn attempted).
        let pool = LspPool::new(std::env::temp_dir());
        let res = execute_lsp(&pool, r#"{"operation":"definition","file_path":"README.md","line":1,"character":1}"#).await;
        assert!(res.success); // informational
        assert!(res.raw.contains("Fallback"));
        assert!(res.raw.contains("Grep"));
    }

    #[tokio::test]
    async fn position_required_error() {
        // Missing line for a position op → arg error, validated BEFORE any server
        // spawn (fail fast). Deterministic regardless of whether rust-analyzer is
        // installed on the dev machine.
        let pool = LspPool::new(std::env::temp_dir());
        let res = execute_lsp(&pool, r#"{"operation":"definition","file_path":"x.rs"}"#).await;
        assert!(!res.success);
        assert!(res.raw.contains("needs `line`"), "{}", res.raw);
    }

    /// run_operation against a duplex fake server scripted to answer definition.
    #[tokio::test]
    async fn run_operation_definition_via_fake_server() {
        let (client_end, server_end) = tokio::io::duplex(64 * 1024);
        let (cr, cw) = tokio::io::split(client_end);
        let client = LspClient::start(Box::new(cw), Box::new(cr));

        let (sr, mut sw) = tokio::io::split(server_end);
        let mut sr = BufReader::new(sr);
        let server = tokio::spawn(async move {
            // Read frames until we see the definition request (skip didOpen notify).
            loop {
                let bytes = read_message(&mut sr).await.unwrap().unwrap();
                let msg: Value = serde_json::from_slice(&bytes).unwrap();
                if msg["method"] == "textDocument/definition" {
                    let id = msg["id"].as_i64().unwrap();
                    let resp = json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": { "uri": "file:///ws/def.rs", "range": { "start": { "line": 11, "character": 8 } } }
                    });
                    write_message(&mut sw, &serde_json::to_vec(&resp).unwrap()).await.unwrap();
                    break;
                }
            }
        });

        let res = run_operation(&client, "rust", "definition", Path::new("/ws/x.rs"), 0, 0).await;
        assert!(res.success, "{}", res.raw);
        assert_eq!(res.raw, "/ws/def.rs:12:9");
        server.await.unwrap();
    }
}
