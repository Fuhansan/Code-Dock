//! ④ LSP dev-smoke：把真 rust-analyzer 的 spawn + initialize 握手 + 4 个操作在一个
//! **真 Cargo 工程**上跑一遍（单测只用 duplex 假服务器验了协议层；这层薄胶水靠它验）。
//!
//! 运行：
//!     cd src-tauri && cargo run --example lsp_smoke
//!
//! 需要 `rust-analyzer` 在 PATH（`rustup component add rust-analyzer` 或包管理器装）。
//! 没装则打印提示并跳过（退出 0）。
//!
//! 它会：
//!   1. 在临时目录写一个最小 Cargo 工程（Cargo.toml + src/lib.rs，含一个定义、两处引用、
//!      一个类型错误）。
//!   2. 建 `LspPool` 指向它（首个操作触发 spawn + initialize 握手）。
//!   3. 跑 definition / references / hover / diagnostics，逐个打印结果 + 耗时。
//!
//! 绿（每个操作返回合理的 `path:line:col` / 类型错误）= P4/P5 的真 server 路径通了。
//! 若卡在第一个操作很久 = initialize 握手有问题（见 CLAUDE.md ④ LSP dev-smoke 待办）。

use std::path::PathBuf;
use std::time::Instant;

use aidock_lib::agent::lsp::execute_lsp;
use aidock_lib::agent::lsp_pool::{binary_on_path, LspPool};

const CARGO_TOML: &str = "[package]\nname = \"lsp-smoke-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n";

// Line/col references used below are 1-based (matching the LSP tool).
//   line 1, col 8   = `greet` definition
//   line 6, col 13  = `greet` use site (let g = greet(...))
//   line 7          = a type error (String assigned to u8)
const LIB_RS: &str = "pub fn greet(name: &str) -> String {\n\
    \x20   format!(\"hello, {name}\")\n\
    }\n\
    \n\
    pub fn use_greet() {\n\
    \x20   let g = greet(\"world\");\n\
    \x20   let _bad: u8 = greet(\"x\");\n\
    \x20   println!(\"{g}\");\n\
    }\n";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if !binary_on_path("rust-analyzer") {
        eprintln!(
            "rust-analyzer not on PATH — install it (`rustup component add rust-analyzer`) \
             to run this smoke test. Skipping."
        );
        return Ok(());
    }

    // 1. Write the fixture project.
    let proj: PathBuf = std::env::temp_dir().join("aidock-lsp-smoke-fixture");
    std::fs::create_dir_all(proj.join("src"))?;
    std::fs::write(proj.join("Cargo.toml"), CARGO_TOML)?;
    std::fs::write(proj.join("src/lib.rs"), LIB_RS)?;
    println!("fixture project at {}", proj.display());
    println!("--- src/lib.rs ---\n{LIB_RS}------------------\n");

    // 2. Pool pointed at the project.
    let pool = LspPool::new(&proj);

    // 3. Run the four operations. The FIRST call triggers spawn + initialize, so
    //    its timing includes the handshake (watch for a 30s timeout here).
    run(&pool, "definition (use site → def)", r#"{"operation":"definition","file_path":"src/lib.rs","line":6,"character":13}"#).await;
    run(&pool, "references (def → call sites)", r#"{"operation":"references","file_path":"src/lib.rs","line":1,"character":8}"#).await;
    run(&pool, "hover (def)", r#"{"operation":"hover","file_path":"src/lib.rs","line":1,"character":8}"#).await;
    run(&pool, "diagnostics (whole file)", r#"{"operation":"diagnostics","file_path":"src/lib.rs"}"#).await;

    println!("\nsmoke done. Eyeball the results above:");
    println!("  · definition → should point at src/lib.rs:1:8 (greet)");
    println!("  · references → should list the two call sites (lines 6 and 7)");
    println!("  · hover      → should show `fn greet(name: &str) -> String`");
    println!("  · diagnostics→ should report a type error around line 7 (String vs u8)");
    Ok(())
}

async fn run(pool: &LspPool, label: &str, args: &str) {
    let t = Instant::now();
    let res = execute_lsp(pool, args).await;
    let ms = t.elapsed().as_millis();
    println!(
        "== {label}  [{ms} ms, success={}]\n{}\n",
        res.success, res.raw
    );
}
