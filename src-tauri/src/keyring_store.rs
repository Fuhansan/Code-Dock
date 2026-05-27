//! Per-provider API key storage.
//!
//! **V0.1 dev:** writes to `~/.aidock/api_keys.json` with file mode 0600.
//! Originally this was supposed to use the OS keyring (AIDOCK_DESIGN.md §8.5),
//! but macOS Keychain silently refuses writes from unsigned Cargo-built dev
//! binaries — `set_password` returns `Ok` while nothing actually persists,
//! making BYOK setup fail mysteriously.
//!
//! **TODO(post-V0.1):** once code signing is in place (Sprint 5 packaging),
//! switch back to the `keyring` crate. The public interface here is
//! deliberately keyring-shaped so the migration is a single-file change.
//!
//! The data dir lives inside `$HOME`, which is the macOS / Linux security
//! boundary for per-user secrets. File mode 0600 keeps other local users out.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const KEYS_FILE_NAME: &str = "api_keys.json";

#[derive(Debug, thiserror::Error)]
pub enum KeyringError {
    #[error("home directory not available: {0}")]
    NoHome(String),
    #[error("io: {context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("parse: {0}")]
    Parse(String),
}

impl serde::Serialize for KeyringError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

fn aidock_data_dir() -> Result<PathBuf, KeyringError> {
    let home = std::env::var("HOME").map_err(|e| KeyringError::NoHome(e.to_string()))?;
    let dir = PathBuf::from(home).join(".aidock");
    fs::create_dir_all(&dir).map_err(|source| KeyringError::Io {
        context: "create .aidock dir",
        source,
    })?;
    Ok(dir)
}

fn keys_file_path() -> Result<PathBuf, KeyringError> {
    Ok(aidock_data_dir()?.join(KEYS_FILE_NAME))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ApiKeysFile {
    #[serde(default)]
    providers: HashMap<String, String>,
}

fn read_keys() -> Result<ApiKeysFile, KeyringError> {
    let path = keys_file_path()?;
    if !path.exists() {
        return Ok(ApiKeysFile::default());
    }
    let raw = fs::read_to_string(&path).map_err(|source| KeyringError::Io {
        context: "read api_keys.json",
        source,
    })?;
    serde_json::from_str(&raw).map_err(|e| KeyringError::Parse(e.to_string()))
}

fn write_keys(data: &ApiKeysFile) -> Result<(), KeyringError> {
    let path = keys_file_path()?;
    let tmp = path.with_extension("json.tmp");

    let json =
        serde_json::to_string_pretty(data).map_err(|e| KeyringError::Parse(e.to_string()))?;

    {
        let mut f = fs::File::create(&tmp).map_err(|source| KeyringError::Io {
            context: "create tmp",
            source,
        })?;
        f.write_all(json.as_bytes())
            .map_err(|source| KeyringError::Io {
                context: "write tmp",
                source,
            })?;
        f.sync_all().map_err(|source| KeyringError::Io {
            context: "fsync tmp",
            source,
        })?;
    }

    fs::rename(&tmp, &path).map_err(|source| KeyringError::Io {
        context: "atomic rename",
        source,
    })?;

    // Best-effort: tighten file mode on Unix so only the user can read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

/// Persist `key` for `provider`. Overwrites any existing key for that provider.
pub fn save_api_key(provider: &str, key: &str) -> Result<(), KeyringError> {
    let mut data = read_keys()?;
    data.providers.insert(provider.to_string(), key.to_string());
    write_keys(&data)?;
    tracing::info!(target: "aidock::keyring", provider, "API key persisted to file");
    Ok(())
}

/// Return the stored key for `provider`, or `None` if none has ever been set.
pub fn load_api_key(provider: &str) -> Result<Option<String>, KeyringError> {
    let data = read_keys()?;
    Ok(data.providers.get(provider).cloned())
}

/// `true` iff a key exists for `provider`. Used by the BYOK setup gate.
pub fn has_api_key(provider: &str) -> bool {
    matches!(load_api_key(provider), Ok(Some(s)) if !s.is_empty())
}

/// Remove the stored key. No-op if nothing was stored.
#[allow(dead_code)] // exposed for future settings UI / key rotation
pub fn delete_api_key(provider: &str) -> Result<(), KeyringError> {
    let mut data = read_keys()?;
    data.providers.remove(provider);
    write_keys(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_save_load_delete() {
        let provider = "test-provider-roundtrip";
        let _ = delete_api_key(provider);

        save_api_key(provider, "sk-test-value-xyz").unwrap();
        assert!(has_api_key(provider));
        assert_eq!(
            load_api_key(provider).unwrap().as_deref(),
            Some("sk-test-value-xyz")
        );

        delete_api_key(provider).unwrap();
        assert!(!has_api_key(provider));
    }
}
