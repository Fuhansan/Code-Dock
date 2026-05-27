//! Disk persistence helpers for session state.
//!
//! Three primitives, all the rest of `agent::` builds on these:
//!
//!   - `atomic_write_json` — write JSON to a tmp file, fsync, rename. On
//!     Unix the final file gets mode 0600 (private to the owning user).
//!   - `read_json` — read + deserialize, returning `None` for "file
//!     doesn't exist" and propagating real I/O / parse failures.
//!   - `read_jsonl_messages` — read a `messages.jsonl` line by line,
//!     parse each as `AgentMessage`, **skip-with-warning** on malformed
//!     lines (the tail-end of the file is exactly where a mid-write crash
//!     would corrupt a line, and we never want startup to refuse to load).
//!
//! Atomicity model: write tmp → fsync tmp → rename → done. The rename is
//! atomic on the same filesystem, so readers either see the old file or
//! the fully-written new one. No half-written states are visible.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Serialize};

use crate::agent::message::AgentMessage;

#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    #[error("io: {context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("serialize/deserialize: {0}")]
    Codec(#[from] serde_json::Error),
}

/// Serialize `value` and replace `path` atomically. Creates parent dirs.
/// On Unix the resulting file is chmod 0600 (owner read/write only).
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), PersistError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| PersistError::Io {
            context: "create parent dir",
            source,
        })?;
    }
    let json = serde_json::to_string_pretty(value)?;
    // Tmp path next to the target so the rename stays within one filesystem.
    let mut tmp: PathBuf = path.to_path_buf();
    tmp.set_extension(match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => format!("{ext}.tmp"),
        None => "tmp".to_string(),
    });

    {
        let mut f = fs::File::create(&tmp).map_err(|source| PersistError::Io {
            context: "create tmp file",
            source,
        })?;
        f.write_all(json.as_bytes())
            .map_err(|source| PersistError::Io {
                context: "write tmp",
                source,
            })?;
        f.sync_all().map_err(|source| PersistError::Io {
            context: "fsync tmp",
            source,
        })?;
    }

    fs::rename(&tmp, path).map_err(|source| PersistError::Io {
        context: "atomic rename",
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

/// Read + deserialize JSON. `Ok(None)` iff the file doesn't exist; any
/// other I/O or parse failure is `Err`.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, PersistError> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|source| PersistError::Io {
        context: "read",
        source,
    })?;
    let value = serde_json::from_str(&raw)?;
    Ok(Some(value))
}

/// Read every successfully-parseable line of `messages.jsonl`. Malformed
/// lines (typically the tail line of a crashed write) are skipped with a
/// `tracing::warn` — we never refuse to start because of bad bytes at
/// the end of the file.
///
/// Returns the messages in file order, oldest first.
pub fn read_jsonl_messages(path: &Path) -> Result<Vec<AgentMessage>, PersistError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path).map_err(|source| PersistError::Io {
        context: "read jsonl",
        source,
    })?;
    let mut out = Vec::new();
    let mut bad = 0;
    for (idx, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<AgentMessage>(line) {
            Ok(m) => out.push(m),
            Err(e) => {
                bad += 1;
                tracing::warn!(
                    target: "aidock::persist",
                    line = idx + 1,
                    error = %e,
                    "skipping malformed JSONL line"
                );
            }
        }
    }
    if bad > 0 {
        tracing::warn!(
            target: "aidock::persist",
            bad,
            kept = out.len(),
            "messages.jsonl had malformed lines"
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::message::AgentMessageKind;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Each test gets its own dir to avoid cross-contamination across
    /// parallel runs. Cleaned up at end-of-process implicitly (tmp dir),
    /// and explicitly when the test passes.
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    fn test_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aidock-persist-{}-{}-{}", label, std::process::id(), n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Sample {
        name: String,
        count: u32,
    }

    #[test]
    fn atomic_write_then_read_round_trips() {
        let dir = test_dir("rw");
        let path = dir.join("sample.json");
        let v = Sample {
            name: "x".into(),
            count: 3,
        };
        atomic_write_json(&path, &v).unwrap();
        let back: Sample = read_json(&path).unwrap().unwrap();
        assert_eq!(back, v);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_missing_returns_none() {
        let dir = test_dir("missing");
        let path = dir.join("does-not-exist.json");
        let v: Option<Sample> = read_json(&path).unwrap();
        assert!(v.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_corrupt_errors() {
        let dir = test_dir("corrupt");
        let path = dir.join("bad.json");
        fs::write(&path, b"{ not json").unwrap();
        let r: Result<Option<Sample>, _> = read_json(&path);
        assert!(matches!(r, Err(PersistError::Codec(_))));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_tmp_left_behind_after_successful_write() {
        let dir = test_dir("notmp");
        let path = dir.join("ok.json");
        atomic_write_json(
            &path,
            &Sample {
                name: "y".into(),
                count: 7,
            },
        )
        .unwrap();
        let leftover = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| {
                e.file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            });
        assert!(leftover.is_none(), "tmp file leaked: {leftover:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unix_mode_is_0600() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = test_dir("perms");
            let path = dir.join("secret.json");
            atomic_write_json(
                &path,
                &Sample {
                    name: "z".into(),
                    count: 1,
                },
            )
            .unwrap();
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn read_jsonl_missing_returns_empty() {
        let dir = test_dir("jsonl-missing");
        let path = dir.join("messages.jsonl");
        let msgs = read_jsonl_messages(&path).unwrap();
        assert!(msgs.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_jsonl_parses_good_lines_and_skips_bad() {
        let dir = test_dir("jsonl-mix");
        let path = dir.join("messages.jsonl");
        let m1 = AgentMessage::new(
            "m-1",
            "PM",
            "t-1",
            1,
            AgentMessageKind::Broadcast { content: "hi".into() },
        );
        let m2 = AgentMessage::new(
            "m-2",
            "PM",
            "t-1",
            2,
            AgentMessageKind::Broadcast { content: "again".into() },
        );
        let good1 = serde_json::to_string(&m1).unwrap();
        let good2 = serde_json::to_string(&m2).unwrap();
        // Mid-file noise + good lines + truncated tail.
        let mut content = String::new();
        content.push_str(&good1);
        content.push('\n');
        content.push_str("{ not valid json"); // simulated mid-file corruption
        content.push('\n');
        content.push_str(&good2);
        content.push('\n');
        // Empty trailing line (idle whitespace) — must be tolerated silently.
        content.push('\n');
        fs::write(&path, content.as_bytes()).unwrap();
        let msgs = read_jsonl_messages(&path).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, "m-1");
        assert_eq!(msgs[1].id, "m-2");
        let _ = fs::remove_dir_all(&dir);
    }
}
