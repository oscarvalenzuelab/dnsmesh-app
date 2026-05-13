//! Per-identity backup: export to a `.dmp-backup.tar.gz`, import back.
//!
//! Archive layout (single tar.gz):
//!
//! ```text
//! backup-meta.json                          # version, generated_at, user, domain
//! identities/<username>/config.yaml
//! identities/<username>/dmp-rs.sqlite
//! identities/<username>/tsig.key            # if present
//! identities/<username>/inbox.jsonl         # if present
//! identities/<username>/inbox-read.json     # if present
//! ```
//!
//! The archive is NOT encrypted — it carries every secret needed to
//! send and receive. The UI warns the user.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use tar::{Archive, Builder, Header};
use tauri::State;

use crate::error::{CommandError, CommandResult};
use crate::state::{
    AppState, IDENTITY_CONFIG_FILE, IdentityConfig, IdentityIndexEntry, sanitize_username,
};

/// Stable on-disk file names that survive an export → import round-trip.
const DB_FILE: &str = "dmp-rs.sqlite";
const TSIG_FILE: &str = "tsig.key";
const INBOX_FILE: &str = "inbox.jsonl";
const INBOX_READ_FILE: &str = "inbox-read.json";
const META_FILE: &str = "backup-meta.json";

/// Current backup-meta schema version. Bump on incompatible shape
/// changes.
const BACKUP_VERSION: u32 = 1;

/// Auto-appended suffix when missing. Two components so
/// `Path::extension` behaves predictably.
const ARCHIVE_SUFFIX: &str = ".dmp-backup.tar.gz";

/// `backup-meta.json` payload. Read first so a wrong-version archive
/// fails before any extraction work.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupMeta {
    version: u32,
    generated_at: u64,
    username: String,
    domain: String,
}

// ---------- Export ---------------------------------------------------

/// Args for [`export_identity_backup`].
#[derive(Debug, Clone, Deserialize)]
pub struct ExportArgs {
    pub username: String,
    /// Output path; `.dmp-backup.tar.gz` is auto-appended if missing.
    pub output_path: PathBuf,
}

/// Result of a successful export.
#[derive(Debug, Clone, Serialize)]
pub struct ExportResult {
    pub archive_path: PathBuf,
    pub total_bytes: u64,
    /// File count including `backup-meta.json`.
    pub file_count: u32,
}

/// Build the tar.gz in-memory and write atomically via a sibling .tmp.
#[tauri::command]
pub async fn export_identity_backup(
    args: ExportArgs,
    state: State<'_, AppState>,
) -> CommandResult<ExportResult> {
    do_export(args, &state)
}

#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn do_export(args: ExportArgs, state: &AppState) -> CommandResult<ExportResult> {
    let username = sanitize_username(&args.username)?;

    let index = state.load_index().map_err(CommandError::from)?;
    let entry = index
        .identities
        .iter()
        .find(|e| e.username == username)
        .ok_or_else(|| {
            CommandError::new(
                "validation",
                format!("identity `{username}` is not in the index"),
            )
        })?;
    let domain = entry.domain.clone();

    let archive_path = ensure_archive_suffix(args.output_path.clone());
    let parent = archive_path.parent().ok_or_else(|| {
        CommandError::new(
            "validation",
            format!("output_path {} has no parent", archive_path.display()),
        )
    })?;
    if !parent.is_dir() {
        return Err(CommandError::new(
            "validation",
            format!(
                "output directory {} does not exist or is not a directory",
                parent.display()
            ),
        ));
    }

    // Inputs are small (sqlite + JSONL); buffer in memory and
    // atomic_write rather than streaming.
    let identity_dir = state.identity_dir(&username);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| CommandError::new("internal", format!("clock failure: {e}")))?
        .as_secs();
    let meta = BackupMeta {
        version: BACKUP_VERSION,
        generated_at: now,
        username: username.clone(),
        domain: domain.clone(),
    };
    let meta_bytes = serde_json::to_vec_pretty(&meta).map_err(|e| {
        CommandError::new("internal", format!("serialising backup-meta failed: {e}"))
    })?;

    let mut buffer: Vec<u8> = Vec::new();
    let mut file_count: u32 = 0;
    {
        let gz = GzEncoder::new(&mut buffer, Compression::default());
        let mut tar = Builder::new(gz);

        // backup-meta first so the importer can short-circuit on
        // version mismatch without scanning every entry.
        append_bytes(&mut tar, META_FILE, &meta_bytes, now)?;
        file_count += 1;

        // Required entries.
        let cfg_path = identity_dir.join(IDENTITY_CONFIG_FILE);
        if !cfg_path.is_file() {
            return Err(CommandError::new(
                "validation",
                format!(
                    "identity {username} has no config.yaml at {}",
                    cfg_path.display()
                ),
            ));
        }
        append_file(
            &mut tar,
            &archive_entry_path(&username, IDENTITY_CONFIG_FILE),
            &cfg_path,
        )?;
        file_count += 1;

        let db_path = state.identity_db_path(&username);
        if !db_path.is_file() {
            return Err(CommandError::new(
                "validation",
                format!(
                    "identity {username} has no sqlite database at {}",
                    db_path.display()
                ),
            ));
        }
        append_file(&mut tar, &archive_entry_path(&username, DB_FILE), &db_path)?;
        file_count += 1;

        // Optional entries — skipped when absent so a fresh identity
        // still backs up cleanly.
        for optional in [TSIG_FILE, INBOX_FILE, INBOX_READ_FILE] {
            let src = identity_dir.join(optional);
            if src.is_file() {
                append_file(&mut tar, &archive_entry_path(&username, optional), &src)?;
                file_count += 1;
            }
        }

        let gz_inner = tar
            .into_inner()
            .map_err(|e| CommandError::new("io", format!("finalising tar failed: {e}")))?;
        gz_inner
            .finish()
            .map_err(|e| CommandError::new("io", format!("finalising gzip failed: {e}")))?;
    }

    // <archive>.tmp → rename. A crash leaves a `.tmp` for the next
    // export to overwrite; users never see a half-written backup.
    let tmp_path = with_tmp_suffix(&archive_path);
    std::fs::write(&tmp_path, &buffer).map_err(CommandError::from)?;
    std::fs::rename(&tmp_path, &archive_path).map_err(CommandError::from)?;

    let total_bytes = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
    Ok(ExportResult {
        archive_path,
        total_bytes,
        file_count,
    })
}

/// In-archive path for an identity-scoped file:
/// `identities/<username>/<file>`.
fn archive_entry_path(username: &str, file_name: &str) -> String {
    format!("identities/{username}/{file_name}")
}

/// Sibling `.tmp` path for the atomic write.
fn with_tmp_suffix(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

/// Auto-append `.dmp-backup.tar.gz` when missing. Case-insensitive
/// suffix check so `.tar.gz` still gets the canonical form (the
/// import command keys off it).
fn ensure_archive_suffix(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if s.to_ascii_lowercase().ends_with(ARCHIVE_SUFFIX) {
        return path;
    }
    let mut out = path.into_os_string();
    out.push(ARCHIVE_SUFFIX);
    PathBuf::from(out)
}

/// Append `bytes` under `name` with a 0644/`mtime` header so
/// extraction lands a sensible-looking file.
fn append_bytes<W: std::io::Write>(
    tar: &mut Builder<W>,
    name: &str,
    bytes: &[u8],
    mtime: u64,
) -> Result<(), CommandError> {
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(mtime);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
        .map_err(|e| CommandError::new("io", format!("appending {name} to archive failed: {e}")))?;
    Ok(())
}

/// Append `path`'s contents under `name`. Reads into RAM (inputs are
/// small).
fn append_file<W: std::io::Write>(
    tar: &mut Builder<W>,
    name: &str,
    path: &Path,
) -> Result<(), CommandError> {
    let bytes = std::fs::read(path)
        .map_err(|e| CommandError::new("io", format!("reading {} failed: {e}", path.display())))?;
    let mtime = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    append_bytes(tar, name, &bytes, mtime)
}

// ---------- Import ---------------------------------------------------

/// Args for [`import_identity_backup`].
#[derive(Debug, Clone, Deserialize)]
pub struct ImportArgs {
    pub archive_path: PathBuf,
    /// Override `backup-meta.username` to avoid an index collision.
    #[serde(default)]
    pub override_username: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportResult {
    pub username: String,
    pub domain: String,
    /// Files extracted into the per-identity dir. Excludes
    /// `backup-meta.json`.
    pub file_count: u32,
}

#[tauri::command]
pub async fn import_identity_backup(
    args: ImportArgs,
    state: State<'_, AppState>,
) -> CommandResult<ImportResult> {
    do_import(args, &state)
}

#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn do_import(args: ImportArgs, state: &AppState) -> CommandResult<ImportResult> {
    if !args.archive_path.is_file() {
        return Err(CommandError::new(
            "validation",
            format!("archive {} does not exist", args.archive_path.display()),
        ));
    }
    // Slurp into RAM so we can take two passes: read backup-meta.json
    // first, then extract with rewritten paths.
    let raw_gz = std::fs::read(&args.archive_path).map_err(CommandError::from)?;
    let meta = read_meta(&raw_gz)?;
    if meta.version != BACKUP_VERSION {
        return Err(CommandError::new(
            "validation",
            format!(
                "backup-meta.version {} is not supported (this build expects {BACKUP_VERSION})",
                meta.version
            ),
        ));
    }

    // Override = user signalled they know about a collision.
    let raw_final = args
        .override_username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| meta.username.trim());
    let final_username = sanitize_username(raw_final)?;
    let original_username = meta.username.trim().to_string();
    if original_username.is_empty() {
        return Err(CommandError::new(
            "validation",
            "backup-meta.username is empty",
        ));
    }
    // Sanitise the original name too: a doctored archive could put
    // `..` or `/` in `meta.username` to escape the per-identity dir.
    sanitize_username(&original_username)?;

    let mut index = state.load_index().map_err(CommandError::from)?;
    if index
        .identities
        .iter()
        .any(|e| e.username == final_username)
    {
        return Err(CommandError::new(
            "validation",
            format!(
                "an identity named `{final_username}` already exists in the index. \
                 Pass override_username to import under a different name."
            ),
        ));
    }

    // Pass 2: extract.
    let dest_dir = state.identity_dir(&final_username);
    std::fs::create_dir_all(&dest_dir).map_err(CommandError::from)?;

    let dec = GzDecoder::new(raw_gz.as_slice());
    let mut archive = Archive::new(dec);
    let mut file_count: u32 = 0;
    let prefix_old = format!("identities/{original_username}/");
    let mut publish_secret_imported = false;

    for entry in archive
        .entries()
        .map_err(|e| CommandError::new("io", format!("opening archive entries failed: {e}")))?
    {
        let mut entry = entry
            .map_err(|e| CommandError::new("io", format!("reading archive entry failed: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| CommandError::new("io", format!("entry path invalid: {e}")))?
            .into_owned();
        let path_str = path.to_string_lossy().to_string();

        if path_str == META_FILE {
            // Already consumed in pass 1.
            continue;
        }
        if !path_str.starts_with(&prefix_old) {
            // Reject anything outside the expected per-identity tree.
            return Err(CommandError::new(
                "validation",
                format!("unexpected archive entry: {path_str}"),
            ));
        }
        // Defence in depth: reject path components that could escape.
        let suffix = &path_str[prefix_old.len()..];
        ensure_safe_relative(suffix)?;

        // `AppState::root` already points at the identities directory,
        // so strip the `identities/<username>/` namespace and join the
        // bare `<username>/<file>` against it.
        let dest_rel = format!("{final_username}/{suffix}");
        let dest_path = state.root.join(&dest_rel);
        if !dest_path.starts_with(&state.root) {
            return Err(CommandError::new(
                "validation",
                format!("rejecting unsafe extract path {}", dest_path.display()),
            ));
        }

        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).map_err(CommandError::from)?;
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| CommandError::new("io", format!("reading {path_str} failed: {e}")))?;
        std::fs::write(&dest_path, &bytes).map_err(CommandError::from)?;

        // tsig.key gets 0600 to match the desktop's own minting path.
        if suffix == TSIG_FILE {
            publish_secret_imported = true;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let perms = std::fs::Permissions::from_mode(0o600);
                std::fs::set_permissions(&dest_path, perms).map_err(CommandError::from)?;
            }
        }
        file_count += 1;
    }

    // Rewrite `publish.tsig_secret_path` to the new per-identity
    // tsig.key. Without this, override-rename and any absolute path
    // from the source installation would persist as broken pointers.
    let cfg_path = state.identity_config_path(&final_username);
    if cfg_path.is_file() {
        let raw = std::fs::read_to_string(&cfg_path).map_err(CommandError::from)?;
        let mut cfg: IdentityConfig = serde_yaml::from_str(&raw).map_err(CommandError::from)?;
        if let Some(publish) = cfg.publish.as_mut() {
            if !publish_secret_imported {
                return Err(CommandError::new(
                    "validation",
                    "imported config.yaml has a publish block but the archive did not include tsig.key",
                ));
            }
            publish.tsig_secret_path = state.identity_dir(&final_username).join(TSIG_FILE);
        }
        let yaml = serde_yaml::to_string(&cfg).map_err(|e| {
            CommandError::new("internal", format!("serialising rewritten config: {e}"))
        })?;
        std::fs::write(&cfg_path, yaml).map_err(CommandError::from)?;
    }

    // Register in the index but leave `active` untouched — the user
    // explicitly picks Switch on the Identities page when ready.
    index.identities.push(IdentityIndexEntry {
        username: final_username.clone(),
        domain: meta.domain.clone(),
    });
    state.save_index(&index).map_err(CommandError::from)?;

    Ok(ImportResult {
        username: final_username,
        domain: meta.domain,
        file_count,
    })
}

/// Read the `backup-meta.json` entry without extracting other files.
fn read_meta(raw_gz: &[u8]) -> Result<BackupMeta, CommandError> {
    let dec = GzDecoder::new(raw_gz);
    let mut archive = Archive::new(dec);
    for entry in archive
        .entries()
        .map_err(|e| CommandError::new("io", format!("opening archive failed: {e}")))?
    {
        let mut entry =
            entry.map_err(|e| CommandError::new("io", format!("reading entry failed: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| CommandError::new("io", format!("entry path invalid: {e}")))?
            .into_owned();
        if path.to_string_lossy() == META_FILE {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| CommandError::new("io", format!("reading meta failed: {e}")))?;
            return serde_json::from_slice::<BackupMeta>(&buf).map_err(|e| {
                CommandError::new("validation", format!("backup-meta.json is malformed: {e}"))
            });
        }
    }
    Err(CommandError::new(
        "validation",
        "archive is missing backup-meta.json — not a DMP backup",
    ))
}

/// Reject path components that could escape the destination dir.
/// Accepts only `Component::Normal`; anything else trips.
fn ensure_safe_relative(rel: &str) -> Result<(), CommandError> {
    if rel.is_empty() {
        return Err(CommandError::new(
            "validation",
            "archive contains an empty path suffix",
        ));
    }
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(_) => {}
            _ => {
                return Err(CommandError::new(
                    "validation",
                    format!("archive path `{rel}` contains an unsafe component"),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PublishConfig;
    use tempfile::TempDir;

    fn fresh_state() -> (AppState, TempDir) {
        let dir = TempDir::new().unwrap();
        (AppState::new(dir.path().to_path_buf()), dir)
    }

    /// Pre-populate an identity dir + index entry. `with_publish`
    /// writes a tsig.key + publish block; `with_inbox` writes a
    /// non-empty inbox.jsonl.
    fn seed_identity(
        state: &AppState,
        username: &str,
        domain: &str,
        with_publish: bool,
        with_inbox: bool,
    ) {
        let dir = state.identity_dir(username);
        std::fs::create_dir_all(&dir).unwrap();
        // sqlite is opaque to the export; arbitrary bytes are fine.
        std::fs::write(state.identity_db_path(username), b"sqlite-bytes").unwrap();
        let cfg = if with_publish {
            std::fs::write(dir.join(TSIG_FILE), b"base64:c2VjcmV0").unwrap();
            IdentityConfig {
                resolvers: Some(vec!["1.1.1.1".into()]),
                publish: Some(PublishConfig {
                    zone: "dmp.example.com".into(),
                    server: "192.0.2.1:53".into(),
                    tsig_key_name: "key1".into(),
                    tsig_algorithm: "hmac-sha256".into(),
                    tsig_secret_path: dir.join(TSIG_FILE),
                }),
                kdf_salt_base64: None,
                claim_via: None,
            }
        } else {
            IdentityConfig::default()
        };
        state.save_identity_config(username, &cfg).unwrap();
        if with_inbox {
            std::fs::write(
                dir.join(INBOX_FILE),
                b"{\"sender_signing_pk_hex\":\"aa\",\"msg_id_hex\":\"bb\",\
                  \"timestamp\":1,\"plaintext_utf8\":\"hi\",\
                  \"plaintext_bytes\":[104,105]}\n",
            )
            .unwrap();
            std::fs::write(dir.join(INBOX_READ_FILE), b"[\"bb\"]").unwrap();
        }
        let mut idx = state.load_index().unwrap();
        idx.identities.push(IdentityIndexEntry {
            username: username.to_string(),
            domain: domain.to_string(),
        });
        state.save_index(&idx).unwrap();
    }

    #[test]
    fn export_then_import_round_trip_into_new_username() {
        let (src_state, _src_root) = fresh_state();
        seed_identity(&src_state, "alice", "mesh.local", true, true);

        let out_dir = TempDir::new().unwrap();
        let archive = out_dir.path().join("alice");
        let exported = do_export(
            ExportArgs {
                username: "alice".into(),
                output_path: archive.clone(),
            },
            &src_state,
        )
        .unwrap();

        // Extension was auto-appended.
        assert!(
            exported
                .archive_path
                .to_string_lossy()
                .ends_with(".dmp-backup.tar.gz"),
            "got {}",
            exported.archive_path.display(),
        );
        assert!(exported.total_bytes > 0);
        // 1 meta + 1 cfg + 1 sqlite + 1 tsig + 1 inbox + 1 read = 6.
        assert_eq!(exported.file_count, 6);

        // Import into a clean state under a different username.
        let (dst_state, _dst_root) = fresh_state();
        let result = do_import(
            ImportArgs {
                archive_path: exported.archive_path.clone(),
                override_username: Some("alice2".into()),
            },
            &dst_state,
        )
        .unwrap();
        assert_eq!(result.username, "alice2");
        assert_eq!(result.domain, "mesh.local");

        // Files landed under the renamed identity.
        let dst_dir = dst_state.identity_dir("alice2");
        assert_eq!(
            std::fs::read(dst_dir.join(DB_FILE)).unwrap(),
            b"sqlite-bytes",
        );
        assert!(dst_dir.join(TSIG_FILE).is_file());
        assert!(dst_dir.join(INBOX_FILE).is_file());
        assert!(dst_dir.join(INBOX_READ_FILE).is_file());

        // The publish block's tsig_secret_path was rewritten to the
        // new identity dir.
        let cfg = dst_state.load_identity_config("alice2").unwrap();
        let publish = cfg.publish.unwrap();
        assert_eq!(publish.tsig_secret_path, dst_dir.join(TSIG_FILE));

        // Index has a single entry, NOT promoted to active.
        let idx = dst_state.load_index().unwrap();
        assert!(idx.active.is_none());
        assert_eq!(idx.identities.len(), 1);
        assert_eq!(idx.identities[0].username, "alice2");
    }

    #[test]
    fn import_rejects_unsupported_meta_version() {
        let (src_state, _src_root) = fresh_state();
        seed_identity(&src_state, "alice", "mesh.local", false, false);
        let out_dir = TempDir::new().unwrap();
        let exported = do_export(
            ExportArgs {
                username: "alice".into(),
                output_path: out_dir.path().join("alice"),
            },
            &src_state,
        )
        .unwrap();

        // Forge an unsupported version by rewriting the tar in memory
        // and swapping the meta entry's `version` field.
        let raw_gz = std::fs::read(&exported.archive_path).unwrap();
        let dec = GzDecoder::new(raw_gz.as_slice());
        let mut archive = Archive::new(dec);

        let mut new_buf: Vec<u8> = Vec::new();
        {
            let gz = GzEncoder::new(&mut new_buf, Compression::default());
            let mut tar = Builder::new(gz);
            for entry in archive.entries().unwrap() {
                let mut entry = entry.unwrap();
                let path = entry.path().unwrap().into_owned();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                if path.to_string_lossy() == META_FILE {
                    let mut meta: BackupMeta = serde_json::from_slice(&bytes).unwrap();
                    meta.version = 999;
                    bytes = serde_json::to_vec(&meta).unwrap();
                }
                append_bytes(&mut tar, &path.to_string_lossy(), &bytes, 0).unwrap();
            }
            tar.into_inner().unwrap().finish().unwrap();
        }
        let forged = out_dir.path().join("forged.dmp-backup.tar.gz");
        std::fs::write(&forged, &new_buf).unwrap();

        let (dst_state, _dst_root) = fresh_state();
        let err = do_import(
            ImportArgs {
                archive_path: forged,
                override_username: None,
            },
            &dst_state,
        )
        .unwrap_err();
        assert_eq!(err.kind, "validation");
        assert!(err.message.contains("version"), "got {err:?}");
    }

    #[test]
    fn override_rewrites_paths_and_publish_secret() {
        let (src_state, _src_root) = fresh_state();
        seed_identity(&src_state, "alice", "mesh.local", true, false);
        let out_dir = TempDir::new().unwrap();
        let exported = do_export(
            ExportArgs {
                username: "alice".into(),
                output_path: out_dir.path().join("alice"),
            },
            &src_state,
        )
        .unwrap();

        let (dst_state, _dst_root) = fresh_state();
        // Pre-seed the destination so an import without the override
        // would collide; this exercises the rename path end-to-end.
        seed_identity(&dst_state, "alice", "old.local", false, false);

        let result = do_import(
            ImportArgs {
                archive_path: exported.archive_path,
                override_username: Some("alice2".into()),
            },
            &dst_state,
        )
        .unwrap();

        // The renamed identity's tsig.key lives under the NEW dir, and
        // the persisted publish block reflects that.
        let new_dir = dst_state.identity_dir("alice2");
        assert!(new_dir.join(TSIG_FILE).is_file());
        let cfg = dst_state.load_identity_config(&result.username).unwrap();
        let publish = cfg.publish.unwrap();
        assert_eq!(publish.tsig_secret_path, new_dir.join(TSIG_FILE));

        // The pre-existing `alice` is untouched.
        let cfg_alice = dst_state.load_identity_config("alice").unwrap();
        assert!(cfg_alice.publish.is_none());
        assert!(!dst_state.identity_dir("alice").join(TSIG_FILE).exists());
    }

    #[test]
    fn import_collision_errors_without_override() {
        let (src_state, _src_root) = fresh_state();
        seed_identity(&src_state, "alice", "mesh.local", false, false);
        let out_dir = TempDir::new().unwrap();
        let exported = do_export(
            ExportArgs {
                username: "alice".into(),
                output_path: out_dir.path().join("alice"),
            },
            &src_state,
        )
        .unwrap();

        let (dst_state, _dst_root) = fresh_state();
        seed_identity(&dst_state, "alice", "mesh.local", false, false);

        let err = do_import(
            ImportArgs {
                archive_path: exported.archive_path,
                override_username: None,
            },
            &dst_state,
        )
        .unwrap_err();
        assert_eq!(err.kind, "validation");
        assert!(err.message.contains("already exists"), "got {err:?}");
    }

    #[test]
    fn ensure_safe_relative_rejects_traversal() {
        assert!(ensure_safe_relative("ok").is_ok());
        assert!(ensure_safe_relative("a/b").is_ok());
        assert!(ensure_safe_relative("../escape").is_err());
        assert!(ensure_safe_relative("/abs").is_err());
        assert!(ensure_safe_relative("").is_err());
    }

    #[test]
    fn ensure_archive_suffix_idempotent() {
        let p = PathBuf::from("/tmp/alice");
        let a = ensure_archive_suffix(p.clone());
        assert!(a.to_string_lossy().ends_with(".dmp-backup.tar.gz"));
        let b = ensure_archive_suffix(a.clone());
        assert_eq!(a, b);
    }
}
