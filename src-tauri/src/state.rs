//! Process-wide state for the Tauri host.
//!
//! Each identity owns its own sqlite under
//! `~/.dmp/identities/<username>/dmp-rs.sqlite`. The top-level
//! `index.yaml` lists known identities and points at the most recently
//! active one; identity secrets are never stored in the index.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use dnsmesh_client::DmpClient;
use dnsmesh_net::{
    DnsRecordReader, DnsRecordWriter, DnsUpdateWriter, DnsUpdateWriterConfig, InMemoryDnsStore,
    NetError, ResolverPool, TsigAlgorithm, TsigKey,
};
use parking_lot::RwLock as ParkingLotRwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::CommandError;

/// Maximum bytes allowed in a username. Mirrors the dnsmesh-core
/// username limit so we never accept a value the SDK would later reject.
pub const USERNAME_MAX_BYTES: usize = 64;

/// Validate a username before it is used as a path component.
///
/// Rejects empty / overlong values, path separators, NUL, leading `.`,
/// and any path that doesn't parse to a single normal component.
/// Returns the trimmed username on success.
pub fn sanitize_username(input: &str) -> Result<String, CommandError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(CommandError::new(
            "validation",
            "username must not be empty",
        ));
    }
    if trimmed.len() > USERNAME_MAX_BYTES {
        return Err(CommandError::new(
            "validation",
            format!(
                "username must be at most {USERNAME_MAX_BYTES} bytes (got {})",
                trimmed.len()
            ),
        ));
    }
    if trimmed.starts_with('.') {
        return Err(CommandError::new(
            "validation",
            "username must not start with '.'",
        ));
    }
    if trimmed.chars().any(|c| c == '/' || c == '\\' || c == '\0') {
        return Err(CommandError::new(
            "validation",
            "username must not contain path separators or NUL",
        ));
    }
    // Defence in depth: reject anything that isn't exactly one normal
    // path component. Catches Windows drive prefixes and other
    // platform-specific shapes the character-level check above misses.
    let mut components = Path::new(trimmed).components();
    let first = components.next();
    if components.next().is_some() {
        return Err(CommandError::new(
            "validation",
            "username must be a single path component",
        ));
    }
    match first {
        Some(Component::Normal(_)) => Ok(trimmed.to_string()),
        _ => Err(CommandError::new(
            "validation",
            "username must be a single normal path component",
        )),
    }
}

/// File name used for each identity's per-user config (publish settings,
/// resolver overrides). Lives next to the sqlite db.
pub const IDENTITY_CONFIG_FILE: &str = "config.yaml";

/// File name of the top-level index that lists every known identity.
pub const INDEX_FILE: &str = "index.yaml";

/// On-disk shape of `~/.dmp/identities/index.yaml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IdentityIndex {
    /// Username of the most recently active identity, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    /// Every identity we know about. Order is preserved.
    #[serde(default)]
    pub identities: Vec<IdentityIndexEntry>,
}

/// One row in the [`IdentityIndex`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityIndexEntry {
    pub username: String,
    pub domain: String,
}

/// On-disk shape of each identity's `config.yaml`. Mirrors the slice of
/// the CLI's `ConfigFile` that's relevant to the desktop app: publish
/// settings (TSIG) and optional resolver overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IdentityConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolvers: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish: Option<PublishConfig>,

    /// Per-identity Argon2id salt, base64-encoded for clean YAML
    /// round-trips. Generated once and persisted so the same passphrase
    /// keeps deriving the same crypto identity. Without per-identity
    /// salts, two identities created with the same passphrase would
    /// share a `user_id` and collide on slot labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kdf_salt_base64: Option<String>,

    /// Cross-zone claim discovery zones. When non-empty, send paths
    /// publish a `ClaimRecord` into each listed zone (mirrors the
    /// CLI's `--claim-via PROVIDER_ZONE`) so a recipient whose poll
    /// walks the same provider zone can pick up the message without
    /// the sender writing into the recipient's authoritative zone.
    /// The poll loop also walks every configured zone via
    /// `receive_via_claim`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_via: Option<Vec<String>>,
}

/// TSIG-signed publish destination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishConfig {
    pub zone: String,
    /// `host:port` of the authoritative server.
    pub server: String,
    pub tsig_key_name: String,
    #[serde(default = "default_tsig_algorithm")]
    pub tsig_algorithm: String,
    /// Path to a file holding the TSIG secret. Accepts `base64:`,
    /// `hex:` prefixes or raw bytes.
    pub tsig_secret_path: PathBuf,
}

fn default_tsig_algorithm() -> String {
    "hmac-sha256".to_string()
}

/// A `DnsRecordReader` whose backing pool can be swapped at runtime
/// without rebuilding the surrounding `DmpClient`.
///
/// Hickory's resolver pools cache UDP sockets bound to whichever
/// network interface was active at construction time. When a VPN
/// drops on Android, those sockets stay bound to the now-defunct
/// tunnel interface and writes silently fail. We hand the SDK an
/// `Arc<RefreshableReader>` once and call [`Self::replace`] to swap
/// in a freshly-built pool when the network changes.
pub struct RefreshableReader {
    inner: ParkingLotRwLock<Arc<dyn DnsRecordReader>>,
}

impl RefreshableReader {
    /// Wrap an initial reader.
    pub fn new(initial: Arc<dyn DnsRecordReader>) -> Self {
        Self {
            inner: ParkingLotRwLock::new(initial),
        }
    }

    /// Atomically swap the inner reader. The previous reader is
    /// dropped once the last in-flight query that captured it returns.
    pub fn replace(&self, next: Arc<dyn DnsRecordReader>) {
        *self.inner.write() = next;
    }
}

#[async_trait]
impl DnsRecordReader for RefreshableReader {
    async fn query_txt_record(&self, name: &str) -> Result<Option<Vec<String>>, NetError> {
        // Clone the Arc under the lock, drop the lock, then await on
        // the inner. Lock is never held across `.await`.
        let inner = self.inner.read().clone();
        inner.query_txt_record(name).await
    }
}

/// The active client + a flag for whether publishing is wired.
///
/// `refreshable_reader` is the same `Arc<RefreshableReader>` handed
/// into the `DmpClient` at construction; we keep a clone so the
/// `refresh_network` command can swap the inner pool atomically.
pub struct ActiveClient {
    /// Wrapped in `Arc` so background commands can take a snapshot
    /// (e.g. the unlock+24h republish heartbeat) and drop the outer
    /// `state.active.read()` guard before issuing a slow network call.
    /// Without this, `lock_identity` / `switch_identity` block behind
    /// an in-flight publish whenever the TSIG server is sluggish.
    pub client: Arc<DmpClient>,
    pub username: String,
    pub domain: String,
    pub publish_configured: bool,
    pub refreshable_reader: Arc<RefreshableReader>,
    /// Configured claim-via provider zones for cross-zone first-
    /// contact. Empty when the identity hasn't opted into a
    /// provider. Snapshotted at unlock; settings changes drop the
    /// active client so the next unlock picks up the new list.
    pub claim_via: Vec<String>,
}

/// Process-wide state plumbed via `tauri::State`.
pub struct AppState {
    /// The currently unlocked identity, if any.
    pub active: RwLock<Option<ActiveClient>>,
    /// Root directory holding `index.yaml` and the per-identity
    /// subdirectories. Defaults to `$HOME/.dmp/identities`.
    pub root: PathBuf,
}

impl AppState {
    pub fn new(root: PathBuf) -> Self {
        Self {
            active: RwLock::new(None),
            root,
        }
    }

    /// Pick the identities root directory.
    ///
    /// Priority: `$DMP_DESKTOP_HOME`, then `$DMP_CONFIG_HOME/identities`,
    /// then `$HOME/.dmp/identities`.
    pub fn default_root() -> Result<PathBuf> {
        if let Some(p) = std::env::var_os("DMP_DESKTOP_HOME") {
            return Ok(PathBuf::from(p));
        }
        if let Some(p) = std::env::var_os("DMP_CONFIG_HOME") {
            return Ok(PathBuf::from(p).join("identities"));
        }
        let home = std::env::var_os("HOME")
            .ok_or_else(|| anyhow!("HOME is not set; set DMP_DESKTOP_HOME or HOME"))?;
        Ok(PathBuf::from(home).join(".dmp").join("identities"))
    }

    /// Path to the index file.
    pub fn index_path(&self) -> PathBuf {
        self.root.join(INDEX_FILE)
    }

    /// Per-identity directory.
    pub fn identity_dir(&self, username: &str) -> PathBuf {
        self.root.join(username)
    }

    /// Per-identity sqlite db path.
    pub fn identity_db_path(&self, username: &str) -> PathBuf {
        self.identity_dir(username).join("dmp-rs.sqlite")
    }

    /// Per-identity config path.
    pub fn identity_config_path(&self, username: &str) -> PathBuf {
        self.identity_dir(username).join(IDENTITY_CONFIG_FILE)
    }

    /// Load the on-disk index, returning an empty one if the file is missing.
    pub fn load_index(&self) -> Result<IdentityIndex> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(IdentityIndex::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let parsed: IdentityIndex =
            serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        Ok(parsed)
    }

    /// Persist the index to disk, creating parent directories as needed.
    pub fn save_index(&self, index: &IdentityIndex) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        let yaml = serde_yaml::to_string(index).context("serialising identity index")?;
        let path = self.index_path();
        std::fs::write(&path, yaml).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Load the per-identity config. Missing files return the empty
    /// default (read-only mode).
    pub fn load_identity_config(&self, username: &str) -> Result<IdentityConfig> {
        let path = self.identity_config_path(username);
        if !path.exists() {
            return Ok(IdentityConfig::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let parsed: IdentityConfig =
            serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        Ok(parsed)
    }

    /// Persist the per-identity config.
    pub fn save_identity_config(&self, username: &str, cfg: &IdentityConfig) -> Result<()> {
        let dir = self.identity_dir(username);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = self.identity_config_path(username);
        let yaml = serde_yaml::to_string(cfg).context("serialising identity config")?;
        std::fs::write(&path, yaml).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// Build a [`DnsRecordReader`] from optional resolver overrides.
///
/// `None` / empty → use the well-known public resolvers.
pub fn build_reader(resolvers: Option<&[String]>) -> Result<Arc<dyn DnsRecordReader>> {
    use dnsmesh_net::{HostSpec, ResolverPoolConfig};

    let pool = match resolvers {
        Some(list) if !list.is_empty() => {
            let hosts: Vec<HostSpec> = list
                .iter()
                .map(|s| {
                    s.parse::<HostSpec>()
                        .map_err(|e| anyhow!("invalid resolver `{s}`: {e}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            ResolverPool::new(hosts, ResolverPoolConfig::default())
                .context("building resolver pool")?
        }
        _ => ResolverPool::well_known().context("building well-known resolver pool")?,
    };
    Ok(Arc::new(pool))
}

/// Build a [`DnsRecordWriter`] from optional publish config.
///
/// Returns `(writer, true)` if a real TSIG writer was wired, `(stub,
/// false)` otherwise. The stub satisfies the SDK's "must have a writer"
/// requirement; commands that need to publish gate on the boolean.
pub fn build_writer(publish: Option<&PublishConfig>) -> Result<(Arc<dyn DnsRecordWriter>, bool)> {
    match publish {
        Some(p) => {
            use std::net::ToSocketAddrs as _;
            let server = p
                .server
                .to_socket_addrs()
                .with_context(|| format!("resolving DNS UPDATE server `{}`", p.server))?
                .next()
                .ok_or_else(|| anyhow!("server `{}` resolved to zero addresses", p.server))?;
            let algorithm = TsigAlgorithm::parse(&p.tsig_algorithm)
                .with_context(|| format!("unsupported TSIG algorithm `{}`", p.tsig_algorithm))?;
            let secret = read_tsig_secret(&p.tsig_secret_path)?;
            let key = TsigKey::new(&p.tsig_key_name, algorithm, secret)
                .context("building TSIG key from config")?;
            let cfg = DnsUpdateWriterConfig::new(p.zone.clone(), server, key);
            let writer = DnsUpdateWriter::new(cfg).context("building DnsUpdateWriter")?;
            Ok((Arc::new(writer), true))
        }
        None => Ok((Arc::new(InMemoryDnsStore::new()), false)),
    }
}

/// Read a TSIG secret from disk. Accepts `base64:` / `hex:` prefixes
/// or raw bytes — same shape the CLI consumes.
fn read_tsig_secret(path: &Path) -> Result<Vec<u8>> {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;

    let bytes = std::fs::read(path)
        .with_context(|| format!("reading TSIG secret at {}", path.display()))?;
    let text = std::str::from_utf8(&bytes).map_or("", str::trim);
    if let Some(b64_body) = text.strip_prefix("base64:") {
        return BASE64
            .decode(b64_body.trim())
            .with_context(|| format!("base64-decoding TSIG secret at {}", path.display()));
    }
    if let Some(hex_body) = text.strip_prefix("hex:") {
        return hex::decode(hex_body.trim())
            .with_context(|| format!("hex-decoding TSIG secret at {}", path.display()));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Two `InMemoryDnsStore`s with different content. After swapping
    /// the wrapper's inner reader, queries must observe the new
    /// backend.
    #[tokio::test]
    async fn refreshable_reader_swap_is_observable() {
        let first = Arc::new(InMemoryDnsStore::new());
        first
            .publish_txt_record("a.example.", "first", 60)
            .await
            .unwrap();
        let second = Arc::new(InMemoryDnsStore::new());
        second
            .publish_txt_record("a.example.", "second", 60)
            .await
            .unwrap();

        let wrapper = RefreshableReader::new(first.clone());
        assert_eq!(
            wrapper.query_txt_record("a.example.").await.unwrap(),
            Some(vec!["first".to_string()]),
        );

        wrapper.replace(second.clone());
        assert_eq!(
            wrapper.query_txt_record("a.example.").await.unwrap(),
            Some(vec!["second".to_string()]),
        );
    }

    #[test]
    fn save_load_index_round_trip() {
        let dir = TempDir::new().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let mut idx = IdentityIndex::default();
        idx.identities.push(IdentityIndexEntry {
            username: "alice".into(),
            domain: "mesh.local".into(),
        });
        idx.active = Some("alice".into());
        state.save_index(&idx).unwrap();
        let loaded = state.load_index().unwrap();
        assert_eq!(loaded.active.as_deref(), Some("alice"));
        assert_eq!(loaded.identities.len(), 1);
        assert_eq!(loaded.identities[0].username, "alice");
    }

    #[test]
    fn missing_index_loads_as_empty() {
        let dir = TempDir::new().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let loaded = state.load_index().unwrap();
        assert!(loaded.active.is_none());
        assert!(loaded.identities.is_empty());
    }

    #[test]
    fn identity_dir_layout() {
        let dir = TempDir::new().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        assert_eq!(state.identity_dir("alice"), dir.path().join("alice"));
        assert_eq!(
            state.identity_db_path("alice"),
            dir.path().join("alice").join("dmp-rs.sqlite"),
        );
        assert_eq!(
            state.identity_config_path("alice"),
            dir.path().join("alice").join("config.yaml"),
        );
    }

    #[test]
    fn identity_config_save_load_round_trip() {
        let dir = TempDir::new().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let cfg = IdentityConfig {
            resolvers: Some(vec!["1.1.1.1".into()]),
            publish: Some(PublishConfig {
                zone: "dmp.example.com".into(),
                server: "192.0.2.1:53".into(),
                tsig_key_name: "dmp-publish".into(),
                tsig_algorithm: "hmac-sha256".into(),
                tsig_secret_path: PathBuf::from("tsig.key"),
            }),
            kdf_salt_base64: None,
            claim_via: Some(vec!["claims.example.com".into()]),
        };
        state.save_identity_config("alice", &cfg).unwrap();
        let loaded = state.load_identity_config("alice").unwrap();
        assert_eq!(loaded.resolvers.as_ref().unwrap().len(), 1);
        assert!(loaded.publish.is_some());
        assert_eq!(
            loaded.claim_via.as_deref(),
            Some(&["claims.example.com".to_string()][..]),
            "claim_via must round-trip through yaml",
        );
    }

    #[test]
    fn missing_identity_config_is_empty_default() {
        let dir = TempDir::new().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let cfg = state.load_identity_config("nobody").unwrap();
        assert!(cfg.resolvers.is_none());
        assert!(cfg.publish.is_none());
    }

    #[test]
    fn sanitize_username_accepts_normal() {
        assert_eq!(sanitize_username("alice").unwrap(), "alice");
        assert_eq!(sanitize_username("  bob  ").unwrap(), "bob");
        assert_eq!(sanitize_username("user-1_2").unwrap(), "user-1_2");
    }

    #[test]
    fn sanitize_username_rejects_traversal_and_separators() {
        for bad in [
            "../../etc",
            "../etc/passwd",
            "foo/bar",
            "a/b\\c",
            "..",
            "/",
            "\\",
        ] {
            assert!(
                sanitize_username(bad).is_err(),
                "expected `{bad}` to be rejected"
            );
        }
    }

    #[test]
    fn sanitize_username_rejects_hidden_and_empty() {
        assert!(sanitize_username("").is_err());
        assert!(sanitize_username("   ").is_err());
        assert!(sanitize_username(".hidden").is_err());
        assert!(sanitize_username(".").is_err());
    }

    #[test]
    fn sanitize_username_rejects_too_long() {
        let too_long = "a".repeat(USERNAME_MAX_BYTES + 1);
        assert!(sanitize_username(&too_long).is_err());
        let just_right = "a".repeat(USERNAME_MAX_BYTES);
        assert!(sanitize_username(&just_right).is_ok());
    }

    #[test]
    fn sanitize_username_rejects_nul() {
        assert!(sanitize_username("foo\0bar").is_err());
    }
}
