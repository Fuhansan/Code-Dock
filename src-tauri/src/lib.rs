//! AiDock — self-assembled multi-agent AI workshop platform.
//!
//! Library crate. The binary in `main.rs` only calls `run()` after setting the
//! Windows subsystem attribute. Module layout follows AIDOCK_DESIGN.md §12.2:
//!
//! ```text
//! agent/        — Agent runtime, state machine, router (Sprint 2+)
//! message/      — Typed messages, JSONL stream (Sprint 2+)
//! llm/          — Provider trait + Bailian impl  ← Sprint 1
//! mcp/          — MCP client (Sprint 4)
//! storage/      — session_state + messages.jsonl (Sprint 3)
//! keyring_store — API key persistence            ← Sprint 1
//! commands      — Tauri IPC surface              ← Sprint 1
//! ```

mod commands;
// `keyring_store`, `llm`, and `agent` are `pub` so dev tools under `examples/`
// can reuse them. The Tauri command surface stays private — frontend talks
// through `commands` only.
pub mod agent;
pub mod keyring_store;
pub mod llm;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Structured logging. RUST_LOG controls verbosity; default to info for the
    // aidock crate, warn elsewhere.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "aidock=info,warn".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(commands::AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::greet,
            commands::provider_status,
            commands::save_api_key,
            commands::send_chat_message,
            commands::start_session,
            commands::new_session,
            commands::resume_session,
            commands::list_sessions,
            commands::current_session,
            commands::rename_session,
            commands::delete_session,
            commands::session_status,
            commands::send_user_message,
            commands::load_message_history,
            commands::load_mcp_call_history,
            commands::respond_to_approval,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
