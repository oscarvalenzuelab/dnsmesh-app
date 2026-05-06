//! Identity commands: init/unlock, show, publish, refresh prekeys, plus
//! the multi-identity list/switch surface.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use tauri::State;

use dnsmesh_client::{DmpClient, DmpClientConfig};

use crate::error::{CommandError, CommandResult};
use std::sync::Arc;

use crate::state::{
    ActiveClient, AppState, IdentityConfig, IdentityIndexEntry, PublishConfig, RefreshableReader,
    build_reader, build_writer, sanitize_username,
};

/// Length of the per-identity Argon2id salt we mint on first creation.
/// 16 bytes is well above dnsmesh-core's 8-byte floor.
const KDF_SALT_BYTES: usize = 16;

/// SDK-default Argon2id salt — what `DmpCrypto::from_passphrase(pass,
/// None)` uses. Legacy identities created before per-identity salts are
/// pinned to this value so the same passphrase keeps unlocking them.
const SDK_DEFAULT_KDF_SALT: &[u8] = dnsmesh_core::DEFAULT_ARGON2_SALT;

/// Read or generate the per-identity KDF salt. Persists the result so
/// subsequent unlocks don't need to re-detect.
fn ensure_kdf_salt(
    cfg: &mut IdentityConfig,
    state: &AppState,
    username: &str,
) -> CommandResult<Vec<u8>> {
    if let Some(b64) = cfg.kdf_salt_base64.as_deref() {
        let bytes = BASE64_STANDARD.decode(b64).map_err(|e| {
            CommandError::new(
                "io",
                format!("kdf_salt_base64 in {username}/config.yaml is malformed: {e}"),
            )
        })?;
        if bytes.len() < 8 {
            let actual = bytes.len();
            return Err(CommandError::new(
                "io",
                format!(
                    "kdf_salt_base64 in {username}/config.yaml decodes to {actual} bytes; need at least 8"
                ),
            ));
        }
        return Ok(bytes);
    }
    let db_path = state.identity_db_path(username);
    let bytes = if db_path.exists() {
        // Legacy identity, no salt recorded: pin to the SDK default so
        // the existing passphrase still unlocks.
        SDK_DEFAULT_KDF_SALT.to_vec()
    } else {
        // Fresh identity: mint a random per-identity salt.
        let mut bytes = vec![0u8; KDF_SALT_BYTES];
        rand::thread_rng().fill_bytes(&mut bytes);
        bytes
    };
    cfg.kdf_salt_base64 = Some(BASE64_STANDARD.encode(&bytes));
    state
        .save_identity_config(username, cfg)
        .map_err(CommandError::from)?;
    Ok(bytes)
}

/// Snapshot of the unlocked identity returned to the UI. Pubkeys are
/// hex-encoded; `publish_configured` lets the UI disable Publish when
/// no TSIG block is wired.
#[derive(Debug, Clone, Serialize)]
pub struct IdentityInfo {
    pub username: String,
    pub domain: String,
    pub user_id_hex: String,
    pub x25519_public_key_hex: String,
    pub ed25519_signing_public_key_hex: String,
    pub publish_configured: bool,
}

impl IdentityInfo {
    fn from_active(active: &ActiveClient) -> Self {
        Self {
            username: active.username.clone(),
            domain: active.domain.clone(),
            user_id_hex: hex::encode(active.client.user_id()),
            x25519_public_key_hex: active.client.x25519_public_key_hex(),
            ed25519_signing_public_key_hex: active.client.ed25519_signing_public_key_hex(),
            publish_configured: active.publish_configured,
        }
    }
}

/// Args for [`init_or_unlock`].
///
/// `domain` is required the first time we see a username; on subsequent
/// unlocks the on-disk index is the source of truth.
#[derive(Debug, Clone, Deserialize)]
pub struct InitOrUnlockArgs {
    pub username: String,
    pub passphrase: String,
    /// Mesh zone. Required for first-time creation.
    #[serde(default)]
    pub domain: Option<String>,
}

/// Open or re-open an identity, replacing whatever was previously active.
///
/// First-time use: `domain` must be set. Creates the per-identity
/// directory + sqlite db, opens a `DmpClient`, and registers the
/// identity in the index.
///
/// Re-open: `domain` may be omitted; the index supplies it.
#[tauri::command]
pub async fn init_or_unlock(
    args: InitOrUnlockArgs,
    state: State<'_, AppState>,
) -> CommandResult<IdentityInfo> {
    let username = sanitize_username(&args.username)?;
    if args.passphrase.is_empty() {
        return Err(CommandError::new(
            "validation",
            "passphrase must not be empty",
        ));
    }

    let mut index = state.load_index().map_err(CommandError::from)?;
    let existing = index.identities.iter().find(|e| e.username == username);
    let domain = match (existing, args.domain.as_deref()) {
        (Some(entry), Some(d)) if d.trim() != entry.domain => {
            return Err(CommandError::new(
                "validation",
                format!(
                    "identity {} is registered under domain {} but {} was supplied",
                    username, entry.domain, d
                ),
            ));
        }
        (Some(entry), _) => entry.domain.clone(),
        (None, Some(d)) if !d.trim().is_empty() => d.trim().to_string(),
        (None, _) => {
            return Err(CommandError::new(
                "validation",
                "domain is required to create a new identity",
            ));
        }
    };

    // Materialise the per-identity directory before we open sqlite.
    let dir = state.identity_dir(&username);
    std::fs::create_dir_all(&dir).map_err(CommandError::from)?;
    let db_path = state.identity_db_path(&username);
    let mut cfg = state
        .load_identity_config(&username)
        .map_err(CommandError::from)?;

    let kdf_salt = ensure_kdf_salt(&mut cfg, &state, &username)?;

    let inner_reader = build_reader(cfg.resolvers.as_deref()).map_err(CommandError::from)?;
    let refreshable_reader = Arc::new(RefreshableReader::new(inner_reader));
    let (writer, publish_configured) =
        build_writer(cfg.publish.as_ref()).map_err(CommandError::from)?;

    let client_cfg = DmpClientConfig {
        username: username.clone(),
        passphrase: args.passphrase,
        domain: domain.clone(),
        kdf_salt: Some(kdf_salt),
        db_path: Some(db_path),
        writer,
        reader: refreshable_reader.clone(),
        // Rotation-chain walk is opt-in pending external audit of the
        // wire format; mirrors the SDK and CLI defaults.
        rotation_chain_enabled: false,
    };
    let client = DmpClient::new(client_cfg).await?;

    let active = ActiveClient {
        client,
        username: username.clone(),
        domain: domain.clone(),
        publish_configured,
        refreshable_reader,
    };

    if !index.identities.iter().any(|e| e.username == username) {
        index.identities.push(IdentityIndexEntry {
            username: username.clone(),
            domain: domain.clone(),
        });
    }
    index.active = Some(username.clone());
    state.save_index(&index).map_err(CommandError::from)?;

    let info = IdentityInfo::from_active(&active);
    *state.active.write().await = Some(active);
    Ok(info)
}

/// Return information about the currently-active identity, if any.
#[tauri::command]
pub async fn get_identity_info(state: State<'_, AppState>) -> CommandResult<Option<IdentityInfo>> {
    let guard = state.active.read().await;
    Ok(guard.as_ref().map(IdentityInfo::from_active))
}

/// Publish the active identity's signed record to DNS.
#[tauri::command]
pub async fn publish_identity(state: State<'_, AppState>) -> CommandResult<()> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    if !active.publish_configured {
        return Err(CommandError::new(
            "publish_unconfigured",
            "no publish (TSIG) block configured for this identity",
        ));
    }
    active.client.publish_identity().await?;
    Ok(())
}

/// Generate + publish a fresh pool of one-time prekeys.
#[derive(Debug, Clone, Deserialize)]
pub struct RefreshPrekeysArgs {
    /// How many prekeys to generate.
    #[serde(default = "default_prekey_count")]
    pub count: u32,
    /// TTL per published prekey TXT, in seconds. Zero = SDK default (24h).
    #[serde(default)]
    pub ttl_seconds: u64,
}

fn default_prekey_count() -> u32 {
    50
}

/// Result of [`refresh_prekeys`] — number of prekeys actually accepted
/// by the writer.
#[derive(Debug, Clone, Serialize)]
pub struct RefreshPrekeysResult {
    pub published: u32,
}

#[tauri::command]
pub async fn refresh_prekeys(
    args: RefreshPrekeysArgs,
    state: State<'_, AppState>,
) -> CommandResult<RefreshPrekeysResult> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    if !active.publish_configured {
        return Err(CommandError::new(
            "publish_unconfigured",
            "no publish (TSIG) block configured for this identity",
        ));
    }
    let published = active
        .client
        .refresh_prekeys(args.count, args.ttl_seconds)
        .await?;
    Ok(RefreshPrekeysResult { published })
}

/// One row in [`list_identities`].
#[derive(Debug, Clone, Serialize)]
pub struct IdentitySummary {
    pub username: String,
    pub domain: String,
    /// True iff this identity is currently unlocked in memory. NOT a
    /// function of the on-disk `index.active` hint, which survives a
    /// restart even when no client is unlocked.
    pub is_active: bool,
}

#[tauri::command]
pub async fn list_identities(state: State<'_, AppState>) -> CommandResult<Vec<IdentitySummary>> {
    let index = state.load_index().map_err(CommandError::from)?;
    // Source of truth for "active" is the in-memory client. The index's
    // `active` field is only a default-to hint for next launch.
    let active = state
        .active
        .read()
        .await
        .as_ref()
        .map(|c| c.username.clone());
    Ok(index
        .identities
        .iter()
        .map(|entry| IdentitySummary {
            username: entry.username.clone(),
            domain: entry.domain.clone(),
            is_active: Some(&entry.username) == active.as_ref(),
        })
        .collect())
}

/// Drop the active client (lock). Also clears the on-disk
/// `index.active` pointer so a subsequent `list_identities` returns no
/// row flagged active.
#[tauri::command]
pub async fn lock_identity(state: State<'_, AppState>) -> CommandResult<()> {
    *state.active.write().await = None;
    let mut index = state.load_index().map_err(CommandError::from)?;
    if index.active.is_some() {
        index.active = None;
        state.save_index(&index).map_err(CommandError::from)?;
    }
    Ok(())
}

/// Snapshot of the per-identity config surfaced to the Settings page.
#[derive(Debug, Clone, Serialize)]
pub struct IdentityConfigView {
    pub username: String,
    pub publish: Option<PublishConfigView>,
    pub resolvers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishConfigView {
    pub zone: String,
    pub server: String,
    pub tsig_key_name: String,
    pub tsig_algorithm: String,
    pub tsig_secret_path: String,
}

#[tauri::command]
pub async fn get_identity_config(
    username: String,
    state: State<'_, AppState>,
) -> CommandResult<IdentityConfigView> {
    let username = sanitize_username(&username)?;
    let cfg = state
        .load_identity_config(&username)
        .map_err(CommandError::from)?;
    Ok(IdentityConfigView {
        username,
        publish: cfg.publish.map(|p| PublishConfigView {
            zone: p.zone,
            server: p.server,
            tsig_key_name: p.tsig_key_name,
            tsig_algorithm: p.tsig_algorithm,
            tsig_secret_path: p.tsig_secret_path.to_string_lossy().into_owned(),
        }),
        resolvers: cfg.resolvers.unwrap_or_default(),
    })
}

/// Args for [`update_publish_config`].
#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePublishArgs {
    pub username: String,
    /// Setting `publish` to `None` removes the block entirely (read-only mode).
    #[serde(default)]
    pub publish: Option<PublishConfigInput>,
    #[serde(default)]
    pub resolvers: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublishConfigInput {
    pub zone: String,
    pub server: String,
    pub tsig_key_name: String,
    #[serde(default = "default_tsig_algo")]
    pub tsig_algorithm: String,
    /// Path to the on-disk TSIG secret. If empty, `tsig_secret_base64`
    /// must be set: the host writes it to `<identity-dir>/tsig.key`
    /// (mode 0600 on Unix) and uses that path. Used by the
    /// `register_tsig` flow where the secret is in memory.
    #[serde(default)]
    pub tsig_secret_path: String,
    /// Raw TSIG secret as base64 (no `base64:` prefix). When present,
    /// the host materialises it as `<identity-dir>/tsig.key`.
    #[serde(default)]
    pub tsig_secret_base64: Option<String>,
}

fn default_tsig_algo() -> String {
    "hmac-sha256".to_string()
}

/// File name used for the auto-materialised TSIG secret.
const TSIG_SECRET_FILE: &str = "tsig.key";

/// Decide the on-disk `tsig_secret_path`, materialising the secret file
/// if the caller passed raw bytes via `tsig_secret_base64`. Refuses if
/// both fields are empty.
fn materialise_publish_block(
    state: &AppState,
    username: &str,
    input: PublishConfigInput,
) -> Result<PublishConfig, CommandError> {
    let path_str = input.tsig_secret_path.trim();
    let secret_b64 = input
        .tsig_secret_base64
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let final_path = match (path_str.is_empty(), secret_b64) {
        (false, None) => std::path::PathBuf::from(path_str),
        (_, Some(b64)) => {
            // Validate the base64 before touching the filesystem so a
            // bad payload doesn't leave a half-written secret behind.
            use base64::Engine as _;
            use base64::engine::general_purpose::STANDARD as BASE64;
            BASE64.decode(b64).map_err(|e| {
                CommandError::new(
                    "validation",
                    format!("tsig_secret_base64 is not valid base64: {e}"),
                )
            })?;
            let dir = state.identity_dir(username);
            std::fs::create_dir_all(&dir).map_err(CommandError::from)?;
            let path = dir.join(TSIG_SECRET_FILE);
            let body = format!("base64:{b64}");
            write_secret_file(&path, body.as_bytes())?;
            path
        }
        (true, None) => {
            return Err(CommandError::new(
                "validation",
                "publish.tsig_secret_path or publish.tsig_secret_base64 must be set",
            ));
        }
    };

    Ok(PublishConfig {
        zone: input.zone,
        server: input.server,
        tsig_key_name: input.tsig_key_name,
        tsig_algorithm: input.tsig_algorithm,
        tsig_secret_path: final_path,
    })
}

/// Write `bytes` to `path` and chmod it to 0600 on Unix. The TSIG
/// secret authorises DNS UPDATE on behalf of the identity, so it's
/// treated like an SSH private key.
fn write_secret_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), CommandError> {
    std::fs::write(path, bytes).map_err(CommandError::from)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms).map_err(CommandError::from)?;
    }
    Ok(())
}

/// Persist publish/resolver settings for an identity. If the identity
/// is the active one, the in-memory client is dropped so the next
/// unlock picks up the new writer.
#[tauri::command]
pub async fn update_publish_config(
    args: UpdatePublishArgs,
    state: State<'_, AppState>,
) -> CommandResult<IdentityConfigView> {
    let username = sanitize_username(&args.username)?;

    let publish = match args.publish {
        Some(p) => Some(materialise_publish_block(&state, &username, p)?),
        None => None,
    };
    // Preserve the existing kdf_salt_base64 — clobbering it would lock
    // the user out (same passphrase would no longer derive the same key).
    let prior = state
        .load_identity_config(&username)
        .map_err(CommandError::from)?;
    let cfg = IdentityConfig {
        resolvers: args.resolvers,
        publish,
        kdf_salt_base64: prior.kdf_salt_base64,
    };
    state
        .save_identity_config(&username, &cfg)
        .map_err(CommandError::from)?;

    // The active client holds writer/reader behind Arc<dyn …> and we
    // can't swap them at runtime, so drop it and let the UI re-prompt
    // for the passphrase before the next unlock.
    let mut guard = state.active.write().await;
    if let Some(active) = guard.as_ref()
        && active.username == username
    {
        *guard = None;
    }
    drop(guard);

    let view = IdentityConfigView {
        username: username.clone(),
        publish: cfg.publish.map(|p| PublishConfigView {
            zone: p.zone,
            server: p.server,
            tsig_key_name: p.tsig_key_name,
            tsig_algorithm: p.tsig_algorithm,
            tsig_secret_path: p.tsig_secret_path.to_string_lossy().into_owned(),
        }),
        resolvers: cfg.resolvers.unwrap_or_default(),
    };
    Ok(view)
}

/// Check whether the active identity has a discoverable record in DNS.
///
/// `Ok` → `Published`, `NoRecordFound` → `NotPublished`, anything else
/// → `Unknown(reason)`. The UI distinguishes NXDOMAIN from transport
/// failure so it only hides the Publish button on the former.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PublishedStatus {
    /// Record is live in DNS.
    Published,
    /// Record is not (yet) in DNS, or has been retracted.
    NotPublished,
    /// Lookup failed for some other reason; UI keeps Publish available.
    Unknown { reason: String },
}

#[tauri::command]
pub async fn is_identity_published(state: State<'_, AppState>) -> CommandResult<PublishedStatus> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    let self_addr = format!("{}@{}", active.username, active.domain);
    match active.client.fetch_identity(&self_addr).await {
        Ok(_) => Ok(PublishedStatus::Published),
        Err(dnsmesh_client::ClientError::NoRecordFound { .. }) => Ok(PublishedStatus::NotPublished),
        Err(e) => Ok(PublishedStatus::Unknown {
            reason: e.to_string(),
        }),
    }
}

/// Switch the active identity. The supplied passphrase unlocks the
/// requested identity; the previous active client is dropped.
#[derive(Debug, Clone, Deserialize)]
pub struct SwitchIdentityArgs {
    pub username: String,
    pub passphrase: String,
}

#[tauri::command]
pub async fn switch_identity(
    args: SwitchIdentityArgs,
    state: State<'_, AppState>,
) -> CommandResult<IdentityInfo> {
    init_or_unlock(
        InitOrUnlockArgs {
            username: args.username,
            passphrase: args.passphrase,
            domain: None,
        },
        state,
    )
    .await
}

/// Helper for tests: construct a temporary [`AppState`] backed by a
/// fresh tempdir.
#[cfg(test)]
pub(crate) fn make_test_state() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().unwrap();
    (AppState::new(dir.path().to_path_buf()), dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_index_entry(state: &AppState, username: &str, domain: &str) {
        let mut idx = state.load_index().unwrap();
        idx.identities.push(IdentityIndexEntry {
            username: username.to_string(),
            domain: domain.to_string(),
        });
        state.save_index(&idx).unwrap();
    }

    #[test]
    fn list_identities_uses_on_disk_index() {
        let (state, _dir) = make_test_state();
        dummy_index_entry(&state, "alice", "mesh.local");
        dummy_index_entry(&state, "bob", "mesh.local");
        let mut idx = state.load_index().unwrap();
        idx.active = Some("bob".into());
        state.save_index(&idx).unwrap();

        // Read directly via load_index since we can't easily make a
        // tauri::State in unit tests; the command is a thin wrapper.
        let loaded = state.load_index().unwrap();
        assert_eq!(loaded.identities.len(), 2);
        assert_eq!(loaded.active.as_deref(), Some("bob"));
    }

    /// Locks the wire shape of [`PublishedStatus`]; the UI switches on
    /// `status` to decide between the Publish button and the badge.
    #[test]
    fn published_status_serializes_with_status_tag() {
        let cases = [
            (PublishedStatus::Published, "{\"status\":\"published\"}"),
            (
                PublishedStatus::NotPublished,
                "{\"status\":\"not_published\"}",
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        }
        let unknown = PublishedStatus::Unknown {
            reason: "transport timeout".into(),
        };
        let s = serde_json::to_string(&unknown).unwrap();
        assert!(s.contains("\"status\":\"unknown\""), "got {s}");
        assert!(s.contains("\"reason\":\"transport timeout\""), "got {s}");
    }

    /// Mirrors `lock_identity`'s on-disk side-effect without spinning
    /// up `tauri::State`.
    #[test]
    fn lock_clears_index_active_flag() {
        let (state, _dir) = make_test_state();
        dummy_index_entry(&state, "alice", "mesh.local");
        let mut idx = state.load_index().unwrap();
        idx.active = Some("alice".into());
        state.save_index(&idx).unwrap();

        // Simulate the on-disk side-effect of `lock_identity`.
        let mut idx = state.load_index().unwrap();
        idx.active = None;
        state.save_index(&idx).unwrap();

        let loaded = state.load_index().unwrap();
        let summaries: Vec<IdentitySummary> = loaded
            .identities
            .iter()
            .map(|entry| IdentitySummary {
                username: entry.username.clone(),
                domain: entry.domain.clone(),
                is_active: Some(entry.username.as_str()) == loaded.active.as_deref(),
            })
            .collect();
        assert!(summaries.iter().all(|s| !s.is_active));
    }
}
