//! Resolver / network diagnostic commands. `doctor` mirrors the CLI's
//! checklist: config present, resolvers reachable, publish wired,
//! identity actually live in DNS.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::State;
use tokio::sync::OnceCell;

use dnsmesh_core::crypto::DmpCrypto;
use dnsmesh_core::heartbeat::HeartbeatRecord;
use dnsmesh_net::resolver_pool::WELL_KNOWN_RESOLVERS;
use dnsmesh_net::{DnsRecordReader, ResolverPool, ResolverPoolConfig};

use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

/// Owner of the per-zone heartbeat RRset:
/// `_dnsmesh-heartbeat.<zone>`. Mirrors `dmp/core/heartbeat.py`.
const HEARTBEAT_RRSET_PREFIX: &str = "_dnsmesh-heartbeat.";

/// Heartbeat verification clock skew (seconds). Matches the live
/// integration test — operators may publish a little ahead.
const DISCOVERY_TS_SKEW_SECONDS: u64 = 600;

/// One resolver entry surfaced to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct ResolverInfo {
    pub address: String,
    pub operator: String,
}

const fn operator_for(ip: &str) -> &'static str {
    match ip.as_bytes() {
        b"8.8.8.8" | b"8.8.4.4" => "Google",
        b"1.1.1.1" | b"1.0.0.1" => "Cloudflare",
        b"9.9.9.9" | b"149.112.112.112" => "Quad9",
        b"208.67.222.222" | b"208.67.220.220" => "OpenDNS",
        _ => "Unknown",
    }
}

#[tauri::command]
pub async fn list_known_resolvers() -> CommandResult<Vec<ResolverInfo>> {
    Ok(WELL_KNOWN_RESOLVERS
        .iter()
        .map(|ip| ResolverInfo {
            address: (*ip).to_string(),
            operator: operator_for(ip).to_string(),
        })
        .collect())
}

/// What resolver pool is actually in effect. `source` is `"override"`
/// when the per-identity config has a non-empty `resolvers:` list,
/// `"well_known"` otherwise.
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveResolvers {
    pub addresses: Vec<String>,
    pub source: String,
}

#[tauri::command]
pub async fn effective_resolvers(state: State<'_, AppState>) -> CommandResult<EffectiveResolvers> {
    let guard = state.active.read().await;
    if let Some(active) = guard.as_ref() {
        let cfg = state
            .load_identity_config(&active.username)
            .map_err(CommandError::from)?;
        if let Some(list) = cfg.resolvers.as_ref() {
            let trimmed: Vec<String> = list
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !trimmed.is_empty() {
                return Ok(EffectiveResolvers {
                    addresses: trimmed,
                    source: "override".to_string(),
                });
            }
        }
    }
    Ok(EffectiveResolvers {
        addresses: WELL_KNOWN_RESOLVERS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        source: "well_known".to_string(),
    })
}

/// Severity of a doctor check.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

/// One row in the doctor report.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
}

/// Aggregated doctor output.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
    pub overall: CheckStatus,
}

/// Lightweight diagnostic over the active identity. The active client
/// implies successful sqlite + KDF, so the CLI's config-loading step
/// is skipped here.
#[tauri::command]
pub async fn doctor(state: State<'_, AppState>) -> CommandResult<DoctorReport> {
    let mut checks: Vec<CheckResult> = Vec::new();
    let mut any_fail = false;
    let mut any_warn = false;

    // 1. Active identity.
    let guard = state.active.read().await;
    let Some(active) = guard.as_ref() else {
        checks.push(CheckResult {
            name: "identity".into(),
            status: CheckStatus::Fail,
            message: "no identity unlocked — open Identities to create or unlock one".into(),
        });
        return Ok(DoctorReport {
            checks,
            overall: CheckStatus::Fail,
        });
    };
    checks.push(CheckResult {
        name: "identity".into(),
        status: CheckStatus::Pass,
        message: format!("active: {}@{}", active.username, active.domain),
    });

    // 2. Resolver pool reachability. Use a known-public name so a
    //    transient miss for our own zone doesn't poison the result.
    //    google.com is just a stable lookup target, not a dependency.
    match ResolverPool::well_known() {
        Ok(pool) => match pool.query_txt_record("google.com").await {
            Ok(_) => checks.push(CheckResult {
                name: "resolvers".into(),
                status: CheckStatus::Pass,
                message: format!("reachable ({} upstream(s))", pool.len()),
            }),
            Err(e) => {
                any_warn = true;
                checks.push(CheckResult {
                    name: "resolvers".into(),
                    status: CheckStatus::Warn,
                    message: format!("TXT lookup failed: {e}"),
                });
            }
        },
        Err(e) => {
            any_fail = true;
            checks.push(CheckResult {
                name: "resolvers".into(),
                status: CheckStatus::Fail,
                message: format!("cannot build well-known pool: {e}"),
            });
        }
    }
    let _ = ResolverPoolConfig::default();

    // 3. Publish destination configured?
    if active.publish_configured {
        checks.push(CheckResult {
            name: "publish".into(),
            status: CheckStatus::Pass,
            message: "TSIG-signed UPDATE writer wired".into(),
        });
    } else {
        any_warn = true;
        checks.push(CheckResult {
            name: "publish".into(),
            status: CheckStatus::Warn,
            message: "no publish (TSIG) block configured — publish/send will refuse".into(),
        });
    }

    // 4. Is our identity actually published?
    let self_addr = format!("{}@{}", active.username, active.domain);
    match active.client.fetch_identity(&self_addr).await {
        Ok(_) => checks.push(CheckResult {
            name: "identity_published".into(),
            status: CheckStatus::Pass,
            message: format!("found at {self_addr}"),
        }),
        Err(e) => {
            any_warn = true;
            checks.push(CheckResult {
                name: "identity_published".into(),
                status: CheckStatus::Warn,
                message: format!("not yet published at {self_addr} ({e})"),
            });
        }
    }

    let overall = if any_fail {
        CheckStatus::Fail
    } else if any_warn {
        CheckStatus::Warn
    } else {
        CheckStatus::Pass
    };
    Ok(DoctorReport { checks, overall })
}

/// One discovered DMP operator surfaced to the create-identity flow.
/// Built from a single verified heartbeat TXT.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredNode {
    /// Operator HTTPS endpoint, used for TSIG registration and as the
    /// publish-server prefill.
    pub endpoint: String,
    /// Operator Ed25519 signing pubkey (hex).
    pub operator_spk_hex: String,
    /// Heartbeat-reported software version (free-form ASCII).
    pub version: String,
    /// Operator's claim-provider zone, if any. Empty for legacy
    /// DMPHB02 wires or non-claim-provider operators.
    pub claim_provider_zone: String,
    /// Seconds until the heartbeat expires. Always positive in
    /// returned values.
    pub seconds_until_stale: i64,
}

/// Discover live DMP node operators publishing under `zone`.
///
/// Queries `_dnsmesh-heartbeat.<zone>` TXT, verifies each answer with
/// a 600s skew, and returns the freshest first. Bad / expired TXTs are
/// silently dropped. Does not require an active identity — the
/// create-identity flow calls this before any state hits disk.
#[tauri::command]
pub async fn discover_nodes(
    _state: State<'_, AppState>,
    zone: String,
) -> CommandResult<Vec<DiscoveredNode>> {
    let zone_trimmed = zone.trim();
    if zone_trimmed.is_empty() {
        return Err(CommandError::new("validation", "zone must not be empty"));
    }
    discover_nodes_in_zone(zone_trimmed).await
}

/// Shared between [`discover_nodes`] and [`list_known_nodes`]. Returns
/// an empty `Vec` on transport-level misses; only resolver-pool build
/// failures surface as errors.
async fn discover_nodes_in_zone(zone: &str) -> CommandResult<Vec<DiscoveredNode>> {
    let pool = ResolverPool::well_known().map_err(CommandError::from)?;
    let rrset_name = format!("{HEARTBEAT_RRSET_PREFIX}{zone}");

    // Both NXDOMAIN-equivalents and transient transport failures
    // collapse to "no nodes found"; the UI offers manual entry.
    let Ok(Some(answers)) = pool.query_txt_record(&rrset_name).await else {
        return Ok(Vec::new());
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    let mut discovered: Vec<DiscoveredNode> = Vec::new();
    for answer in &answers {
        let Some(record) =
            HeartbeatRecord::parse_and_verify(answer, Some(now), DISCOVERY_TS_SKEW_SECONDS)
        else {
            continue;
        };
        // exp > now is already enforced by parse_and_verify.
        let seconds_until_stale = i64::try_from(record.exp.saturating_sub(now)).unwrap_or(i64::MAX);
        if seconds_until_stale <= 0 {
            continue;
        }
        discovered.push(DiscoveredNode {
            endpoint: record.endpoint,
            operator_spk_hex: hex::encode(record.operator_spk),
            version: record.version,
            claim_provider_zone: record.claim_provider_zone,
            seconds_until_stale,
        });
    }

    // Freshest first so the UI can default-select index 0.
    discovered.sort_by_key(|n| std::cmp::Reverse(n.seconds_until_stale));
    Ok(discovered)
}

/// Curated list of known DMP node operators. DMP has no central
/// directory, so a fresh install needs at least one zone to query.
/// Adding a node = bump this constant and ship a release.
pub const WELL_KNOWN_NODES: &[KnownNode] = &[
    KnownNode {
        zone: "dmp.dnsmesh.io",
        operator_name: "dnsmesh.io (reference node)",
        description: "The canonical public DMP node operated by the DNS Mesh Protocol project. Open registration.",
    },
    KnownNode {
        zone: "dmp.dnsmesh.pro",
        operator_name: "dnsmesh.pro",
        description: "Open-registration DMP node hosted on dnsmesh.pro.",
    },
];

/// Static metadata for one curated DMP node.
#[derive(Debug, Clone, Serialize)]
pub struct KnownNode {
    pub zone: &'static str,
    pub operator_name: &'static str,
    pub description: &'static str,
}

/// Curated-node entry enriched with live heartbeat info. `live` is
/// `Some` when at least one verified heartbeat was found (freshest
/// wins), `None` otherwise. `source` distinguishes curated entries
/// from directory-feed entries.
#[derive(Debug, Clone, Serialize)]
pub struct KnownNodeStatus {
    pub zone: String,
    pub operator_name: String,
    pub description: String,
    pub live: Option<DiscoveredNode>,
    /// `"curated"` for hard-coded [`WELL_KNOWN_NODES`] entries,
    /// `"directory"` for entries from the public directory feed.
    pub source: String,
}

/// One node entry from `dnsmeshprotocol.org/directory/feed.json`. Only
/// the UI-surfaced fields are deserialised.
#[derive(Debug, Clone, Deserialize)]
pub struct DirectoryNode {
    pub endpoint: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub exp: u64,
    #[serde(default)]
    pub last_seen_via: Vec<String>,
    #[serde(default)]
    pub geo: Option<DirectoryGeo>,
}

/// Geolocation block on a [`DirectoryNode`].
#[derive(Debug, Clone, Deserialize)]
pub struct DirectoryGeo {
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub country_code: String,
    #[serde(default)]
    pub city: String,
}

/// Top-level shape of `dnsmeshprotocol.org/directory/feed.json`.
#[derive(Debug, Clone, Deserialize)]
struct DirectoryFeed {
    #[serde(default)]
    nodes: Vec<DirectoryNode>,
}

/// URL of the public DMP node directory feed.
const DIRECTORY_FEED_URL: &str = "https://dnsmeshprotocol.org/directory/feed.json";

/// Timeout for fetching the directory feed.
const DIRECTORY_FEED_TIMEOUT: Duration = Duration::from_secs(5);

/// In-process cache TTL for the directory feed (1 hour).
#[allow(clippy::duration_suboptimal_units)]
const DIRECTORY_FEED_TTL: Duration = Duration::from_secs(3_600);

/// Cache slot alias — keeps the static below readable.
type DirectoryFeedCache = parking_lot::Mutex<Option<(Instant, Vec<DirectoryNode>)>>;

/// Lazy in-process cache for the directory feed. Network failures
/// never poison the cache; the fetcher returns an empty `Vec` and the
/// next call retries.
static DIRECTORY_FEED_CACHE: OnceCell<DirectoryFeedCache> = OnceCell::const_new();

async fn directory_feed_cache_slot() -> &'static DirectoryFeedCache {
    DIRECTORY_FEED_CACHE
        .get_or_init(|| async { parking_lot::Mutex::new(None) })
        .await
}

/// Fetch the directory feed, using the in-process cache when fresh.
/// Any failure collapses to an empty `Vec`; this is best-effort
/// enrichment on top of the curated list.
async fn fetch_directory_feed() -> Vec<DirectoryNode> {
    let slot = directory_feed_cache_slot().await;
    if let Some((stored_at, ref cached)) = *slot.lock()
        && stored_at.elapsed() < DIRECTORY_FEED_TTL
    {
        return cached.clone();
    }

    let Ok(client) = reqwest::ClientBuilder::new()
        .timeout(DIRECTORY_FEED_TIMEOUT)
        .build()
    else {
        return Vec::new();
    };
    let nodes = match client.get(DIRECTORY_FEED_URL).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<DirectoryFeed>().await {
            Ok(feed) => feed.nodes,
            Err(_) => Vec::new(),
        },
        _ => Vec::new(),
    };
    *slot.lock() = Some((Instant::now(), nodes.clone()));
    nodes
}

/// Pull the bare host out of a directory-feed `endpoint` URL.
fn endpoint_host(endpoint: &str) -> String {
    let no_scheme = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest);
    no_scheme.split('/').next().unwrap_or(no_scheme).to_string()
}

/// `<host> · <city>, <country_code>` when geo is rich, falling back to
/// the bare host otherwise.
fn directory_operator_label(node: &DirectoryNode) -> String {
    let host = endpoint_host(&node.endpoint);
    let geo = node.geo.as_ref();
    let city = geo.map_or("", |g| g.city.trim());
    let cc = geo.map_or("", |g| g.country_code.trim());
    let country = geo.map_or("", |g| g.country.trim());
    if !city.is_empty() && !cc.is_empty() {
        format!("{host} · {city}, {cc}")
    } else if !city.is_empty() && !country.is_empty() {
        format!("{host} · {city}, {country}")
    } else if !country.is_empty() {
        format!("{host} · {country}")
    } else {
        host
    }
}

/// Return [`WELL_KNOWN_NODES`] enriched with live heartbeat status,
/// merged with the public directory feed.
///
/// Per-zone heartbeat discoveries fire in parallel with the directory
/// fetch (cached one hour). Curated entries win on zone collision;
/// directory-only zones are appended after. Network failures collapse
/// to "no extra entries" / `live: None` rather than poisoning the
/// listing.
#[tauri::command]
pub async fn list_known_nodes() -> CommandResult<Vec<KnownNodeStatus>> {
    // Kick off per-zone discoveries and the directory fetch
    // concurrently.
    let mut set: tokio::task::JoinSet<(usize, Option<DiscoveredNode>)> =
        tokio::task::JoinSet::new();
    for (idx, node) in WELL_KNOWN_NODES.iter().enumerate() {
        let zone = node.zone.to_string();
        set.spawn(async move {
            // Collapse a per-zone failure into `None` so one dead node
            // doesn't take the whole list down.
            let discovered = discover_nodes_in_zone(&zone).await.unwrap_or_default();
            (idx, discovered.into_iter().next())
        });
    }
    let directory_fut = fetch_directory_feed();

    let directory = directory_fut.await;
    let mut slots: Vec<Option<DiscoveredNode>> =
        (0..WELL_KNOWN_NODES.len()).map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        if let Ok((idx, live)) = joined
            && let Some(slot) = slots.get_mut(idx)
        {
            *slot = live;
        }
    }

    // Curated rows first (preserve declaration order).
    let mut out: Vec<KnownNodeStatus> = WELL_KNOWN_NODES
        .iter()
        .zip(slots)
        .map(|(node, live)| KnownNodeStatus {
            zone: node.zone.to_string(),
            operator_name: node.operator_name.to_string(),
            description: node.description.to_string(),
            live,
            source: "curated".to_string(),
        })
        .collect();

    let mut seen_zones: HashSet<String> = out.iter().map(|n| n.zone.clone()).collect();

    // One row per `last_seen_via` zone, deduped against curated zones
    // and against earlier directory entries.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    for node in directory {
        let operator_name = directory_operator_label(&node);
        let endpoint = node.endpoint.clone();
        let version = node.version.clone();
        let exp = node.exp;
        for zone in &node.last_seen_via {
            let zone = zone.trim();
            if zone.is_empty() {
                continue;
            }
            if !seen_zones.insert(zone.to_string()) {
                continue;
            }
            let seconds_until_stale = if exp > now {
                i64::try_from(exp - now).unwrap_or(i64::MAX)
            } else {
                0
            };
            let live = if seconds_until_stale > 0 {
                Some(DiscoveredNode {
                    endpoint: endpoint.clone(),
                    operator_spk_hex: String::new(),
                    version: version.clone(),
                    claim_provider_zone: zone.to_string(),
                    seconds_until_stale,
                })
            } else {
                None
            };
            out.push(KnownNodeStatus {
                zone: zone.to_string(),
                operator_name: operator_name.clone(),
                description: format!("Discovered via dnsmeshprotocol.org directory · {endpoint}"),
                live,
                source: "directory".to_string(),
            });
        }
    }

    Ok(out)
}

// Keep `CommandError` referenced; the doctor surface is expected to
// grow new error sites.
#[allow(dead_code)]
fn _force_command_error_used(_e: &CommandError) {}

/// HTTP timeout for both the challenge GET and the confirm POST.
const REGISTRATION_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Domain-separator byte appended to the signed registration payload.
/// Must match the Python reference (`cmd_tsig_register`) — changing
/// this silently breaks server-side signature verification.
const REGISTRATION_PAYLOAD_VERSION: u8 = 0x01;

/// JSON shape from `GET /v1/registration/challenge`. Only `challenge`
/// and `node` are load-bearing.
#[derive(Debug, Clone, Deserialize)]
struct ChallengeResponse {
    challenge: String,
    node: String,
}

/// JSON shape from `POST /v1/registration/tsig-confirm` on success.
/// Field names mirror the Python CLI's parser. `tsig_secret_hex` is
/// re-encoded as base64 on disk; `zone` is the UPDATE zone the minted
/// key is scoped to.
#[derive(Debug, Clone, Deserialize)]
struct TsigConfirmResponse {
    tsig_key_name: String,
    tsig_secret_hex: String,
    #[serde(default = "default_tsig_algo_for_response")]
    tsig_algorithm: String,
    zone: String,
    #[serde(default)]
    expires_at: Option<i64>,
}

fn default_tsig_algo_for_response() -> String {
    "hmac-sha256".to_string()
}

/// Result returned to the UI after a successful TSIG registration.
/// `dns_server` is a bare host (port stripped); the publish writer
/// defaults to UDP/53.
#[derive(Debug, Clone, Serialize)]
pub struct RegisteredTsig {
    pub key_name: String,
    pub algorithm: String,
    pub secret_base64: String,
    pub dns_zone: String,
    pub dns_server: String,
    pub expires_at: Option<i64>,
}

/// Run the TSIG-register challenge / sign / confirm dance against a
/// multi-tenant DMP node.
///
/// The signing key is re-derived from `passphrase` (the SDK's `crypto`
/// field is `pub(crate)`); the KDF salt is read from the active
/// identity's `config.yaml` so the derivation matches `init_or_unlock`.
/// An unlocked identity is not required — the create-identity flow
/// calls this before any state is unlocked.
#[tauri::command]
#[allow(clippy::too_many_lines)] // multi-stage HTTP dance reads cleanest as one fn
pub async fn register_tsig(
    state: State<'_, AppState>,
    endpoint: String,
    subject: String,
    passphrase: String,
) -> CommandResult<RegisteredTsig> {
    let endpoint = endpoint.trim().trim_end_matches('/').to_string();
    if endpoint.is_empty() {
        return Err(CommandError::new(
            "validation",
            "endpoint must not be empty",
        ));
    }
    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        return Err(CommandError::new(
            "validation",
            "endpoint must start with http:// or https://",
        ));
    }
    let subject = subject.trim().to_string();
    if subject.is_empty() || !subject.contains('@') {
        return Err(CommandError::new(
            "validation",
            "subject must look like user@domain",
        ));
    }
    if passphrase.is_empty() {
        return Err(CommandError::new(
            "validation",
            "passphrase must not be empty",
        ));
    }

    // Re-derive the signing key. KDF salt MUST match what
    // `init_or_unlock` used; the sanity check below refuses if it
    // doesn't.
    let kdf_salt: Option<Vec<u8>> = {
        let guard = state.active.read().await;
        if let Some(active) = guard.as_ref() {
            let cfg = state
                .load_identity_config(&active.username)
                .map_err(CommandError::from)?;
            match cfg.kdf_salt_base64 {
                Some(b64) => {
                    use base64::Engine as _;
                    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
                    Some(BASE64_STANDARD.decode(&b64).map_err(|e| {
                        CommandError::new(
                            "internal",
                            format!("kdf_salt_base64 in active identity config is malformed: {e}"),
                        )
                    })?)
                }
                None => None,
            }
        } else {
            // No active identity (create-identity flow); use the
            // SDK default salt.
            None
        }
    };
    let crypto = DmpCrypto::from_passphrase(&passphrase, kdf_salt.as_deref()).map_err(|e| {
        CommandError::new("internal", format!("re-deriving signing key failed: {e}"))
    })?;
    let spk_hex = hex::encode(crypto.signing_public_key_bytes());
    let x25519_pub_hex = hex::encode(crypto.public_key_bytes());

    // If an identity is active, the re-derived signing key must match
    // it; mismatches turn an opaque server 401 into an early refusal.
    {
        let guard = state.active.read().await;
        if let Some(active) = guard.as_ref() {
            let active_spk = active.client.ed25519_signing_public_key_hex();
            if active_spk != spk_hex {
                return Err(CommandError::new(
                    "validation",
                    "passphrase does not match the active identity's signing key",
                ));
            }
        }
    }

    let http = reqwest::ClientBuilder::new()
        .timeout(REGISTRATION_HTTP_TIMEOUT)
        .build()
        .map_err(|e| CommandError::new("net", format!("building HTTP client failed: {e}")))?;

    let challenge = fetch_challenge(&http, &endpoint).await?;
    let signature_hex = sign_registration_payload(&crypto, &challenge, &subject)?;
    let confirm = submit_confirm(
        &http,
        &endpoint,
        &subject,
        &spk_hex,
        &x25519_pub_hex,
        &challenge.challenge,
        &signature_hex,
    )
    .await?;

    // Re-encode hex → base64 for the more compact on-disk form.
    let secret_bytes = hex::decode(&confirm.tsig_secret_hex).map_err(|e| {
        CommandError::with_details(
            "registration_failed",
            format!("tsig-confirm returned non-hex secret: {e}"),
            json!({ "stage": "confirm" }),
        )
    })?;
    let secret_base64 = {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD as BASE64;
        BASE64.encode(&secret_bytes)
    };

    // Derive the DNS UPDATE host from the HTTPS endpoint. Publish
    // writer defaults to UDP/53 unless overridden in Settings.
    let dns_server = strip_endpoint_to_host(&endpoint);

    Ok(RegisteredTsig {
        key_name: confirm.tsig_key_name,
        algorithm: confirm.tsig_algorithm,
        secret_base64,
        dns_zone: confirm.zone,
        dns_server,
        expires_at: confirm.expires_at,
    })
}

/// Stage 1: GET the challenge.
async fn fetch_challenge(
    http: &reqwest::Client,
    endpoint: &str,
) -> Result<ChallengeResponse, CommandError> {
    let url = format!("{endpoint}/v1/registration/challenge");
    let resp = http.get(&url).send().await.map_err(|e| {
        CommandError::with_details(
            "registration_failed",
            format!("cannot reach {endpoint}: {e}"),
            json!({ "stage": "challenge", "endpoint": endpoint }),
        )
    })?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(CommandError::with_details(
            "registration_failed",
            format!("challenge request failed: HTTP {status}"),
            json!({
                "stage": "challenge",
                "http_status": status.as_u16(),
                "body": body,
            }),
        ));
    }
    resp.json().await.map_err(|e| {
        CommandError::with_details(
            "registration_failed",
            format!("challenge response not JSON: {e}"),
            json!({ "stage": "challenge" }),
        )
    })
}

/// Stage 2: sign the canonical payload
/// `challenge_bytes || subject || node_hostname || 0x01`. Byte-for-byte
/// identical to the Python reference.
fn sign_registration_payload(
    crypto: &DmpCrypto,
    challenge: &ChallengeResponse,
    subject: &str,
) -> Result<String, CommandError> {
    let challenge_bytes = hex::decode(&challenge.challenge).map_err(|e| {
        CommandError::with_details(
            "registration_failed",
            format!("challenge is not valid hex: {e}"),
            json!({ "stage": "challenge", "value": challenge.challenge.clone() }),
        )
    })?;
    let mut payload =
        Vec::with_capacity(challenge_bytes.len() + subject.len() + challenge.node.len() + 1);
    payload.extend_from_slice(&challenge_bytes);
    payload.extend_from_slice(subject.as_bytes());
    payload.extend_from_slice(challenge.node.as_bytes());
    payload.push(REGISTRATION_PAYLOAD_VERSION);
    Ok(hex::encode(crypto.sign_data(&payload)))
}

/// Stage 3: POST the signed challenge back, mapping 401/403/404/409 to
/// the same human-friendly hints the Python CLI prints.
async fn submit_confirm(
    http: &reqwest::Client,
    endpoint: &str,
    subject: &str,
    spk_hex: &str,
    x25519_pub_hex: &str,
    challenge_hex: &str,
    signature_hex: &str,
) -> Result<TsigConfirmResponse, CommandError> {
    let url = format!("{endpoint}/v1/registration/tsig-confirm");
    let body = json!({
        "subject": subject,
        "ed25519_spk": spk_hex,
        "challenge": challenge_hex,
        "signature": signature_hex,
        "x25519_pub": x25519_pub_hex,
    });
    let resp = http.post(&url).json(&body).send().await.map_err(|e| {
        CommandError::with_details(
            "registration_failed",
            format!("tsig-confirm POST failed: {e}"),
            json!({ "stage": "confirm", "endpoint": endpoint }),
        )
    })?;
    let status = resp.status();
    if !status.is_success() {
        let response_body = resp.text().await.unwrap_or_default();
        let message = match status.as_u16() {
            401 => "node rejected the signature (401). Re-check the passphrase.".to_string(),
            403 => format!("subject {subject} not in the node's allowlist (403)"),
            404 => format!(
                "{endpoint} does not expose /v1/registration/tsig-confirm (404). Operator must enable DMP_DNS_UPDATE_ENABLED."
            ),
            409 => format!(
                "subject {subject} already owned by a different key (409). Use the same passphrase you registered with."
            ),
            _ => format!("tsig-confirm failed: HTTP {status}"),
        };
        return Err(CommandError::with_details(
            "registration_failed",
            message,
            json!({
                "stage": "confirm",
                "http_status": status.as_u16(),
                "body": response_body,
            }),
        ));
    }
    resp.json().await.map_err(|e| {
        CommandError::with_details(
            "registration_failed",
            format!("tsig-confirm response not JSON: {e}"),
            json!({ "stage": "confirm" }),
        )
    })
}

/// Strip scheme + port + path from an endpoint URL. HTTPS endpoint and
/// DNS UPDATE port are separate listeners; the writer defaults to 53.
fn strip_endpoint_to_host(endpoint: &str) -> String {
    let no_scheme = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest);
    let host_only = no_scheme.split('/').next().unwrap_or(no_scheme);
    // Handle bracketed IPv6 literals: `[::1]:8443` → `::1`,
    // `[::1]` → `::1`. Python's CLI does the same.
    if let Some(rest) = host_only.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        return rest[..end].to_string();
    }
    if let Some((host, _port)) = host_only.rsplit_once(':') {
        // Bare-IPv6 (more than one `:`, no brackets) isn't a valid
        // host:port — leave untouched.
        if !host.contains(':') {
            return host.to_string();
        }
    }
    host_only.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrset_prefix_matches_python_layout() {
        // Per-zone heartbeat owner contract: `_dnsmesh-heartbeat.<zone>`.
        assert_eq!(HEARTBEAT_RRSET_PREFIX, "_dnsmesh-heartbeat.");
        assert_eq!(
            format!("{HEARTBEAT_RRSET_PREFIX}{}", "dmp.dnsmesh.io"),
            "_dnsmesh-heartbeat.dmp.dnsmesh.io",
        );
    }

    /// Locks the wire shape from `POST /v1/registration/tsig-confirm`
    /// against the Python reference parser.
    #[test]
    fn tsig_confirm_response_json_round_trip() {
        let body = serde_json::json!({
            "tsig_key_name": "alkamod-test.dmp.dnsmesh.pro.",
            "tsig_secret_hex": "deadbeef00112233",
            "tsig_algorithm": "hmac-sha256",
            "zone": "dmp.dnsmesh.pro",
            "allowed_suffixes": [".dmp.dnsmesh.pro"],
            "expires_at": 1_800_000_000_i64,
        });
        let parsed: TsigConfirmResponse = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.tsig_key_name, "alkamod-test.dmp.dnsmesh.pro.");
        assert_eq!(parsed.tsig_secret_hex, "deadbeef00112233");
        assert_eq!(parsed.tsig_algorithm, "hmac-sha256");
        assert_eq!(parsed.zone, "dmp.dnsmesh.pro");
        assert_eq!(parsed.expires_at, Some(1_800_000_000));
    }

    /// `tsig_algorithm` defaults to `hmac-sha256`; `expires_at` is
    /// optional. Mirrors the Python parser.
    #[test]
    fn tsig_confirm_response_optional_fields() {
        let body = serde_json::json!({
            "tsig_key_name": "k.example.com.",
            "tsig_secret_hex": "00",
            "zone": "example.com",
        });
        let parsed: TsigConfirmResponse = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.tsig_algorithm, "hmac-sha256");
        assert_eq!(parsed.expires_at, None);
    }

    #[test]
    fn challenge_response_round_trip() {
        let body = serde_json::json!({
            "challenge": "305bf924c0c5fc81609a",
            "node": "dmp.dnsmesh.pro",
            "expires_at": 1_800_000_000_i64,
            "version": 1,
        });
        let parsed: ChallengeResponse = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.challenge, "305bf924c0c5fc81609a");
        assert_eq!(parsed.node, "dmp.dnsmesh.pro");
    }

    /// Curated-node list must not regress to empty — the Nodes page
    /// would otherwise have nothing to discover from.
    #[test]
    fn well_known_nodes_population() {
        assert!(
            WELL_KNOWN_NODES.len() >= 2,
            "curated node list must ship with at least two entries; got {}",
            WELL_KNOWN_NODES.len(),
        );
        for node in WELL_KNOWN_NODES {
            assert!(!node.zone.is_empty(), "zone must not be empty");
            assert!(
                !node.operator_name.is_empty(),
                "operator_name must not be empty",
            );
            assert!(
                !node.description.is_empty(),
                "description must not be empty",
            );
        }
    }

    /// Lock the [`KnownNodeStatus`] wire shape consumed by the UI.
    #[test]
    fn known_node_status_json_round_trip() {
        let status = KnownNodeStatus {
            zone: "dmp.dnsmesh.io".to_string(),
            operator_name: "dnsmesh.io (reference node)".to_string(),
            description: "canonical".to_string(),
            live: Some(DiscoveredNode {
                endpoint: "https://dnsmesh.io".to_string(),
                operator_spk_hex: "00".repeat(32),
                version: "0.6.6".to_string(),
                claim_provider_zone: "dmp.dnsmesh.io".to_string(),
                seconds_until_stale: 1234,
            }),
            source: "curated".to_string(),
        };
        let encoded = serde_json::to_string(&status).expect("serialize");
        // Field-name spot checks — the TS type keys off these.
        assert!(encoded.contains("\"zone\":\"dmp.dnsmesh.io\""));
        assert!(encoded.contains("\"operator_name\":"));
        assert!(encoded.contains("\"description\":"));
        assert!(encoded.contains("\"live\":"));
        assert!(encoded.contains("\"endpoint\":"));
        assert!(encoded.contains("\"seconds_until_stale\":1234"));

        let decoded: serde_json::Value = serde_json::from_str(&encoded).expect("parse");
        assert_eq!(decoded["zone"], "dmp.dnsmesh.io");
        assert_eq!(
            decoded["live"]["operator_spk_hex"]
                .as_str()
                .map_or(0, str::len),
            64,
        );

        // `live: None` must serialise as explicit `null` — the UI
        // differentiates LIVE vs DOWN on this field's presence.
        let down = KnownNodeStatus {
            zone: "dmp.example.com".to_string(),
            operator_name: "Example Corp".to_string(),
            description: "test".to_string(),
            live: None,
            source: "curated".to_string(),
        };
        let down_encoded = serde_json::to_string(&down).expect("serialize");
        assert!(
            down_encoded.contains("\"live\":null"),
            "expected explicit null for missing heartbeat, got {down_encoded}",
        );
    }

    #[test]
    fn endpoint_host_strips_scheme_and_path() {
        assert_eq!(endpoint_host("https://dnsmesh.io"), "dnsmesh.io");
        assert_eq!(endpoint_host("https://dnsmesh.io/"), "dnsmesh.io");
        assert_eq!(endpoint_host("https://dnsmesh.io/v1/foo"), "dnsmesh.io",);
        assert_eq!(endpoint_host("dnsmesh.io"), "dnsmesh.io");
    }

    #[test]
    fn directory_operator_label_format() {
        let with_geo = DirectoryNode {
            endpoint: "https://dnsmesh.io".into(),
            version: "0.6.6".into(),
            exp: 0,
            last_seen_via: vec!["dmp.dnsmesh.io".into()],
            geo: Some(DirectoryGeo {
                country: "United States".into(),
                country_code: "US".into(),
                city: "Santa Clara".into(),
            }),
        };
        assert_eq!(
            directory_operator_label(&with_geo),
            "dnsmesh.io · Santa Clara, US",
        );

        let no_geo = DirectoryNode {
            endpoint: "https://example.com".into(),
            version: String::new(),
            exp: 0,
            last_seen_via: vec![],
            geo: None,
        };
        assert_eq!(directory_operator_label(&no_geo), "example.com");
    }

    /// Lock the wire shape from the public directory feed.
    #[test]
    fn directory_feed_round_trips() {
        let body = serde_json::json!({
            "version": 2,
            "generated_at": 1_777_789_101_i64,
            "node_count": 1,
            "nodes": [
                {
                    "operator_spk_hex": "00".repeat(32),
                    "endpoint": "https://dnsmesh.io",
                    "version": "0.6.6",
                    "ts": 1_777_789_068_i64,
                    "exp": 1_777_875_468_i64,
                    "wire": "v=dmp1;t=heartbeat;...",
                    "last_seen_via": ["dmp.dnsmesh.io"],
                    "geo": { "country": "United States", "country_code": "US", "city": "Santa Clara" },
                    "host": { "asn": "AS14061" }
                }
            ]
        });
        let feed: DirectoryFeed = serde_json::from_value(body).unwrap();
        assert_eq!(feed.nodes.len(), 1);
        let n = &feed.nodes[0];
        assert_eq!(n.endpoint, "https://dnsmesh.io");
        assert_eq!(n.version, "0.6.6");
        assert_eq!(n.exp, 1_777_875_468);
        assert_eq!(n.last_seen_via, vec!["dmp.dnsmesh.io"]);
        let geo = n.geo.as_ref().expect("geo");
        assert_eq!(geo.city, "Santa Clara");
        assert_eq!(geo.country_code, "US");
    }

    /// `source` must be carried so the UI can label curated vs
    /// directory entries.
    #[test]
    fn known_node_status_carries_source_field() {
        let status = KnownNodeStatus {
            zone: "dmp.example.com".to_string(),
            operator_name: "Example".to_string(),
            description: "test".to_string(),
            live: None,
            source: "directory".to_string(),
        };
        let s = serde_json::to_string(&status).unwrap();
        assert!(s.contains("\"source\":\"directory\""));
    }

    #[test]
    fn strip_endpoint_to_host_handles_common_shapes() {
        assert_eq!(strip_endpoint_to_host("https://dnsmesh.pro"), "dnsmesh.pro");
        assert_eq!(
            strip_endpoint_to_host("https://dnsmesh.pro/"),
            "dnsmesh.pro",
        );
        assert_eq!(
            strip_endpoint_to_host("https://dnsmesh.pro:8443"),
            "dnsmesh.pro",
        );
        assert_eq!(
            strip_endpoint_to_host("http://node.example.com:80/v1"),
            "node.example.com",
        );
        assert_eq!(strip_endpoint_to_host("https://[::1]:8443"), "::1");
        assert_eq!(strip_endpoint_to_host("https://[::1]"), "::1");
    }
}
