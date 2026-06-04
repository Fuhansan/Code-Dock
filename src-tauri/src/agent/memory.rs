//! ④.b — 记忆仓库（骨架）。
//!
//! See CLAUDE.md block ④.b. The agent's private, layered memory, stored as
//! **nested folders** (the directory hierarchy *is* the 大主题 → 小主题 → 详情
//! tree). This module is the pure-Rust mechanical floor: it reads/writes those
//! files and nothing else.
//!
//! **A-档 (防剖方案)**: folder/file names carry NO semantics (sequence only),
//! and file *content* is stored as an obfuscated blob (`cat` → garbage). The
//! LLM only ever sees decoded content (via the bundle / recall_detail), never
//! the raw files. Obfuscation is a SPEED BUMP, not encryption (the keystream
//! lives in the binary); see CLAUDE.md ④.b.
//!
//! The topic file stores the **`Plan` as JSON** (round-trippable — the agent
//! resumes a task by loading its prior plan back as context). markdown is only
//! rendered on the fly for the bundle. The 大主题 ≈ the Plan (CLAUDE.md ④.b 2).
//! Detail files store free text (a summary line + body).
//!
//! Deliberate non-goals here:
//!   - assembling the context bundle for ④.a  → done below (reads from here)
//!   - the guardian summarizer (LLM)           → ④.b's LLM half, MVP-optional
//!   - L3 git capture                          → hooks the ActionExecutor
//!   - how a turn's Plan maps to a topic id     → [`TopicTracker`] below
//!
//! Layout (names sequence-only; content obfuscated):
//! ```text
//! {root}/
//!   topic-001/        # 大主题 = folder
//!     _topic          # obfuscated JSON: { updated_at, plan }
//!     01/             # 小主题 = subfolder (序号)
//!       detail        # obfuscated text: summary line + body
//!     02/
//!       detail
//! ```

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::react::{Plan, PlanStep, StepStatus};
use crate::llm::Tool;

/// On-disk topic payload (obfuscated when written). The Plan is the source of
/// truth; status/markdown are derived on read.
#[derive(Serialize, Deserialize)]
struct TopicFile {
    updated_at: i64,
    plan: Plan,
}

/// Reads/writes one agent's layered memory under a root dir. Cheap to clone
/// (just a PathBuf) — the runtime keeps one and hands a clone to the executor.
#[derive(Clone)]
pub struct MemoryStore {
    root: PathBuf,
}

impl MemoryStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn topic_dir(&self, topic_id: &str) -> PathBuf {
        self.root.join(topic_id)
    }

    fn subtopic_dir(&self, topic_id: &str, step_seq: usize) -> PathBuf {
        self.topic_dir(topic_id).join(format!("{step_seq:02}"))
    }

    /// Create/overwrite a topic's `_topic` from a [`Plan`] (大主题 ≈ the plan).
    /// Stored as obfuscated JSON so it round-trips back to a `Plan`.
    pub fn write_topic(&self, topic_id: &str, plan: &Plan, now_ms: i64) -> std::io::Result<()> {
        let dir = self.topic_dir(topic_id);
        fs::create_dir_all(&dir)?;
        let payload = TopicFile {
            updated_at: now_ms,
            plan: plan.clone(),
        };
        let json = serde_json::to_string(&payload).unwrap_or_default();
        fs::write(dir.join("_topic"), obfuscate(&json))
    }

    /// Load a topic's [`Plan`] back, or None if absent/unreadable.
    pub fn read_topic_plan(&self, topic_id: &str) -> Option<Plan> {
        let raw = fs::read(self.topic_dir(topic_id).join("_topic")).ok()?;
        let json = deobfuscate(&raw)?;
        serde_json::from_str::<TopicFile>(&json).ok().map(|t| t.plan)
    }

    /// Write/overwrite a step's `detail` (obfuscated text). Folder = `{seq:02}`
    /// — sequence-only so the name leaks no step semantics.
    pub fn write_detail(
        &self,
        topic_id: &str,
        step_seq: usize,
        summary: &str,
        body: &str,
    ) -> std::io::Result<()> {
        let dir = self.subtopic_dir(topic_id, step_seq);
        fs::create_dir_all(&dir)?;
        // Summary line first (the「自报家门」one-liner), then the full body.
        fs::write(dir.join("detail"), obfuscate(&format!("> {summary}\n\n{body}\n")))
    }

    /// Read a step's decoded detail by sequence, or None.
    pub fn read_detail(&self, topic_id: &str, step_seq: usize) -> Option<String> {
        let raw = fs::read(self.subtopic_dir(topic_id, step_seq).join("detail")).ok()?;
        deobfuscate(&raw)
    }

    /// Topic ids currently on disk (each is a folder under root). Order is
    /// filesystem-dependent — callers sort if they care.
    pub fn list_topics(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// 回合 → 大主题映射 (CLAUDE.md ④.b: id = topic-{序号}, 闭合 = 全步 Done)
//
// Programmatic, no LLM. Tracks the currently-OPEN topic; the agent's plan (its
// `set_plan` goal) drives the structure. A new id is minted only when none is
// open; an open topic is updated in place; closure is when the plan's steps are
// all Done. Closed-ness is DERIVED from the stored plan, so a crash can't
// corrupt it — `recover` re-reads to find the open topic.
// ---------------------------------------------------------------------------

/// Tracks which 大主题 the agent is working and persists the plan into it.
#[derive(Default)]
pub struct TopicTracker {
    current: Option<String>,
}

impl TopicTracker {
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Persist `plan` to the open topic (minting one if none is open), then
    /// close it (clear `current`) if the plan is now all-done. Returns the
    /// topic id written to. Call after a turn settles.
    pub fn record(&mut self, store: &MemoryStore, plan: &Plan, now_ms: i64) -> std::io::Result<String> {
        let topic_id = match &self.current {
            Some(id) => id.clone(),
            None => {
                let id = mint_topic_id(store);
                self.current = Some(id.clone());
                id
            }
        };
        store.write_topic(&topic_id, plan, now_ms)?;
        if plan_closed(plan) {
            self.current = None; // closed → next plan opens a fresh topic
        }
        Ok(topic_id)
    }

    /// On restart, re-derive the open topic by scanning for one whose plan
    /// isn't all-done. Crash-safe — closure is a function of the durable file.
    pub fn recover(&mut self, store: &MemoryStore) {
        self.current = store
            .list_topics()
            .into_iter()
            .filter(|t| store.read_topic_plan(t).map(|p| !plan_closed(&p)).unwrap_or(false))
            .min(); // earliest open id, deterministic
    }
}

/// Next topic id = `topic-{NNN}` where NNN = existing-topic-count + 1. Monotonic
/// as long as topics aren't deleted (MVP doesn't delete).
fn mint_topic_id(store: &MemoryStore) -> String {
    format!("topic-{:03}", store.list_topics().len() + 1)
}

/// Closed = a non-empty plan whose every step is Done.
fn plan_closed(plan: &Plan) -> bool {
    !plan.steps.is_empty() && plan.steps.iter().all(|s| s.status == StepStatus::Done)
}

// ---------------------------------------------------------------------------
// content obfuscation (A-档, CLAUDE.md ④.b). SPEED BUMP, not encryption.
//
// `cat` of a memory file shows random-looking bytes after a magic header.
// A determined dev who decompiles the binary recovers the keystream — expected;
// this only stops a casual look from revealing the scheme. Swap obfuscate/
// deobfuscate for something stronger later without touching the store's callers.
// ---------------------------------------------------------------------------

const MEM_MAGIC: &[u8] = b"ADKM1\n"; // AiDock memory, format v1

/// Deterministic keystream (LCG). Embedded seed — not a secret, just obfuscation.
fn keystream_xor(bytes: &[u8]) -> Vec<u8> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    bytes
        .iter()
        .map(|b| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            b ^ ((state >> 33) as u8)
        })
        .collect()
}

fn obfuscate(plain: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(MEM_MAGIC.len() + plain.len());
    out.extend_from_slice(MEM_MAGIC);
    out.extend(keystream_xor(plain.as_bytes()));
    out
}

fn deobfuscate(raw: &[u8]) -> Option<String> {
    let body = raw.strip_prefix(MEM_MAGIC)?;
    String::from_utf8(keystream_xor(body)).ok()
}

// ---------------------------------------------------------------------------
// context bundle assembler (CLAUDE.md ④.b「本轮敲定」3 + 4)
//
// The OUTPUT half of ④.b: read the store → produce the slice ④.a's LlmSource
// injects at turn start. The agent sees: the FULL plan table (total scope) +
// the CURRENT step's full detail + DONE steps as one-line summaries + a recall
// pointer + a standing note that 📎 items have full detail. Pure, deterministic.
// Recall id = `"{topic_id}#{step_number}"` (1-based), parsed by `recall_detail`.
// ---------------------------------------------------------------------------

/// Assemble the context-memory section for `plan`'s topic. Empty string when
/// there's no plan yet (nothing to show).
pub fn assemble_bundle(store: &MemoryStore, topic_id: &str, plan: &Plan) -> String {
    if plan.steps.is_empty() {
        return String::new();
    }

    let mut s = String::from("## 当前任务记忆\n\n");
    s.push_str(&format!("# {}\n\n## 计划\n", plan.goal));
    for step in &plan.steps {
        s.push_str(&format!(
            "- {} {}. {}{}\n",
            checkbox(step.status),
            step.seq + 1,
            step.description,
            marker(step.status)
        ));
    }
    s.push('\n');

    // Current step (in-progress, else first pending) → full detail.
    let current = plan
        .steps
        .iter()
        .find(|s| s.status == StepStatus::InProgress)
        .or_else(|| plan.steps.iter().find(|s| s.status == StepStatus::Pending));
    if let Some(cur) = current {
        s.push_str(&format!("### 当前这一步（第{}步：{}）\n", cur.seq + 1, cur.description));
        match store.read_detail(topic_id, cur.seq) {
            Some(d) => {
                s.push_str(d.trim_end());
                s.push('\n');
            }
            None => s.push_str("（还没有详情）\n"),
        }
        s.push('\n');
    }

    // Done steps → one-line summary + recall pointer (collapsed, signposted).
    let done: Vec<&PlanStep> = plan.steps.iter().filter(|s| s.status == StepStatus::Done).collect();
    if !done.is_empty() {
        s.push_str("### 已完成步骤（小结，要看全文用 recall_detail）\n");
        for step in done {
            let summary = store
                .read_detail(topic_id, step.seq)
                .as_deref()
                .map(extract_summary)
                .unwrap_or_else(|| "（无详情）".to_string());
            s.push_str(&format!(
                "- 第{}步 {} — {}  📎 recall_detail(\"{}#{}\")\n",
                step.seq + 1,
                step.description,
                summary,
                topic_id,
                step.seq + 1
            ));
        }
        s.push('\n');
    }

    s.push_str("> 提示：带 📎 的步骤有完整详情，需要时调 recall_detail(id) 拉取。\n");
    s
}

fn checkbox(st: StepStatus) -> &'static str {
    match st {
        StepStatus::Done => "[x]",
        StepStatus::Failed => "[!]",
        StepStatus::Pending | StepStatus::InProgress => "[ ]",
    }
}

fn marker(st: StepStatus) -> &'static str {
    match st {
        StepStatus::InProgress => "  ← 进行中",
        StepStatus::Failed => "  ← 失败",
        _ => "",
    }
}

/// First `> summary` line of a detail (stripped), else its first non-empty
/// line clamped — the one-liner a collapsed done-step shows.
fn extract_summary(detail_md: &str) -> String {
    for line in detail_md.lines() {
        if let Some(rest) = line.trim().strip_prefix("> ") {
            return rest.trim().to_string();
        }
    }
    detail_md
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(60).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// recall_detail tool (CLAUDE.md ④.b: 主 agent 按需拉详情；`recall_topic` 的演进)
// ---------------------------------------------------------------------------

pub const TOOL_RECALL_DETAIL: &str = "recall_detail";

/// True iff `name` is the recall_detail tool (for executor routing).
pub fn is_recall_detail(name: &str) -> bool {
    name == TOOL_RECALL_DETAIL
}

/// The `recall_detail` tool schema, advertised to the model.
pub fn recall_detail_tool() -> Tool {
    Tool::function(
        TOOL_RECALL_DETAIL,
        "Fetch the FULL detail of a plan step you currently only see summarised. \
         Pass the id shown next to the step, e.g. \"topic-001#1\" (topic + step number). \
         Use it when the one-line summary isn't enough.",
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "e.g. \"topic-001#1\" (topic#step-number, 1-based)" }
            },
            "required": ["id"]
        }),
    )
}

#[derive(Deserialize)]
struct RecallArgs {
    id: String,
}

/// Run a `recall_detail` call against the store. Returns the detail body, or a
/// human-readable message (errors prefixed `ERROR:` so ④.c / the failure
/// backstop can see a malformed call, while a simple not-found is non-error).
pub fn execute_recall_detail(store: &MemoryStore, raw_args: &str) -> String {
    let args: RecallArgs = match serde_json::from_str(raw_args) {
        Ok(a) => a,
        Err(e) => return format!("ERROR: recall_detail bad args: {e}"),
    };
    let Some((topic, seq)) = parse_detail_id(&args.id) else {
        return format!(
            "ERROR: recall_detail id must look like \"topic-id#N\" (N = step number, 1-based); got {:?}",
            args.id
        );
    };
    match store.read_detail(&topic, seq) {
        Some(d) => d,
        None => format!("（没有找到 {topic} 第{}步的详情）", seq + 1),
    }
}

/// Parse `"{topic}#{n}"` (n 1-based) → `(topic, seq)` (seq 0-based). Splits on
/// the LAST `#` so topic ids containing `#` survive.
fn parse_detail_id(id: &str) -> Option<(String, usize)> {
    let (topic, num) = id.rsplit_once('#')?;
    let n: usize = num.trim().parse().ok()?;
    if n == 0 || topic.is_empty() {
        return None;
    }
    Some((topic.to_string(), n - 1))
}

// ---------------------------------------------------------------------------
// tests — pure, temp-dir, no LLM.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::react::Plan;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn temp_root() -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("aidock-mem-{}-{n}", std::process::id()))
    }

    fn login_plan() -> Plan {
        let mut p = Plan::new("做登录功能", ["写技术文档", "实现 login 接口", "写 JWT 验证"]);
        p.steps[0].status = StepStatus::Done;
        p.steps[1].status = StepStatus::InProgress;
        p
    }

    #[test]
    fn topic_plan_round_trips() {
        let store = MemoryStore::new(temp_root());
        store.write_topic("topic-001", &login_plan(), 1000).unwrap();

        let got = store.read_topic_plan("topic-001").expect("topic exists");
        assert_eq!(got, login_plan()); // goal + step statuses preserved
    }

    #[test]
    fn raw_topic_file_is_obfuscated_on_disk() {
        let root = temp_root();
        let store = MemoryStore::new(&root);
        store.write_topic("topic-001", &login_plan(), 1000).unwrap();

        let raw = fs::read(root.join("topic-001/_topic")).unwrap();
        assert!(raw.starts_with(MEM_MAGIC));
        let as_str = String::from_utf8_lossy(&raw);
        assert!(!as_str.contains("做登录功能")); // goal not readable on disk
        assert!(!as_str.contains("goal")); // not even the JSON keys
    }

    #[test]
    fn obfuscate_round_trips() {
        let plain = "{\"goal\":\"做登录功能\"}";
        let blob = obfuscate(plain);
        assert_ne!(&blob[MEM_MAGIC.len()..], plain.as_bytes());
        assert_eq!(deobfuscate(&blob).as_deref(), Some(plain));
        assert_eq!(deobfuscate(b"not ours"), None);
    }

    #[test]
    fn write_then_read_detail_round_trips() {
        let store = MemoryStore::new(temp_root());
        store
            .write_detail("topic-001", 0, "定了用 JWT + bcrypt", "## 决策\n- 鉴权用 JWT")
            .unwrap();

        let detail = store.read_detail("topic-001", 0).expect("detail exists");
        assert!(detail.contains("> 定了用 JWT + bcrypt"));
        assert!(detail.contains("鉴权用 JWT"));
    }

    #[test]
    fn missing_topic_and_detail_return_none() {
        let store = MemoryStore::new(temp_root());
        assert!(store.read_topic_plan("nope").is_none());
        assert!(store.read_detail("nope", 0).is_none());
    }

    // --- context bundle assembler ----------------------------------------

    #[test]
    fn bundle_shows_plan_current_detail_and_done_pointer() {
        let store = MemoryStore::new(temp_root());
        let tid = "topic-001";
        store.write_topic(tid, &login_plan(), 1000).unwrap();
        store.write_detail(tid, 0, "定了用 JWT + bcrypt", "## 决策\n- JWT").unwrap();
        store.write_detail(tid, 1, "写到一半", "POST /api/login 草稿…").unwrap();

        let bundle = assemble_bundle(&store, tid, &login_plan());

        assert_eq!(bundle.matches("- [").count(), 3); // full scope
        assert!(bundle.contains("当前这一步（第2步：实现 login 接口）"));
        assert!(bundle.contains("POST /api/login 草稿")); // current detail inlined
        assert!(bundle.contains("定了用 JWT + bcrypt")); // done summary
        assert!(bundle.contains(r#"recall_detail("topic-001#1")"#)); // pointer + id
        assert!(bundle.contains("带 📎 的步骤有完整详情")); // standing note
        assert!(!bundle.contains("## 决策")); // done body NOT inlined
    }

    #[test]
    fn bundle_handles_current_step_without_detail() {
        let store = MemoryStore::new(temp_root());
        let tid = "topic-001";
        store.write_topic(tid, &login_plan(), 1).unwrap();
        let bundle = assemble_bundle(&store, tid, &login_plan());
        assert!(bundle.contains("（还没有详情）"));
        assert!(bundle.contains("（无详情）"));
    }

    #[test]
    fn bundle_empty_when_no_plan() {
        let store = MemoryStore::new(temp_root());
        let empty = Plan::new("g", Vec::<String>::new());
        assert!(assemble_bundle(&store, "t", &empty).is_empty());
    }

    #[test]
    fn extract_summary_prefers_quote_line() {
        assert_eq!(extract_summary("> the gist\n\nbody"), "the gist");
        assert_eq!(extract_summary("no quote\nsecond"), "no quote");
        assert_eq!(extract_summary(""), "");
    }

    // --- recall_detail tool ----------------------------------------------

    #[test]
    fn parse_detail_id_cases() {
        assert_eq!(parse_detail_id("topic-001#1"), Some(("topic-001".into(), 0)));
        assert_eq!(parse_detail_id("a#b#2"), Some(("a#b".into(), 1)));
        assert_eq!(parse_detail_id("topic#0"), None);
        assert_eq!(parse_detail_id("nohash"), None);
        assert_eq!(parse_detail_id("#3"), None);
    }

    #[test]
    fn recall_detail_serves_the_body() {
        let store = MemoryStore::new(temp_root());
        store.write_detail("topic-001", 0, "用 JWT", "## 决策\n- 用 JWT\n- bcrypt").unwrap();

        let out = execute_recall_detail(&store, r#"{"id":"topic-001#1"}"#);
        assert!(out.contains("用 JWT"));
        assert!(out.contains("bcrypt"));
        assert!(!out.starts_with("ERROR"));
    }

    #[test]
    fn recall_detail_reports_bad_id_and_missing() {
        let store = MemoryStore::new(temp_root());
        assert!(execute_recall_detail(&store, r#"{"id":"nope"}"#).starts_with("ERROR:"));
        assert!(execute_recall_detail(&store, r#"{}"#).starts_with("ERROR:"));
        let nf = execute_recall_detail(&store, r#"{"id":"topic-x#2"}"#);
        assert!(!nf.starts_with("ERROR"));
        assert!(nf.contains("没有找到"));
    }

    #[test]
    fn recall_detail_tool_advertises_name() {
        assert_eq!(recall_detail_tool().function.name, TOOL_RECALL_DETAIL);
        assert!(is_recall_detail("recall_detail"));
        assert!(!is_recall_detail("recall_topic"));
    }

    // --- TopicTracker (回合 → 大主题映射) -------------------------------

    #[test]
    fn tracker_mints_updates_then_closes() {
        let store = MemoryStore::new(temp_root());
        let mut tk = TopicTracker::default();

        let mut plan = Plan::new("做登录功能", ["a", "b"]);
        plan.steps[0].status = StepStatus::InProgress;
        assert_eq!(tk.record(&store, &plan, 1).unwrap(), "topic-001");
        assert_eq!(tk.current(), Some("topic-001"));

        plan.steps[0].status = StepStatus::Done;
        plan.steps[1].status = StepStatus::InProgress;
        assert_eq!(tk.record(&store, &plan, 2).unwrap(), "topic-001"); // updates in place
        assert_eq!(tk.current(), Some("topic-001"));

        plan.steps[1].status = StepStatus::Done;
        tk.record(&store, &plan, 3).unwrap();
        assert_eq!(tk.current(), None); // all done → closed
        assert_eq!(store.read_topic_plan("topic-001").unwrap().steps[1].status, StepStatus::Done);
    }

    #[test]
    fn tracker_opens_fresh_topic_after_close() {
        let store = MemoryStore::new(temp_root());
        let mut tk = TopicTracker::default();

        let mut done = Plan::new("旧任务", ["x"]);
        done.steps[0].status = StepStatus::Done;
        assert_eq!(tk.record(&store, &done, 1).unwrap(), "topic-001");
        assert_eq!(tk.current(), None);

        let mut fresh = Plan::new("新任务", ["y"]);
        fresh.steps[0].status = StepStatus::InProgress;
        assert_eq!(tk.record(&store, &fresh, 2).unwrap(), "topic-002");
        assert_eq!(tk.current(), Some("topic-002"));
    }

    #[test]
    fn tracker_recover_finds_the_open_topic() {
        let store = MemoryStore::new(temp_root());
        let mut done = Plan::new("done one", ["x"]);
        done.steps[0].status = StepStatus::Done;
        store.write_topic("topic-001", &done, 1).unwrap();
        let mut open = Plan::new("open one", ["y"]);
        open.steps[0].status = StepStatus::InProgress;
        store.write_topic("topic-002", &open, 1).unwrap();

        let mut tk = TopicTracker::default();
        tk.recover(&store);
        assert_eq!(tk.current(), Some("topic-002"));
    }
}
