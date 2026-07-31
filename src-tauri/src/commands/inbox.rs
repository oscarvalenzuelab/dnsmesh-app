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
use dnsmesh_core::crypto::STORAGE_KEY_LEN;
use zeroize::Zeroizing;

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

/// Rows plus whether any of them came from unsealed lines.
///
/// `had_plaintext` drives a rewrite: a plaintext row that is merely read
/// stays plaintext on disk, so the caller re-emits the file sealed. Losing
/// that flag would mean an upgraded install keeps readable message bodies
/// indefinitely.
struct LoadedInbox {
    rows: Vec<PersistedInboxMessage>,
    had_plaintext: bool,
}

fn load_inbox_file(path: &std::path::Path, key: &[u8]) -> Result<LoadedInbox, CommandError> {
    if !path.exists() {
        return Ok(LoadedInbox {
            rows: Vec::new(),
            had_plaintext: false,
        });
    }
    let raw = std::fs::read_to_string(path).map_err(CommandError::from)?;
    let mut rows = Vec::new();
    let mut had_plaintext = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(plain) = crate::atrest::open(key, line) {
            if let Ok(m) = serde_json::from_slice::<PersistedInboxMessage>(&plain) {
                rows.push(m);
            }
            continue;
        }
        // Not sealed under our key. A row written by a build before this
        // file was encrypted still parses as bare JSON — adopt it so the
        // upgrade doesn't silently drop the user's history, and flag the
        // file for re-emission so the plaintext doesn't survive.
        //
        // Anything that is neither sealed nor parseable JSON — a truncated
        // write, a row sealed under another identity's key — is skipped, as
        // the plaintext loader always did. One bad record costs one message,
        // never the whole history.
        if let Ok(m) = serde_json::from_str::<PersistedInboxMessage>(line) {
            rows.push(m);
            had_plaintext = true;
        }
    }
    Ok(LoadedInbox {
        rows,
        had_plaintext,
    })
}

/// Re-emit `rows` sealed. Used to retire plaintext left by an older build.
fn reseal_inbox(
    path: &std::path::Path,
    key: &[u8],
    rows: &[PersistedInboxMessage],
) -> Result<(), CommandError> {
    let mut out = String::new();
    for m in rows {
        let json = serde_json::to_vec(m).map_err(|e| {
            CommandError::new("internal", format!("serialising inbox row failed: {e}"))
        })?;
        let line = crate::atrest::seal(key, &json)
            .map_err(|e| CommandError::new("internal", format!("sealing inbox row: {e}")))?;
        out.push_str(&line);
        out.push('\n');
    }
    atomic_write(path, out.as_bytes()).map_err(CommandError::from)
}

fn load_read_set(path: &std::path::Path, key: &[u8]) -> Result<HashSet<String>, CommandError> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let raw = std::fs::read_to_string(path).map_err(CommandError::from)?;
    // Sealed as one record. Anything that doesn't open — a plaintext file
    // from before this, a partial write — degrades to "nothing is read",
    // which is recoverable by the user in a way lost messages are not.
    let Some(plain) = crate::atrest::open(key, raw.trim()) else {
        return Ok(HashSet::new());
    };
    let parsed: Vec<String> = serde_json::from_slice(&plain).unwrap_or_default();
    Ok(parsed.into_iter().collect())
}

fn save_read_set(
    path: &std::path::Path,
    key: &[u8],
    set: &HashSet<String>,
) -> Result<(), CommandError> {
    let mut sorted: Vec<&String> = set.iter().collect();
    sorted.sort();
    let body = serde_json::to_vec(&sorted)
        .map_err(|e| CommandError::new("internal", format!("serialising read-set failed: {e}")))?;
    let sealed = crate::atrest::seal(key, &body)
        .map_err(|e| CommandError::new("internal", format!("sealing read-set: {e}")))?;
    atomic_write(path, sealed.as_bytes()).map_err(CommandError::from)
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
    let Some((username, key)) = active_identity(&state).await else {
        return Ok(Vec::new());
    };
    let lock = lock_for(&username).await;
    let _g = lock.lock();
    let inbox_p = inbox_path(&state, &username);
    let read_p = read_state_path(&state, &username);
    let loaded = load_inbox_file(&inbox_p, key.as_ref())?;
    if loaded.had_plaintext {
        // Retire plaintext left by a build that predates encryption.
        reseal_inbox(&inbox_p, key.as_ref(), &loaded.rows)?;
    }
    let messages = loaded.rows;
    let read_set = load_read_set(&read_p, key.as_ref())?;
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
    let Some((username, key)) = active_identity(&state).await else {
        return Ok(InboxAppendResult {
            appended: 0,
            total: 0,
        });
    };
    append_for_username(&state, &username, key.as_ref(), args.messages).await
}

/// Reusable append body. Same dedupe + atomic-rewrite semantics as
/// [`inbox_append`], but callable from other Tauri commands that
/// already know the active username (e.g. the intro promote flow,
/// which must persist before returning success to avoid a window
/// where the durable intro row is consumed but the plaintext has
/// not yet landed on disk).
pub(crate) async fn append_for_username(
    state: &AppState,
    username: &str,
    key: &[u8],
    messages: Vec<PersistedInboxMessage>,
) -> CommandResult<InboxAppendResult> {
    let lock = lock_for(username).await;
    let _g = lock.lock();
    let dir = state.identity_dir(username);
    std::fs::create_dir_all(&dir).map_err(CommandError::from)?;
    let path = inbox_path_for(state, username);
    let existing = load_inbox_file(&path, key)?.rows;
    let mut seen: HashSet<String> = existing.iter().map(|m| m.msg_id_hex.clone()).collect();

    let mut appended = 0usize;
    let mut additions: Vec<PersistedInboxMessage> = Vec::new();
    for m in messages {
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
        let json = serde_json::to_vec(m).map_err(|e| {
            CommandError::new("internal", format!("serialising inbox row failed: {e}"))
        })?;
        let line = crate::atrest::seal(key, &json)
            .map_err(|e| CommandError::new("internal", format!("sealing inbox row: {e}")))?;
        out.push_str(&line);
        out.push('\n');
    }
    atomic_write(&path, out.as_bytes()).map_err(CommandError::from)?;

    Ok(InboxAppendResult {
        appended,
        total: existing.len() + appended,
    })
}

fn inbox_path_for(state: &AppState, username: &str) -> PathBuf {
    state.identity_dir(username).join(INBOX_FILE)
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
    let Some((username, key)) = active_identity(&state).await else {
        return Ok(());
    };
    let lock = lock_for(&username).await;
    let _g = lock.lock();
    let path = read_state_path(&state, &username);
    let mut set = load_read_set(&path, key.as_ref())?;
    if !args.msg_id_hex.is_empty() && set.insert(args.msg_id_hex) {
        save_read_set(&path, key.as_ref(), &set)?;
    }
    Ok(())
}

/// Mark every message currently in the persistent inbox as read for
/// the active identity. Idempotent.
#[tauri::command]
pub async fn inbox_mark_all_read(state: State<'_, AppState>) -> CommandResult<()> {
    let Some((username, key)) = active_identity(&state).await else {
        return Ok(());
    };
    let lock = lock_for(&username).await;
    let _g = lock.lock();
    let inbox_p = inbox_path(&state, &username);
    let read_p = read_state_path(&state, &username);
    let messages = load_inbox_file(&inbox_p, key.as_ref())?.rows;
    let mut set = load_read_set(&read_p, key.as_ref())?;
    let mut changed = false;
    for m in messages {
        if set.insert(m.msg_id_hex) {
            changed = true;
        }
    }
    if changed {
        save_read_set(&read_p, key.as_ref(), &set)?;
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
    let Some((username, key)) = active_identity(&state).await else {
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

    let existing = load_inbox_file(&inbox_p, key.as_ref())?.rows;
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
        let mut read_set = load_read_set(&read_p, key.as_ref())?;
        let before = read_set.len();
        read_set
            .retain(|id| !targets.contains(&id.to_ascii_lowercase()) && !removed_ids.contains(id));
        if read_set.len() != before {
            save_read_set(&read_p, key.as_ref(), &read_set)?;
        }
    }

    Ok(InboxDeleteResult { removed })
}

/// Active identity's username plus the at-rest key its files are sealed
/// under. Both come from the unlocked client, so a locked app simply has
/// no way to read the history — which is the point.
async fn active_identity(
    state: &State<'_, AppState>,
) -> Option<(String, Zeroizing<[u8; STORAGE_KEY_LEN]>)> {
    let guard = state.active.read().await;
    guard
        .as_ref()
        .map(|a| (a.username.clone(), a.client.storage_key()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed at-rest key for the unit tests. Production callers take
    /// theirs from the unlocked client.
    const TEST_KEY: [u8; STORAGE_KEY_LEN] = [0x2a; STORAGE_KEY_LEN];
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
        let existing = load_inbox_file(&path, &TEST_KEY).unwrap().rows;
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
            let json = serde_json::to_vec(m).unwrap();
            out.push_str(&crate::atrest::seal(&TEST_KEY, &json).unwrap());
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
        let loaded = load_inbox_file(&inbox_path(&state, "alice"), &TEST_KEY)
            .unwrap()
            .rows;
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
        let loaded = load_inbox_file(&inbox_path(&state, "alice"), &TEST_KEY)
            .unwrap()
            .rows;
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
        save_read_set(&path, &TEST_KEY, &set).unwrap();
        let loaded = load_read_set(&path, &TEST_KEY).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains("msg-a"));
        assert!(loaded.contains("msg-b"));
    }

    #[test]
    fn missing_files_load_as_empty() {
        let (state, _tmp) = fresh_state();
        let loaded = load_inbox_file(&inbox_path(&state, "ghost"), &TEST_KEY)
            .unwrap()
            .rows;
        assert!(loaded.is_empty());
        let read = load_read_set(&read_state_path(&state, "ghost"), &TEST_KEY).unwrap();
        assert!(read.is_empty());
    }

    /// Corrupt lines must be skipped, not abort the load.
    #[test]
    fn corrupt_line_is_skipped() {
        let (state, _tmp) = fresh_state();
        let dir = state.identity_dir("alice");
        std::fs::create_dir_all(&dir).unwrap();
        let path = inbox_path(&state, "alice");
        let seal = |m: &PersistedInboxMessage| {
            crate::atrest::seal(&TEST_KEY, &serde_json::to_vec(m).unwrap()).unwrap()
        };
        let mut body = String::new();
        body.push_str(&seal(&sample(1)));
        body.push('\n');
        // Junk from a truncated write.
        body.push_str("{not json\n");
        // A plaintext row from a build that predates at-rest encryption is
        // adopted rather than skipped — see
        // `plaintext_rows_are_adopted_and_flagged_for_reseal`. Included here
        // to prove it survives alongside junk rather than aborting the load.
        body.push_str(&serde_json::to_string(&sample(9)).unwrap());
        body.push('\n');
        // A row sealed under a different identity's key.
        body.push_str(&crate::atrest::seal(&[0x99; STORAGE_KEY_LEN], b"{}").unwrap());
        body.push('\n');
        body.push_str(&seal(&sample(2)));
        body.push('\n');
        std::fs::write(&path, body).unwrap();
        let loaded = load_inbox_file(&path, &TEST_KEY).unwrap().rows;
        // Two sealed rows plus the adopted legacy one. The truncated line
        // and the row sealed under a foreign key are both dropped.
        assert_eq!(loaded.len(), 3);
        let ids: Vec<&str> = loaded.iter().map(|m| m.msg_id_hex.as_str()).collect();
        assert!(ids.contains(&sample(1).msg_id_hex.as_str()));
        assert!(ids.contains(&sample(2).msg_id_hex.as_str()));
        assert!(
            ids.contains(&sample(9).msg_id_hex.as_str()),
            "legacy plaintext row should be adopted, not dropped"
        );
    }

    /// Upgrade path: rows written by a build before the file was encrypted
    /// must still load, and must not be left readable on disk afterwards.
    #[test]
    fn plaintext_rows_are_adopted_and_flagged_for_reseal() {
        let (state, _tmp) = fresh_state();
        let dir = state.identity_dir("alice");
        std::fs::create_dir_all(&dir).unwrap();
        let path = inbox_path(&state, "alice");

        // Exactly what the old loader wrote: bare JSON, one per line.
        let mut body = String::new();
        body.push_str(&serde_json::to_string(&sample(1)).unwrap());
        body.push('\n');
        body.push_str(&serde_json::to_string(&sample(2)).unwrap());
        body.push('\n');
        std::fs::write(&path, &body).unwrap();

        let loaded = load_inbox_file(&path, &TEST_KEY).unwrap();
        assert_eq!(loaded.rows.len(), 2, "legacy history must not be dropped");
        assert!(
            loaded.had_plaintext,
            "plaintext rows must be flagged so the file gets re-emitted",
        );

        // Re-emitting must remove the readable copy while keeping the rows.
        reseal_inbox(&path, &TEST_KEY, &loaded.rows).unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(
            !raw.windows(7).any(|w| w == b"hello 1"),
            "message body still readable after reseal",
        );
        let after = load_inbox_file(&path, &TEST_KEY).unwrap();
        assert_eq!(after.rows.len(), 2);
        assert!(
            !after.had_plaintext,
            "reseal should leave nothing plaintext"
        );
    }

    /// Drives the same delete write path as the command, without a
    /// `State<AppState>`.
    fn delete_direct(state: &AppState, username: &str, ids: &[String]) -> usize {
        let inbox_p = inbox_path(state, username);
        let read_p = read_state_path(state, username);
        let targets: HashSet<String> = ids.iter().map(|s| s.to_ascii_lowercase()).collect();
        let existing = load_inbox_file(&inbox_p, &TEST_KEY).unwrap().rows;
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
                let json = serde_json::to_vec(m).unwrap();
                out.push_str(&crate::atrest::seal(&TEST_KEY, &json).unwrap());
                out.push('\n');
            }
            atomic_write(&inbox_p, out.as_bytes()).unwrap();
            let mut read_set = load_read_set(&read_p, &TEST_KEY).unwrap();
            let before = read_set.len();
            read_set.retain(|id| {
                !targets.contains(&id.to_ascii_lowercase()) && !removed_ids.contains(id)
            });
            if read_set.len() != before {
                save_read_set(&read_p, &TEST_KEY, &read_set).unwrap();
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
        let loaded = load_inbox_file(&inbox_path(&state, "alice"), &TEST_KEY)
            .unwrap()
            .rows;
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
        let loaded = load_inbox_file(&inbox_path(&state, "alice"), &TEST_KEY)
            .unwrap()
            .rows;
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn delete_empty_list_is_noop() {
        let (state, _tmp) = fresh_state();
        append_direct(&state, "alice", vec![sample(1), sample(2)]);
        let removed = delete_direct(&state, "alice", &[]);
        assert_eq!(removed, 0);
        let loaded = load_inbox_file(&inbox_path(&state, "alice"), &TEST_KEY)
            .unwrap()
            .rows;
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
        save_read_set(&read_p, &TEST_KEY, &set).unwrap();

        let removed = delete_direct(&state, "alice", &[sample(1).msg_id_hex]);
        assert_eq!(removed, 1);

        let after = load_read_set(&read_p, &TEST_KEY).unwrap();
        assert_eq!(after.len(), 1);
        assert!(!after.contains(&sample(1).msg_id_hex));
        assert!(after.contains(&sample(2).msg_id_hex));
    }

    #[test]
    fn mark_read_persists() {
        let (state, _tmp) = fresh_state();
        append_direct(&state, "alice", vec![sample(1), sample(2)]);
        let path = read_state_path(&state, "alice");
        let mut set = load_read_set(&path, &TEST_KEY).unwrap();
        set.insert(sample(1).msg_id_hex);
        save_read_set(&path, &TEST_KEY, &set).unwrap();
        let messages = load_inbox_file(&inbox_path(&state, "alice"), &TEST_KEY)
            .unwrap()
            .rows;
        let read_set = load_read_set(&path, &TEST_KEY).unwrap();
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
