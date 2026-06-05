//! ③ 会话存储布局 + 生命周期辅助。
//!
//! 三层容纳(路径管归属,切上层零碰撞):
//! ```text
//! ~/.aidock/users/{user}/workshops/{workshop}/sessions/{session}/
//!     messages.jsonl · scratchpads/ · tool_calls.jsonl · agents/{id}/memory/ · workspace/
//! ```
//! V0.1 默认 `user=default`、`workshop=ws-default`。
//! TODO(低优,无账号系统暂搁):有真登录后 `user` 用账号 id（见 `DEFAULT_USER`）。
//!
//! 会话 id = `{unix_millis}_{uuid8}`：可排序(时间前缀)、文件系统/URL 安全(数字+hex+下划线)。
//! 人看的名字由 [`SessionMeta::title`]（首条用户消息派生）承担,不靠 id 可读。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// 未登录时的用户 id。TODO：有账号系统后换成真实账号 id。
pub const DEFAULT_USER: &str = "default";
/// V0.1 单一硬编码工作室的 id（`roles::default_workshop`）。多工作室编辑器落地后由用户建。
pub const DEFAULT_WORKSHOP: &str = "ws-default";

/// `~/.aidock`（HOME 缺失时退回当前目录下的 `.aidock`）。
pub fn aidock_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".aidock")
}

/// `.../users/{user}/workshops/{workshop}` — the workshop's own directory
/// (holds `workshop.json` + the `sessions/` subtree).
pub fn workshop_dir(user: &str, workshop: &str) -> PathBuf {
    aidock_root()
        .join("users")
        .join(user)
        .join("workshops")
        .join(workshop)
}

/// `.../users/{user}/workshops/{workshop}/sessions`
pub fn sessions_dir(user: &str, workshop: &str) -> PathBuf {
    workshop_dir(user, workshop).join("sessions")
}

/// 一个具体会话的目录。
pub fn session_dir(user: &str, workshop: &str, session_id: &str) -> PathBuf {
    sessions_dir(user, workshop).join(session_id)
}

/// 生成一个新会话 id：`{unix_millis}_{uuid8}`。
pub fn new_session_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let rand = uuid::Uuid::new_v4().simple().to_string();
    format!("{millis}_{}", &rand[..8])
}

/// 列表里给前端看的会话摘要（读时从目录派生，不写 meta 文件）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionMeta {
    pub id: String,
    /// 首条用户消息截取；无消息则"新会话"。
    pub title: String,
    /// 从 id 的时间前缀解析（会话创建时刻，unix millis）。
    pub created_ms: i64,
    /// `messages.jsonl` 的最后修改时刻（unix millis）；无则同 created。
    pub last_active_ms: i64,
    /// 消息条数（`messages.jsonl` 行数）。
    pub message_count: usize,
}

/// 标题截断长度（字符）。
const TITLE_MAX: usize = 40;

/// 扫描某工作室下所有会话，按最后活跃倒序。纯读时派生：标题=首条用户消息、
/// 活跃=messages.jsonl mtime、条数=行数、创建=id 时间前缀。
pub fn list_sessions(user: &str, workshop: &str) -> Vec<SessionMeta> {
    let dir = sessions_dir(user, workshop);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new(); // 还没有任何会话
    };
    let mut out: Vec<SessionMeta> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let id = e.file_name().to_str()?.to_string();
            Some(meta_for(&e.path(), id))
        })
        .collect();
    // 按**创建时间**倒序（新建的在上）。用 created（id 时间前缀，固定不变）而非
    // last_active 排序，这样打开/切换会话**永不重排**——避免"开一个、别的下沉"。
    out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
    out
}

fn meta_for(dir: &Path, id: String) -> SessionMeta {
    let created_ms = created_from_id(&id);
    let msgs = dir.join("messages.jsonl");
    let (derived_title, message_count) = title_and_count(&msgs);
    // LLM 生成的标题(meta.json)优先;否则回落"截取首条用户消息"。
    let title = read_title(dir).unwrap_or(derived_title);
    let last_active_ms = mtime_ms(&msgs).unwrap_or(created_ms);
    SessionMeta {
        id,
        title,
        created_ms,
        last_active_ms,
        message_count,
    }
}

/// 持久化的会话标题(LLM 按意图生成)存在 `{dir}/meta.json` 的 `title` 字段。
/// 有则优先于"截首条消息"的回落标题。
pub fn read_title(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join("meta.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let t = v.get("title")?.as_str()?.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// 写入(覆盖)会话标题到 `meta.json`。
pub fn write_title(dir: &Path, title: &str) -> std::io::Result<()> {
    let body = serde_json::json!({ "title": title }).to_string();
    std::fs::write(dir.join("meta.json"), body)
}

/// True iff 该会话还没有任何消息(下一条用户消息即首条——用于"首条触发起标题")。
pub fn is_empty_session(dir: &Path) -> bool {
    match std::fs::read_to_string(dir.join("messages.jsonl")) {
        Ok(c) => c.lines().all(|l| l.trim().is_empty()),
        Err(_) => true,
    }
}

/// id 形如 `{millis}_{rand}`；取前缀当创建时刻。解析失败给 0。
fn created_from_id(id: &str) -> i64 {
    id.split('_')
        .next()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
}

fn mtime_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let d = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(d.as_millis() as i64)
}

/// 读 messages.jsonl：标题 = 第一条用户消息内容（截断），条数 = 行数。
fn title_and_count(messages_jsonl: &Path) -> (String, usize) {
    let Ok(content) = std::fs::read_to_string(messages_jsonl) else {
        return ("新会话".to_string(), 0);
    };
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    let count = lines.len();
    let title = lines
        .iter()
        .find_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            if v.get("sender").and_then(|s| s.as_str()) == Some("user") {
                // content lives under kind (USER_INPUT { content }), with a top-level fallback.
                let c = v
                    .pointer("/kind/content")
                    .or_else(|| v.get("content"))
                    .and_then(|c| c.as_str())?;
                Some(truncate(c, TITLE_MAX))
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            if count == 0 {
                "新会话".to_string()
            } else {
                "(无标题)".to_string()
            }
        });
    (title, count)
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn tmp_sessions() -> (PathBuf, String, String) {
        // Build an isolated sessions dir by overriding the user/workshop names.
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let user = format!("test-user-{}-{n}", std::process::id());
        let ws = "ws-test";
        let dir = sessions_dir(&user, ws);
        std::fs::create_dir_all(&dir).unwrap();
        (dir, user, ws.to_string())
    }

    #[test]
    fn id_is_sortable_and_safe() {
        let a = new_session_id();
        let b = new_session_id();
        // millis prefix parseable; chars are filesystem-safe.
        assert!(created_from_id(&a) > 0);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        assert_ne!(a, b);
    }

    #[test]
    fn list_empty_when_none() {
        let (_dir, user, ws) = tmp_sessions();
        assert!(list_sessions(&user, &ws).is_empty());
    }

    #[test]
    fn list_shows_all_and_sorts_by_created() {
        let (dir, user, ws) = tmp_sessions();
        let write_msg = |id: &str, content: &str| {
            std::fs::create_dir_all(dir.join(id)).unwrap();
            std::fs::write(
                dir.join(id).join("messages.jsonl"),
                format!(
                    "{{\"id\":\"m1\",\"sender\":\"user\",\"timestamp\":1,\"kind\":{{\"type\":\"USER_INPUT\",\"content\":\"{content}\"}}}}\n"
                ),
            )
            .unwrap();
        };
        write_msg("1000_aaaaaaaa", "做一个登录页");
        write_msg("2000_bbbbbbbb", "todo 应用");
        // 空会话(无消息)也会被列出——惰性创建/新建后要立刻可见。
        std::fs::create_dir_all(dir.join("3000_dddddddd")).unwrap();

        let list = list_sessions(&user, &ws);
        assert_eq!(list.len(), 3);
        // created 倒序：3000、2000、1000。
        assert_eq!(list[0].id, "3000_dddddddd");
        assert_eq!(list[1].id, "2000_bbbbbbbb");
        assert_eq!(list[2].id, "1000_aaaaaaaa");
        let m_empty = list.iter().find(|m| m.id == "3000_dddddddd").unwrap();
        assert_eq!(m_empty.title, "新会话");
        assert_eq!(m_empty.message_count, 0);
        let m1 = list.iter().find(|m| m.id == "1000_aaaaaaaa").unwrap();
        assert_eq!(m1.title, "做一个登录页");
        assert_eq!(m1.message_count, 1);
    }

    #[test]
    fn title_truncates_long_input() {
        let long = "x".repeat(100);
        let t = truncate(&long, TITLE_MAX);
        assert_eq!(t.chars().count(), TITLE_MAX + 1); // +1 for the ellipsis
        assert!(t.ends_with('…'));
    }

    #[test]
    fn meta_title_overrides_derived() {
        let (dir, user, ws) = tmp_sessions();
        let s = "3000_cccccccc".to_string();
        let sd = dir.join(&s);
        std::fs::create_dir_all(&sd).unwrap();
        std::fs::write(
            sd.join("messages.jsonl"),
            "{\"sender\":\"user\",\"timestamp\":1,\"kind\":{\"type\":\"USER_INPUT\",\"content\":\"原始首条消息\"}}\n",
        )
        .unwrap();
        let title_of = |id: &str| {
            list_sessions(&user, &ws)
                .into_iter()
                .find(|m| m.id == id)
                .unwrap()
                .title
        };
        // No meta yet → derived from first message.
        assert_eq!(title_of(&s), "原始首条消息");
        assert!(!is_empty_session(&sd));
        // LLM-written meta title overrides.
        write_title(&sd, "登录页开发").unwrap();
        assert_eq!(read_title(&sd).as_deref(), Some("登录页开发"));
        assert_eq!(title_of(&s), "登录页开发");
    }
}
