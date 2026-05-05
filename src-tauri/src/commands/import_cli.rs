//! Import an identity from the dnsmesh CLI's flat layout.
//!
//! CLI layout:
//!
//! ```text
//! ~/.dmp/
//! ├── config.yaml      # username, domain, optional publish, optional kdf_salt
//! ├── dmp-rs.sqlite
//! └── tsig.key         # if publish was configured
//! ```
//!
//! Desktop layout:
//!
//! ```text
//! ~/.dmp/identities/
//! ├── index.yaml
//! └── <username>/
//!     ├── config.yaml
//!     ├── dmp-rs.sqlite
//!     └── tsig.key
//! ```
//!
//! [`import_from_cli`] copies the CLI's files into a per-identity dir,
//! rewrites `publish.tsig_secret_path`, and registers the identity in
//! the desktop's index, ready for `init_or_unlock`.

use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::{CommandError, CommandResult};
use crate::state::{
    AppState, IdentityConfig, IdentityIndexEntry, PublishConfig, sanitize_username,
};

/// On-disk shape of the CLI's `config.yaml`. Only the slice the
/// desktop consumes is deserialised; unknown fields (e.g.
/// `cloudflare:`) are ignored.
#[derive(Debug, Clone, Deserialize)]
struct CliConfigFile {
    username: String,
    domain: String,
    #[serde(default)]
    resolvers: Option<Vec<String>>,
    #[serde(default)]
    publish: Option<CliPublishConfig>,
    /// Hex-encoded Argon2id salt. When present we re-encode to base64
    /// for `kdf_salt_base64`. Absent ⇒ `ensure_kdf_salt` falls back to
    /// the SDK default at unlock.
    #[serde(default)]
    kdf_salt: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CliPublishConfig {
    zone: String,
    server: String,
    tsig_key_name: String,
    #[serde(default = "default_tsig_algorithm")]
    tsig_algorithm: String,
    /// Always rewritten on import; kept here so the YAML parses.
    #[allow(dead_code)]
    tsig_secret_path: PathBuf,
}

fn default_tsig_algorithm() -> String {
    "hmac-sha256".to_string()
}

/// File names used by the CLI flat layout.
const CLI_CONFIG_FILE: &str = "config.yaml";
const CLI_DB_FILE: &str = "dmp-rs.sqlite";
const CLI_TSIG_FILE: &str = "tsig.key";

/// Args for [`import_from_cli`].
#[derive(Debug, Clone, Deserialize)]
pub struct ImportFromCliArgs {
    /// Source dir holding the CLI's flat layout. `None` resolves to
    /// `$DMP_CONFIG_HOME` then `$HOME/.dmp`.
    #[serde(default)]
    pub source_dir: Option<PathBuf>,
    /// Override the username on collision; `None` + collision is an
    /// error so the frontend can prompt.
    #[serde(default)]
    pub override_username: Option<String>,
}

/// Result of a successful import.
#[derive(Debug, Clone, Serialize)]
pub struct ImportFromCliResult {
    pub username: String,
    pub domain: String,
    /// True iff a `tsig.key` file was carried over.
    pub publish_imported: bool,
}

/// Source dir resolution: `$DMP_CONFIG_HOME` then `$HOME/.dmp`.
/// Mirrors the CLI's `default_config_home`.
fn default_cli_source() -> Result<PathBuf, CommandError> {
    if let Some(p) = std::env::var_os("DMP_CONFIG_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        CommandError::new(
            "validation",
            "HOME is not set; supply source_dir explicitly",
        )
    })?;
    Ok(PathBuf::from(home).join(".dmp"))
}

/// Translate a CLI publish block, rewriting `tsig_secret_path` to the
/// per-identity `tsig.key` we just copied.
fn rewrite_publish(cli: CliPublishConfig, dest_tsig: PathBuf) -> PublishConfig {
    PublishConfig {
        zone: cli.zone,
        server: cli.server,
        tsig_key_name: cli.tsig_key_name,
        tsig_algorithm: cli.tsig_algorithm,
        tsig_secret_path: dest_tsig,
    }
}

/// Import an identity from the dnsmesh CLI's flat layout.
///
/// Resolves the source dir, validates `config.yaml`, picks the final
/// username (or override on collision), copies `dmp-rs.sqlite` and
/// `tsig.key`, persists a rewritten [`IdentityConfig`], and adds the
/// identity to the index. `active` is left untouched.
#[tauri::command]
pub async fn import_from_cli(
    args: ImportFromCliArgs,
    state: State<'_, AppState>,
) -> CommandResult<ImportFromCliResult> {
    do_import(args, &state)
}

/// Inner impl, factored out so tests can drive it without a
/// `tauri::State`. Linear top-to-bottom on purpose; the
/// `too_many_lines` lint is silenced.
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn do_import(args: ImportFromCliArgs, state: &AppState) -> CommandResult<ImportFromCliResult> {
    let source_dir = match args.source_dir {
        Some(p) => p,
        None => default_cli_source()?,
    };
    if !source_dir.is_dir() {
        return Err(CommandError::new(
            "validation",
            format!("source_dir {} is not a directory", source_dir.display()),
        ));
    }

    let cli_config_path = source_dir.join(CLI_CONFIG_FILE);
    if !cli_config_path.is_file() {
        return Err(CommandError::new(
            "validation",
            format!(
                "no config.yaml at {} — does the CLI live there?",
                cli_config_path.display()
            ),
        ));
    }
    let raw = std::fs::read_to_string(&cli_config_path).map_err(CommandError::from)?;
    let cli: CliConfigFile = serde_yaml::from_str(&raw).map_err(CommandError::from)?;

    if cli.username.trim().is_empty() {
        return Err(CommandError::new(
            "validation",
            "imported config.yaml has empty `username`",
        ));
    }
    if cli.domain.trim().is_empty() {
        return Err(CommandError::new(
            "validation",
            "imported config.yaml has empty `domain`",
        ));
    }

    let raw_final = args
        .override_username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cli.username.trim());
    let final_username = sanitize_username(raw_final)?;

    // Override = user signalled they know about the collision.
    let mut index = state.load_index().map_err(CommandError::from)?;
    let collision = index
        .identities
        .iter()
        .any(|e| e.username == final_username);
    if collision {
        return Err(CommandError::new(
            "validation",
            format!(
                "an identity named `{final_username}` already exists in the index. \
                 Pass override_username to import under a different name."
            ),
        ));
    }

    let dest_dir = state.identity_dir(&final_username);
    std::fs::create_dir_all(&dest_dir).map_err(CommandError::from)?;

    let src_db = source_dir.join(CLI_DB_FILE);
    if !src_db.is_file() {
        return Err(CommandError::new(
            "validation",
            format!(
                "expected {} to exist alongside config.yaml",
                src_db.display()
            ),
        ));
    }
    let dest_db = dest_dir.join(CLI_DB_FILE);
    std::fs::copy(&src_db, &dest_db).map_err(CommandError::from)?;

    // Optional TSIG secret. Copy byte-for-byte so any wrapping format
    // round-trips into `read_tsig_secret`.
    let mut publish_imported = false;
    let dest_tsig = dest_dir.join(CLI_TSIG_FILE);
    let src_tsig = source_dir.join(CLI_TSIG_FILE);
    if src_tsig.is_file() {
        std::fs::copy(&src_tsig, &dest_tsig).map_err(CommandError::from)?;
        publish_imported = true;
        // chmod 0600 on Unix to match the desktop's own minting path.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&dest_tsig, perms).map_err(CommandError::from)?;
        }
    }

    let publish = match cli.publish {
        Some(cli_publish) if publish_imported => Some(rewrite_publish(cli_publish, dest_tsig)),
        Some(_) => {
            // Publish block without a tsig.key alongside is a corrupt
            // import; refuse rather than half-configure the identity.
            return Err(CommandError::new(
                "validation",
                format!(
                    "config.yaml has a publish block but no tsig.key was found at {}",
                    src_tsig.display()
                ),
            ));
        }
        None => None,
    };

    // Re-encode the CLI's hex `kdf_salt` as base64. Treat blank
    // strings as absent so a stray `kdf_salt: ""` doesn't trip
    // validation.
    let kdf_salt_base64 = match cli
        .kdf_salt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(hex_salt) => {
            let bytes = hex::decode(hex_salt).map_err(|e| {
                CommandError::new(
                    "validation",
                    format!("imported config.yaml has invalid kdf_salt hex: {e}"),
                )
            })?;
            if bytes.len() < 8 {
                return Err(CommandError::new(
                    "validation",
                    format!(
                        "imported config.yaml's kdf_salt decodes to {} bytes; need at least 8",
                        bytes.len()
                    ),
                ));
            }
            Some(BASE64.encode(&bytes))
        }
        None => None,
    };

    let cfg = IdentityConfig {
        resolvers: cli.resolvers,
        publish,
        kdf_salt_base64,
    };
    state
        .save_identity_config(&final_username, &cfg)
        .map_err(CommandError::from)?;

    // Register in the index but leave `active` untouched — the user
    // explicitly switches via the Identities page.
    index.identities.push(IdentityIndexEntry {
        username: final_username.clone(),
        domain: cli.domain.trim().to_string(),
    });
    state.save_index(&index).map_err(CommandError::from)?;

    Ok(ImportFromCliResult {
        username: final_username,
        domain: cli.domain.trim().to_string(),
        publish_imported,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_state() -> (AppState, TempDir) {
        let dir = TempDir::new().unwrap();
        (AppState::new(dir.path().to_path_buf()), dir)
    }

    /// Write a CLI flat layout under `dir`.
    fn write_cli_layout(
        dir: &std::path::Path,
        username: &str,
        domain: &str,
        with_publish: bool,
        kdf_salt_hex: Option<&str>,
    ) {
        use std::fmt::Write as _;
        std::fs::create_dir_all(dir).unwrap();
        let mut yaml = format!("username: {username}\ndomain: {domain}\n");
        if with_publish {
            yaml.push_str(
                "publish:\n  zone: dmp.example.com\n  server: 192.0.2.1:53\n  \
                 tsig_key_name: cli-key\n  tsig_algorithm: hmac-sha256\n  \
                 tsig_secret_path: tsig.key\n",
            );
        }
        if let Some(salt) = kdf_salt_hex {
            writeln!(yaml, "kdf_salt: {salt}").unwrap();
        }
        std::fs::write(dir.join("config.yaml"), yaml).unwrap();
        // sqlite is opaque to the import.
        std::fs::write(dir.join("dmp-rs.sqlite"), b"sqlite-bytes").unwrap();
        if with_publish {
            std::fs::write(dir.join("tsig.key"), b"base64:dGVzdC1zZWNyZXQ=").unwrap();
        }
    }

    #[test]
    fn import_round_trips_into_per_identity_layout() {
        let (state, _root) = fresh_state();
        let cli_dir = TempDir::new().unwrap();
        write_cli_layout(cli_dir.path(), "alice", "mesh.local", false, None);

        let result = do_import(
            ImportFromCliArgs {
                source_dir: Some(cli_dir.path().to_path_buf()),
                override_username: None,
            },
            &state,
        )
        .unwrap();

        assert_eq!(result.username, "alice");
        assert_eq!(result.domain, "mesh.local");
        assert!(!result.publish_imported);

        // The per-identity dir should now hold a config + sqlite copy.
        assert!(state.identity_config_path("alice").exists());
        assert!(state.identity_db_path("alice").exists());
        let copied = std::fs::read(state.identity_db_path("alice")).unwrap();
        assert_eq!(copied, b"sqlite-bytes");

        // Index now includes the new entry, but `active` is left
        // untouched (None on a fresh state).
        let index = state.load_index().unwrap();
        assert!(index.active.is_none());
        assert_eq!(index.identities.len(), 1);
        assert_eq!(index.identities[0].username, "alice");
        assert_eq!(index.identities[0].domain, "mesh.local");
    }

    #[test]
    fn import_collision_errors_without_override() {
        let (state, _root) = fresh_state();

        // Pre-seed `alice` so the import collides.
        let mut idx = state.load_index().unwrap();
        idx.identities.push(IdentityIndexEntry {
            username: "alice".into(),
            domain: "old.local".into(),
        });
        state.save_index(&idx).unwrap();

        let cli_dir = TempDir::new().unwrap();
        write_cli_layout(cli_dir.path(), "alice", "mesh.local", false, None);

        let err = do_import(
            ImportFromCliArgs {
                source_dir: Some(cli_dir.path().to_path_buf()),
                override_username: None,
            },
            &state,
        )
        .unwrap_err();
        assert_eq!(err.kind, "validation");
        assert!(err.message.contains("already exists"), "got {err:?}");
    }

    #[test]
    fn import_collision_succeeds_with_override() {
        let (state, _root) = fresh_state();

        let mut idx = state.load_index().unwrap();
        idx.identities.push(IdentityIndexEntry {
            username: "alice".into(),
            domain: "old.local".into(),
        });
        state.save_index(&idx).unwrap();

        let cli_dir = TempDir::new().unwrap();
        write_cli_layout(cli_dir.path(), "alice", "mesh.local", false, None);

        let result = do_import(
            ImportFromCliArgs {
                source_dir: Some(cli_dir.path().to_path_buf()),
                override_username: Some("alice2".into()),
            },
            &state,
        )
        .unwrap();
        assert_eq!(result.username, "alice2");

        let index = state.load_index().unwrap();
        let names: Vec<&str> = index
            .identities
            .iter()
            .map(|e| e.username.as_str())
            .collect();
        assert_eq!(names, vec!["alice", "alice2"]);
    }

    #[test]
    fn import_copies_tsig_and_rewrites_publish_path() {
        let (state, _root) = fresh_state();
        let cli_dir = TempDir::new().unwrap();
        write_cli_layout(cli_dir.path(), "alice", "mesh.local", true, None);

        let result = do_import(
            ImportFromCliArgs {
                source_dir: Some(cli_dir.path().to_path_buf()),
                override_username: None,
            },
            &state,
        )
        .unwrap();
        assert!(result.publish_imported);

        // tsig.key copied into the per-identity dir.
        let dest_tsig = state.identity_dir("alice").join("tsig.key");
        assert!(dest_tsig.is_file());
        let body = std::fs::read(&dest_tsig).unwrap();
        assert_eq!(body, b"base64:dGVzdC1zZWNyZXQ=");

        // The persisted config's publish.tsig_secret_path now points at
        // the per-identity location, not the CLI's source path.
        let cfg = state.load_identity_config("alice").unwrap();
        let publish = cfg.publish.expect("publish block was not persisted");
        assert_eq!(publish.tsig_secret_path, dest_tsig);
        assert_eq!(publish.tsig_key_name, "cli-key");
        assert_eq!(publish.zone, "dmp.example.com");
    }

    #[test]
    fn import_rejects_publish_without_tsig_file() {
        let (state, _root) = fresh_state();
        let cli_dir = TempDir::new().unwrap();
        // publish block present, but tsig.key removed after writing.
        write_cli_layout(cli_dir.path(), "alice", "mesh.local", true, None);
        std::fs::remove_file(cli_dir.path().join("tsig.key")).unwrap();

        let err = do_import(
            ImportFromCliArgs {
                source_dir: Some(cli_dir.path().to_path_buf()),
                override_username: None,
            },
            &state,
        )
        .unwrap_err();
        assert_eq!(err.kind, "validation");
        assert!(err.message.contains("tsig.key"), "got {err:?}");
    }

    #[test]
    fn import_preserves_kdf_salt_as_base64() {
        let (state, _root) = fresh_state();
        let cli_dir = TempDir::new().unwrap();
        // 8 raw bytes — `ensure_kdf_salt`'s minimum.
        let salt_hex = "0011223344556677";
        write_cli_layout(cli_dir.path(), "alice", "mesh.local", false, Some(salt_hex));

        do_import(
            ImportFromCliArgs {
                source_dir: Some(cli_dir.path().to_path_buf()),
                override_username: None,
            },
            &state,
        )
        .unwrap();

        let cfg = state.load_identity_config("alice").unwrap();
        let b64 = cfg
            .kdf_salt_base64
            .expect("kdf_salt should have been re-encoded");
        let decoded = BASE64.decode(b64).unwrap();
        assert_eq!(decoded, hex::decode(salt_hex).unwrap());
    }

    /// Persisted config must be ready for `init_or_unlock`. We don't
    /// call `DmpClient::new` here, but every field it inspects is
    /// covered.
    #[test]
    fn import_leaves_destination_ready_for_unlock() {
        let (state, _root) = fresh_state();
        let cli_dir = TempDir::new().unwrap();
        write_cli_layout(cli_dir.path(), "alice", "mesh.local", true, None);
        // Append a resolvers entry so that path is exercised too.
        let cfg_path = cli_dir.path().join("config.yaml");
        let mut yaml = std::fs::read_to_string(&cfg_path).unwrap();
        yaml.push_str("resolvers:\n  - 1.1.1.1\n");
        std::fs::write(&cfg_path, yaml).unwrap();

        do_import(
            ImportFromCliArgs {
                source_dir: Some(cli_dir.path().to_path_buf()),
                override_username: None,
            },
            &state,
        )
        .unwrap();

        // Same call `init_or_unlock` makes.
        let cfg = state.load_identity_config("alice").unwrap();
        assert_eq!(cfg.resolvers.as_deref(), Some(&["1.1.1.1".to_string()][..]));
        let publish = cfg.publish.unwrap();
        assert!(publish.tsig_secret_path.exists());
        // db file lives where DmpClientConfig.db_path points.
        assert!(state.identity_db_path("alice").exists());
    }
}
