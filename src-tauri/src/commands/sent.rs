//! Per-identity persistent sent log.
//!
//! Mirrors [`crate::commands::inbox`]: one sealed record per line in
//! `<identity-dir>/sent.jsonl`, encrypted under the identity's at-rest
//! storage key.
//!
//! This used to live in the frontend's `localStorage`, marked proto-only.
//! That put outgoing message bodies outside the identity directory
//! entirely, in the clear — so encrypting the database and the inbox would
//! have left sent messages as the one readable copy of a conversation.
//!
//! Retention is a sweep on load rather than a background job: rows older
//! than the caller's TTL are dropped and the survivors rewritten, so the
//! file self-trims whenever the log is read.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use dnsmesh_core::crypto::STORAGE_KEY_LEN;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::sync::OnceCell;
use zeroize::Zeroizing;

use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

/// File name for the per-identity sent log.
const SENT_FILE: &str = "sent.jsonl";

/// One outgoing message as persisted. Shape matches the frontend's
/// `SentRow` so the store can round-trip it unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentRow {
    pub msg_id_hex: String,
    pub recipient_username: String,
    /// Unix seconds.
    pub timestamp: u64,
    pub plaintext_utf8: String,
}

/// Per-identity I/O locks, keyed by sanitised username.
static SENT_LOCKS: OnceCell<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceCell::const_new();

async fn lock_for(username: &str) -> Arc<Mutex<()>> {
    let map = SENT_LOCKS
        .get_or_init(|| async { Mutex::new(HashMap::new()) })
        .await;
    let mut guard = map.lock();
    guard
        .entry(username.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn sent_path(state: &AppState, username: &str) -> PathBuf {
    state.identity_dir(username).join(SENT_FILE)
}

async fn active_identity(
    state: &State<'_, AppState>,
) -> Option<(String, Zeroizing<[u8; STORAGE_KEY_LEN]>)> {
    let guard = state.active.read().await;
    guard
        .as_ref()
        .map(|a| (a.username.clone(), a.client.storage_key()))
}

fn load_rows(path: &std::path::Path, key: &[u8]) -> Result<Vec<SentRow>, CommandError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path).map_err(CommandError::from)?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Same tolerance as the inbox: skip whatever doesn't open rather
        // than failing the load.
        let Some(plain) = crate::atrest::open(key, line) else {
            continue;
        };
        if let Ok(row) = serde_json::from_slice::<SentRow>(&plain) {
            out.push(row);
        }
    }
    Ok(out)
}

fn write_rows(path: &std::path::Path, key: &[u8], rows: &[SentRow]) -> Result<(), CommandError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(CommandError::from)?;
    }
    let mut out = String::new();
    for row in rows {
        let json = serde_json::to_vec(row).map_err(|e| {
            CommandError::new("internal", format!("serialising sent row failed: {e}"))
        })?;
        let sealed = crate::atrest::seal(key, &json)
            .map_err(|e| CommandError::new("internal", format!("sealing sent row: {e}")))?;
        out.push_str(&sealed);
        out.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, out.as_bytes()).map_err(CommandError::from)?;
    std::fs::rename(&tmp, path).map_err(CommandError::from)?;
    Ok(())
}

/// Drop rows older than `ttl_hours`. `None` disables the sweep.
fn sweep(rows: Vec<SentRow>, ttl_hours: Option<u32>, now: u64) -> Vec<SentRow> {
    let Some(ttl) = ttl_hours else {
        return rows;
    };
    let cutoff = now.saturating_sub(u64::from(ttl) * 3600);
    rows.into_iter().filter(|r| r.timestamp >= cutoff).collect()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Args for [`sent_load`].
#[derive(Debug, Clone, Deserialize)]
pub struct SentLoadArgs {
    /// Retention window in hours. `None` keeps everything.
    #[serde(default)]
    pub ttl_hours: Option<u32>,
}

/// Load the active identity's sent log, sweeping expired rows and
/// rewriting the file when any were dropped.
#[tauri::command]
pub async fn sent_load(
    args: SentLoadArgs,
    state: State<'_, AppState>,
) -> CommandResult<Vec<SentRow>> {
    let Some((username, key)) = active_identity(&state).await else {
        return Ok(Vec::new());
    };
    let lock = lock_for(&username).await;
    let _g = lock.lock();
    let path = sent_path(&state, &username);
    let all = load_rows(&path, key.as_ref())?;
    let before = all.len();
    let kept = sweep(all, args.ttl_hours, now_secs());
    if kept.len() != before {
        write_rows(&path, key.as_ref(), &kept)?;
    }
    Ok(kept)
}

/// Args for [`sent_append`].
#[derive(Debug, Clone, Deserialize)]
pub struct SentAppendArgs {
    pub row: SentRow,
}

/// Append one outgoing message. Deduped by `msg_id_hex`.
#[tauri::command]
pub async fn sent_append(
    args: SentAppendArgs,
    state: State<'_, AppState>,
) -> CommandResult<Vec<SentRow>> {
    let Some((username, key)) = active_identity(&state).await else {
        return Ok(Vec::new());
    };
    let lock = lock_for(&username).await;
    let _g = lock.lock();
    let path = sent_path(&state, &username);
    let mut rows = load_rows(&path, key.as_ref())?;
    if !rows.iter().any(|r| r.msg_id_hex == args.row.msg_id_hex) {
        rows.push(args.row);
        write_rows(&path, key.as_ref(), &rows)?;
    }
    Ok(rows)
}

/// Args for [`sent_remove_by_recipient`].
#[derive(Debug, Clone, Deserialize)]
pub struct SentRemoveArgs {
    pub recipient_username: String,
}

/// Drop every row addressed to `recipient_username`, case-insensitively.
/// Backs "Clear chat", which wipes one thread without touching others.
#[tauri::command]
pub async fn sent_remove_by_recipient(
    args: SentRemoveArgs,
    state: State<'_, AppState>,
) -> CommandResult<Vec<SentRow>> {
    let Some((username, key)) = active_identity(&state).await else {
        return Ok(Vec::new());
    };
    let lock = lock_for(&username).await;
    let _g = lock.lock();
    let path = sent_path(&state, &username);
    let rows = load_rows(&path, key.as_ref())?;
    let target = args.recipient_username.to_lowercase();
    let kept: Vec<SentRow> = rows
        .into_iter()
        .filter(|r| r.recipient_username.to_lowercase() != target)
        .collect();
    write_rows(&path, key.as_ref(), &kept)?;
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const TEST_KEY: [u8; STORAGE_KEY_LEN] = [0x2a; STORAGE_KEY_LEN];

    fn row(id: u8, recipient: &str, ts: u64) -> SentRow {
        SentRow {
            msg_id_hex: format!("{id:032x}"),
            recipient_username: recipient.to_string(),
            timestamp: ts,
            plaintext_utf8: format!("message {id}"),
        }
    }

    #[test]
    fn round_trips_through_a_sealed_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SENT_FILE);
        let rows = vec![row(1, "bob", 100), row(2, "carol", 200)];
        write_rows(&path, &TEST_KEY, &rows).unwrap();
        let loaded = load_rows(&path, &TEST_KEY).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1].recipient_username, "carol");
    }

    /// The reason this moved out of localStorage.
    #[test]
    fn file_does_not_contain_plaintext_bodies() {
        // Long canaries: a 3-byte one like "bob" can appear by chance in
        // random base64, which would make this pass or fail for reasons
        // unrelated to encryption.
        const RECIPIENT: &str = "RECIPIENT-CANARY-carol";
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SENT_FILE);
        write_rows(&path, &TEST_KEY, &[row(1, RECIPIENT, 100)]).unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw.windows(9).any(|w| w == b"message 1"));
        assert!(
            !raw.windows(RECIPIENT.len())
                .any(|w| w == RECIPIENT.as_bytes())
        );
    }

    #[test]
    fn wrong_key_yields_nothing_rather_than_erroring() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(SENT_FILE);
        write_rows(&path, &TEST_KEY, &[row(1, "bob", 100)]).unwrap();
        assert!(
            load_rows(&path, &[0x99; STORAGE_KEY_LEN])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn sweep_drops_only_expired_rows() {
        let now = 1_000_000u64;
        let rows = vec![
            row(1, "bob", now - 3600),      // 1h old
            row(2, "bob", now - 25 * 3600), // 25h old
        ];
        let kept = sweep(rows.clone(), Some(24), now);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].msg_id_hex, row(1, "bob", 0).msg_id_hex);
        // None disables the sweep entirely.
        assert_eq!(sweep(rows, None, now).len(), 2);
    }

    #[test]
    fn sweep_handles_timestamps_beyond_the_window_without_underflow() {
        // now < ttl seconds: saturating_sub must not wrap.
        let kept = sweep(vec![row(1, "bob", 5)], Some(24), 10);
        assert_eq!(kept.len(), 1);
    }
}
