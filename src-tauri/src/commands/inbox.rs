//! Per-identity persistent inbox.
//!
//! The SDK's `receive_messages` flips the replay cache on delivery, so
//! the desktop keeps its own copy: messages must survive identity
//! switches, app restarts, and navigation away from the Inbox.
//!
//! On-disk layout, one directory per identity:
//!
//! - `<identity-dir>/inbox.jsonl` — append-only log of
//!   [`PersistedInboxMessage`] rows, one JSON per line.
//! - `<identity-dir>/inbox-read.json` — JSON array of read
//!   `msg_id_hex` strings. Separate file so mark-read doesn't rewrite
//!   the whole inbox.
//!
//! Writes go through a per-username `parking_lot::Mutex` so reads
//! never see a half-written rename target.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::sync::OnceCell;

use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

/// One persisted inbox row. Owned here (not aliased to
/// [`crate::commands::messaging::InboxMessageView`]) so the on-disk
/// shape can evolve independently of the JS-facing view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedInboxMessage {
    pub sender_signing_pk_hex: String,
    pub msg_id_hex: String,
    pub timestamp: u64,
    pub plaintext_utf8: String,
    pub plaintext_bytes: Vec<u8>,
    /// SPK-verified `user@host` label from the inbound DMPv2
    /// envelope. Added in 0.1.0-alpha.7; older `inbox.jsonl` rows
    /// deserialize as `None` via the serde default.
    #[serde(default)]
    pub sender_label: Option<String>,
}

/// File name used for the per-identity append-only inbox log.
const INBOX_FILE: &str = "inbox.jsonl";

/// File name used for the per-identity read-state set.
const INBOX_READ_FILE: &str = "inbox-read.json";

/// Per-identity I/O locks, keyed by sanitised username.
static INBOX_LOCKS: OnceCell<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceCell::const_new();

async fn lock_for(username: &str) -> Arc<Mutex<()>> {
    let map = INBOX_LOCKS
        .get_or_init(|| async { Mutex::new(HashMap::new()) })
        .await;
    let mut guard = map.lock();
    guard
        .entry(username.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn inbox_path(state: &AppState, username: &str) -> PathBuf {
    state.identity_dir(username).join(INBOX_FILE)
}

fn read_state_path(state: &AppState, username: &str) -> PathBuf {
    state.identity_dir(username).join(INBOX_READ_FILE)
}

/// Atomically write `bytes` to `path` via a sibling `.tmp` + rename.
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("data"),
    ));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn load_inbox_file(path: &std::path::Path) -> Result<Vec<PersistedInboxMessage>, CommandError> {
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
        // Skip corrupt lines so an interrupted write that left a
        // half-line behind doesn't take the whole inbox down.
        if let Ok(m) = serde_json::from_str::<PersistedInboxMessage>(line) {
            out.push(m);
        }
    }
    Ok(out)
}

fn load_read_set(path: &std::path::Path) -> Result<HashSet<String>, CommandError> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let raw = std::fs::read_to_string(path).map_err(CommandError::from)?;
    let parsed: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
    Ok(parsed.into_iter().collect())
}

fn save_read_set(path: &std::path::Path, set: &HashSet<String>) -> Result<(), CommandError> {
    let mut sorted: Vec<&String> = set.iter().collect();
    sorted.sort();
    let body = serde_json::to_vec(&sorted)
        .map_err(|e| CommandError::new("internal", format!("serialising read-set failed: {e}")))?;
    atomic_write(path, &body).map_err(CommandError::from)
}

/// One inbox row as the frontend consumes it. Same payload as
/// [`PersistedInboxMessage`] plus the derived `read` flag.
#[derive(Debug, Clone, Serialize)]
pub struct InboxRow {
    pub sender_signing_pk_hex: String,
    pub msg_id_hex: String,
    pub timestamp: u64,
    pub plaintext_utf8: String,
    pub plaintext_bytes: Vec<u8>,
    pub sender_label: Option<String>,
    pub read: bool,
}

/// Load every persisted inbox row for the active identity, attaching a
/// `read` flag from the read-state file. Returns empty when the file
/// doesn't exist (fresh identity) or no identity is unlocked.
#[tauri::command]
pub async fn inbox_load(state: State<'_, AppState>) -> CommandResult<Vec<InboxRow>> {
    let Some(username) = active_username(&state).await else {
        return Ok(Vec::new());
    };
    let lock = lock_for(&username).await;
    let _g = lock.lock();
    let inbox_p = inbox_path(&state, &username);
    let read_p = read_state_path(&state, &username);
    let messages = load_inbox_file(&inbox_p)?;
    let read_set = load_read_set(&read_p)?;
    Ok(messages
        .into_iter()
        .map(|m| {
            let read = read_set.contains(&m.msg_id_hex);
            InboxRow {
                sender_signing_pk_hex: m.sender_signing_pk_hex,
                msg_id_hex: m.msg_id_hex,
                timestamp: m.timestamp,
                plaintext_utf8: m.plaintext_utf8,
                plaintext_bytes: m.plaintext_bytes,
                sender_label: m.sender_label,
                read,
            }
        })
        .collect())
}

/// Args for [`inbox_append`].
#[derive(Debug, Clone, Deserialize)]
pub struct InboxAppendArgs {
    pub messages: Vec<PersistedInboxMessage>,
}

/// Result of [`inbox_append`]: how many messages were actually new and
/// written to disk. Duplicates (already-known `msg_id_hex`) are silently
/// skipped.
#[derive(Debug, Clone, Serialize)]
pub struct InboxAppendResult {
    pub appended: usize,
    pub total: usize,
}

/// Append `messages` to the active identity's persistent inbox,
/// deduping against the existing log by `msg_id_hex`. No-op when no
/// identity is unlocked.
#[tauri::command]
pub async fn inbox_append(
    args: InboxAppendArgs,
    state: State<'_, AppState>,
) -> CommandResult<InboxAppendResult> {
    let Some(username) = active_username(&state).await else {
        return Ok(InboxAppendResult {
            appended: 0,
            total: 0,
        });
    };
    let lock = lock_for(&username).await;
    let _g = lock.lock();
    let dir = state.identity_dir(&username);
    std::fs::create_dir_all(&dir).map_err(CommandError::from)?;
    let path = inbox_path(&state, &username);
    let existing = load_inbox_file(&path)?;
    let mut seen: HashSet<String> = existing.iter().map(|m| m.msg_id_hex.clone()).collect();

    let mut appended = 0usize;
    let mut additions: Vec<PersistedInboxMessage> = Vec::new();
    for m in args.messages {
        if seen.insert(m.msg_id_hex.clone()) {
            additions.push(m);
            appended += 1;
        }
    }
    if appended == 0 {
        return Ok(InboxAppendResult {
            appended: 0,
            total: existing.len(),
        });
    }

    // Full-file rewrite via the atomic-rename helper. Inbox files are
    // small enough that this beats appending + fsync.
    let mut out = String::new();
    for m in existing.iter().chain(additions.iter()) {
        let line = serde_json::to_string(m).map_err(|e| {
            CommandError::new("internal", format!("serialising inbox row failed: {e}"))
        })?;
        out.push_str(&line);
        out.push('\n');
    }
    atomic_write(&path, out.as_bytes()).map_err(CommandError::from)?;

    Ok(InboxAppendResult {
        appended,
        total: existing.len() + appended,
    })
}

/// Args for [`inbox_mark_read`].
#[derive(Debug, Clone, Deserialize)]
pub struct InboxMarkReadArgs {
    pub msg_id_hex: String,
}

/// Mark a single message as read for the active identity. Idempotent.
/// No-op when no identity is unlocked.
#[tauri::command]
pub async fn inbox_mark_read(
    args: InboxMarkReadArgs,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    let Some(username) = active_username(&state).await else {
        return Ok(());
    };
    let lock = lock_for(&username).await;
    let _g = lock.lock();
    let path = read_state_path(&state, &username);
    let mut set = load_read_set(&path)?;
    if !args.msg_id_hex.is_empty() && set.insert(args.msg_id_hex) {
        save_read_set(&path, &set)?;
    }
    Ok(())
}

/// Mark every message currently in the persistent inbox as read for
/// the active identity. Idempotent.
#[tauri::command]
pub async fn inbox_mark_all_read(state: State<'_, AppState>) -> CommandResult<()> {
    let Some(username) = active_username(&state).await else {
        return Ok(());
    };
    let lock = lock_for(&username).await;
    let _g = lock.lock();
    let inbox_p = inbox_path(&state, &username);
    let read_p = read_state_path(&state, &username);
    let messages = load_inbox_file(&inbox_p)?;
    let mut set = load_read_set(&read_p)?;
    let mut changed = false;
    for m in messages {
        if set.insert(m.msg_id_hex) {
            changed = true;
        }
    }
    if changed {
        save_read_set(&read_p, &set)?;
    }
    Ok(())
}

/// Args for [`inbox_delete`].
#[derive(Debug, Clone, Deserialize)]
pub struct InboxDeleteArgs {
    pub msg_id_hexes: Vec<String>,
}

/// Result of [`inbox_delete`]: how many of the requested ids were
/// actually present and removed.
#[derive(Debug, Clone, Serialize)]
pub struct InboxDeleteResult {
    pub removed: usize,
}

/// Permanently remove the rows whose `msg_id_hex` appears in
/// `args.msg_id_hexes`. Case-insensitive, idempotent, and also drops
/// matching read-state entries so a future re-publish under the same
/// id starts unread. No-op when no identity is unlocked.
#[tauri::command]
pub async fn inbox_delete(
    args: InboxDeleteArgs,
    state: State<'_, AppState>,
) -> CommandResult<InboxDeleteResult> {
    if args.msg_id_hexes.is_empty() {
        return Ok(InboxDeleteResult { removed: 0 });
    }
    let Some(username) = active_username(&state).await else {
        return Ok(InboxDeleteResult { removed: 0 });
    };
    let lock = lock_for(&username).await;
    let _g = lock.lock();
    let inbox_p = inbox_path(&state, &username);
    let read_p = read_state_path(&state, &username);

    // Hex ids; compare case-insensitively so callers don't have to
    // pick a canonical casing.
    let targets: HashSet<String> = args
        .msg_id_hexes
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .collect();

    let existing = load_inbox_file(&inbox_p)?;
    let original_len = existing.len();
    let mut kept: Vec<PersistedInboxMessage> = Vec::with_capacity(original_len);
    let mut removed_ids: HashSet<String> = HashSet::new();
    for m in existing {
        let key = m.msg_id_hex.to_ascii_lowercase();
        if targets.contains(&key) {
            removed_ids.insert(m.msg_id_hex.clone());
        } else {
            kept.push(m);
        }
    }
    let removed = original_len - kept.len();

    if removed > 0 {
        let mut out = String::new();
        for m in &kept {
            let line = serde_json::to_string(m).map_err(|e| {
                CommandError::new("internal", format!("serialising inbox row failed: {e}"))
            })?;
            out.push_str(&line);
            out.push('\n');
        }
        atomic_write(&inbox_p, out.as_bytes()).map_err(CommandError::from)?;

        // Drop deleted ids from the read-state file too. Case-
        // insensitive to catch entries written before normalisation.
        let mut read_set = load_read_set(&read_p)?;
        let before = read_set.len();
        read_set
            .retain(|id| !targets.contains(&id.to_ascii_lowercase()) && !removed_ids.contains(id));
        if read_set.len() != before {
            save_read_set(&read_p, &read_set)?;
        }
    }

    Ok(InboxDeleteResult { removed })
}

async fn active_username(state: &State<'_, AppState>) -> Option<String> {
    let guard = state.active.read().await;
    guard.as_ref().map(|a| a.username.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_state() -> (AppState, TempDir) {
        let dir = TempDir::new().unwrap();
        (AppState::new(dir.path().to_path_buf()), dir)
    }

    fn sample(id: u8) -> PersistedInboxMessage {
        PersistedInboxMessage {
            sender_signing_pk_hex: "ab".repeat(32),
            msg_id_hex: format!("{id:032x}"),
            timestamp: 1_700_000_000 + u64::from(id),
            plaintext_utf8: format!("hello {id}"),
            plaintext_bytes: format!("hello {id}").into_bytes(),
            sender_label: None,
        }
    }

    fn append_direct(state: &AppState, username: &str, batch: Vec<PersistedInboxMessage>) -> usize {
        let dir = state.identity_dir(username);
        std::fs::create_dir_all(&dir).unwrap();
        let path = inbox_path(state, username);
        let existing = load_inbox_file(&path).unwrap();
        let mut seen: HashSet<String> = existing.iter().map(|m| m.msg_id_hex.clone()).collect();
        let mut additions = Vec::new();
        for m in batch {
            if seen.insert(m.msg_id_hex.clone()) {
                additions.push(m);
            }
        }
        let added = additions.len();
        if added == 0 {
            return 0;
        }
        let mut out = String::new();
        for m in existing.iter().chain(additions.iter()) {
            out.push_str(&serde_json::to_string(m).unwrap());
            out.push('\n');
        }
        atomic_write(&path, out.as_bytes()).unwrap();
        added
    }

    #[test]
    fn append_and_load_round_trip() {
        let (state, _tmp) = fresh_state();
        let n = append_direct(&state, "alice", vec![sample(1), sample(2)]);
        assert_eq!(n, 2);
        let loaded = load_inbox_file(&inbox_path(&state, "alice")).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].msg_id_hex, sample(1).msg_id_hex);
        assert_eq!(loaded[1].plaintext_utf8, "hello 2");
    }

    #[test]
    fn append_dedupes_by_msg_id() {
        let (state, _tmp) = fresh_state();
        append_direct(&state, "alice", vec![sample(1), sample(2)]);
        let n = append_direct(&state, "alice", vec![sample(2), sample(3)]);
        assert_eq!(n, 1, "sample(2) should be deduped");
        let loaded = load_inbox_file(&inbox_path(&state, "alice")).unwrap();
        assert_eq!(loaded.len(), 3);
    }

    #[test]
    fn read_set_round_trip() {
        let (state, _tmp) = fresh_state();
        let dir = state.identity_dir("alice");
        std::fs::create_dir_all(&dir).unwrap();
        let path = read_state_path(&state, "alice");
        let mut set = HashSet::new();
        set.insert("msg-a".to_string());
        set.insert("msg-b".to_string());
        save_read_set(&path, &set).unwrap();
        let loaded = load_read_set(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains("msg-a"));
        assert!(loaded.contains("msg-b"));
    }

    #[test]
    fn missing_files_load_as_empty() {
        let (state, _tmp) = fresh_state();
        let loaded = load_inbox_file(&inbox_path(&state, "ghost")).unwrap();
        assert!(loaded.is_empty());
        let read = load_read_set(&read_state_path(&state, "ghost")).unwrap();
        assert!(read.is_empty());
    }

    /// Corrupt lines must be skipped, not abort the load.
    #[test]
    fn corrupt_line_is_skipped() {
        let (state, _tmp) = fresh_state();
        let dir = state.identity_dir("alice");
        std::fs::create_dir_all(&dir).unwrap();
        let path = inbox_path(&state, "alice");
        let mut body = String::new();
        body.push_str(&serde_json::to_string(&sample(1)).unwrap());
        body.push('\n');
        body.push_str("{not json\n");
        body.push_str(&serde_json::to_string(&sample(2)).unwrap());
        body.push('\n');
        std::fs::write(&path, body).unwrap();
        let loaded = load_inbox_file(&path).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    /// Drives the same delete write path as the command, without a
    /// `State<AppState>`.
    fn delete_direct(state: &AppState, username: &str, ids: &[String]) -> usize {
        let inbox_p = inbox_path(state, username);
        let read_p = read_state_path(state, username);
        let targets: HashSet<String> = ids.iter().map(|s| s.to_ascii_lowercase()).collect();
        let existing = load_inbox_file(&inbox_p).unwrap();
        let original_len = existing.len();
        let mut kept: Vec<PersistedInboxMessage> = Vec::new();
        let mut removed_ids: HashSet<String> = HashSet::new();
        for m in existing {
            let key = m.msg_id_hex.to_ascii_lowercase();
            if targets.contains(&key) {
                removed_ids.insert(m.msg_id_hex.clone());
            } else {
                kept.push(m);
            }
        }
        let removed = original_len - kept.len();
        if removed > 0 {
            let mut out = String::new();
            for m in &kept {
                out.push_str(&serde_json::to_string(m).unwrap());
                out.push('\n');
            }
            atomic_write(&inbox_p, out.as_bytes()).unwrap();
            let mut read_set = load_read_set(&read_p).unwrap();
            let before = read_set.len();
            read_set.retain(|id| {
                !targets.contains(&id.to_ascii_lowercase()) && !removed_ids.contains(id)
            });
            if read_set.len() != before {
                save_read_set(&read_p, &read_set).unwrap();
            }
        }
        removed
    }

    #[test]
    fn delete_known_id_removes_row() {
        let (state, _tmp) = fresh_state();
        append_direct(&state, "alice", vec![sample(1), sample(2), sample(3)]);
        let removed = delete_direct(&state, "alice", &[sample(2).msg_id_hex]);
        assert_eq!(removed, 1);
        let loaded = load_inbox_file(&inbox_path(&state, "alice")).unwrap();
        assert_eq!(loaded.len(), 2);
        let ids: Vec<&str> = loaded.iter().map(|m| m.msg_id_hex.as_str()).collect();
        assert!(ids.contains(&sample(1).msg_id_hex.as_str()));
        assert!(ids.contains(&sample(3).msg_id_hex.as_str()));
        assert!(!ids.contains(&sample(2).msg_id_hex.as_str()));
    }

    #[test]
    fn delete_missing_id_is_idempotent() {
        let (state, _tmp) = fresh_state();
        append_direct(&state, "alice", vec![sample(1), sample(2)]);
        let removed = delete_direct(&state, "alice", &["deadbeef".repeat(8)]);
        assert_eq!(removed, 0);
        let loaded = load_inbox_file(&inbox_path(&state, "alice")).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn delete_empty_list_is_noop() {
        let (state, _tmp) = fresh_state();
        append_direct(&state, "alice", vec![sample(1), sample(2)]);
        let removed = delete_direct(&state, "alice", &[]);
        assert_eq!(removed, 0);
        let loaded = load_inbox_file(&inbox_path(&state, "alice")).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn delete_drops_read_state_entry() {
        let (state, _tmp) = fresh_state();
        append_direct(&state, "alice", vec![sample(1), sample(2)]);
        let read_p = read_state_path(&state, "alice");
        let mut set = HashSet::new();
        set.insert(sample(1).msg_id_hex);
        set.insert(sample(2).msg_id_hex);
        save_read_set(&read_p, &set).unwrap();

        let removed = delete_direct(&state, "alice", &[sample(1).msg_id_hex]);
        assert_eq!(removed, 1);

        let after = load_read_set(&read_p).unwrap();
        assert_eq!(after.len(), 1);
        assert!(!after.contains(&sample(1).msg_id_hex));
        assert!(after.contains(&sample(2).msg_id_hex));
    }

    #[test]
    fn mark_read_persists() {
        let (state, _tmp) = fresh_state();
        append_direct(&state, "alice", vec![sample(1), sample(2)]);
        let path = read_state_path(&state, "alice");
        let mut set = load_read_set(&path).unwrap();
        set.insert(sample(1).msg_id_hex);
        save_read_set(&path, &set).unwrap();
        let messages = load_inbox_file(&inbox_path(&state, "alice")).unwrap();
        let read_set = load_read_set(&path).unwrap();
        let rows: Vec<InboxRow> = messages
            .into_iter()
            .map(|m| InboxRow {
                read: read_set.contains(&m.msg_id_hex),
                sender_signing_pk_hex: m.sender_signing_pk_hex,
                msg_id_hex: m.msg_id_hex,
                timestamp: m.timestamp,
                plaintext_utf8: m.plaintext_utf8,
                plaintext_bytes: m.plaintext_bytes,
                sender_label: m.sender_label,
            })
            .collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].read, "first message should be marked read");
        assert!(!rows[1].read, "second message should still be unread");
    }
}
