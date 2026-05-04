//! Send + receive commands. `receive_messages` is pull-only; the UI
//! drives polls via the Refresh button or a `setInterval`.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::State;

use dnsmesh_client::addressing::{slot_domain, SLOT_COUNT};
use dnsmesh_client::InboxMessage;
use dnsmesh_core::manifest::SlotManifest;

use crate::error::{CommandError, CommandResult};
use crate::state::{build_reader, AppState};

/// Args for [`send_message`].
#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageArgs {
    /// Recipient username (must be a pinned contact). The contact's
    /// domain is read from the local store.
    pub recipient_username: String,
    /// UTF-8 plaintext.
    pub plaintext: String,
}

/// Result of [`send_message`] — the 16-byte message ID, hex-encoded for JS.
#[derive(Debug, Clone, Serialize)]
pub struct SendMessageResult {
    pub msg_id_hex: String,
}

#[tauri::command]
pub async fn send_message(
    args: SendMessageArgs,
    state: State<'_, AppState>,
) -> CommandResult<SendMessageResult> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    if !active.publish_configured {
        return Err(CommandError::new(
            "publish_unconfigured",
            "send requires a configured publish (TSIG) destination — open Settings to add one",
        ));
    }
    if args.recipient_username.trim().is_empty() {
        return Err(CommandError::new(
            "validation",
            "recipient_username must not be empty",
        ));
    }
    let msg_id = active
        .client
        .send_message(&args.recipient_username, args.plaintext.as_bytes())
        .await?;
    Ok(SendMessageResult {
        msg_id_hex: hex::encode(msg_id),
    })
}

/// One decrypted inbox row. Plaintext is surfaced as both lossy UTF-8
/// (for display) and raw bytes (so binary payloads round-trip without
/// information loss). `sender_signing_pk_hex` is the lookup key
/// against the pinned contact list.
#[derive(Debug, Clone, Serialize)]
pub struct InboxMessageView {
    pub sender_signing_pk_hex: String,
    pub msg_id_hex: String,
    pub timestamp: u64,
    pub plaintext_utf8: String,
    pub plaintext_bytes: Vec<u8>,
}

impl From<InboxMessage> for InboxMessageView {
    fn from(m: InboxMessage) -> Self {
        let plaintext_utf8 = String::from_utf8_lossy(&m.plaintext).into_owned();
        Self {
            sender_signing_pk_hex: hex::encode(m.sender_signing_pk),
            msg_id_hex: hex::encode(m.msg_id),
            timestamp: m.timestamp,
            plaintext_utf8,
            plaintext_bytes: m.plaintext,
        }
    }
}

/// Pull every reassembled message out of the mailbox slots. Each call
/// drains everything visible and flips the replay cache, so a second
/// call returns only what arrived between polls.
#[tauri::command]
pub async fn receive_messages(state: State<'_, AppState>) -> CommandResult<Vec<InboxMessageView>> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    let messages = active.client.receive_messages().await?;
    Ok(messages.into_iter().map(InboxMessageView::from).collect())
}

/// One slot manifest the diagnostic walk found, plus the call's
/// verdict. `decision` mirrors the branches inside
/// `DmpClient::receive_messages`:
///
/// - `signature_invalid` — TXT didn't parse / Ed25519 verify failed.
/// - `recipient_mismatch` — manifest's `recipient_id` is not ours.
/// - `expired` — manifest `exp` has passed.
/// - `deliverable_pinned` — sender SPK is in our pinned set.
/// - `deliverable_tofu` — TOFU mode (no pinned contacts).
/// - `quarantine_intro` — pinned mode, un-pinned sender → intro queue.
///
/// Manifests from every polled zone are reported (not just the
/// freshest) so misdirected publishes are visible.
#[derive(Debug, Clone, Serialize)]
pub struct ManifestSeen {
    pub zone: String,
    pub slot: u32,
    pub sender_spk_hex: String,
    pub msg_id_hex: String,
    pub decision: String,
    pub note: String,
}

/// Diagnostic snapshot returned by [`receive_messages_diagnostic`].
///
/// Walks the same zone/slot space `DmpClient::receive_messages` walks
/// and reports both what was visible and the SDK's delivery verdict
/// per manifest. The real inbox population runs via the SDK call so
/// the diagnostic doesn't double-decrypt or desync the replay cache.
#[derive(Debug, Clone, Serialize)]
pub struct ReceiveDiagnostic {
    /// Identity the diagnostic ran against, in `user@domain` form.
    pub identity: String,
    /// SHA-256 of the X25519 pubkey, hex-encoded.
    pub recipient_id_hex: String,
    /// Zones polled, in walk order. Own zone first, then deduped
    /// pinned-contact zones.
    pub zones_polled: Vec<String>,
    /// `SLOT_COUNT`, exposed so the UI doesn't have to bake it in.
    pub slots_per_zone: u32,
    /// One entry per manifest seen (verified or not).
    pub manifests_found: Vec<ManifestSeen>,
    /// Pinned contact count at diagnostic time. Zero ⇒ TOFU mode.
    pub pinned_contacts: usize,
    /// `true` when the contact set was empty (TOFU mode).
    pub tofu_mode: bool,
    /// Messages the SDK delivered. A nonzero `manifests_found` with
    /// `inbox_count == 0` is the "wrong zone" pattern.
    pub inbox_count: usize,
    /// Free-form notes (transport failures, fallbacks, etc.) rendered
    /// above the manifest table.
    pub notes: Vec<String>,
}

/// Per-zone walker. Factored out so the diagnostic command stays under
/// clippy's `too_many_lines` threshold.
struct DiagnosticWalker<'a> {
    recipient_id: &'a [u8; 32],
    recipient_id_hex: &'a str,
    pinned: &'a HashSet<[u8; 32]>,
    tofu_mode: bool,
    now: u64,
}

impl DiagnosticWalker<'_> {
    /// Poll every slot in `zone` and emit a [`ManifestSeen`] per TXT.
    /// Per-query transport errors are recorded under `notes`.
    async fn walk_zone(
        &self,
        reader: &dyn dnsmesh_net::DnsRecordReader,
        zone: &str,
        manifests_found: &mut Vec<ManifestSeen>,
        notes: &mut Vec<String>,
    ) {
        for slot in 0..SLOT_COUNT {
            let name = slot_domain(self.recipient_id, slot, zone);
            let answers = match reader.query_txt_record(&name).await {
                Ok(Some(records)) => records,
                Ok(None) => continue,
                Err(e) => {
                    notes.push(format!("query {name} failed: {e}"));
                    continue;
                }
            };
            for record in &answers {
                manifests_found.push(self.classify_record(zone, slot, &name, record));
            }
        }
    }

    /// Classify one TXT. Mirrors `DmpClient::receive_messages`'s
    /// branches so the verdict matches what the SDK would do on a
    /// cold replay cache.
    fn classify_record(&self, zone: &str, slot: u32, name: &str, record: &str) -> ManifestSeen {
        let Some((manifest, _sig)) = SlotManifest::parse_and_verify(record) else {
            return ManifestSeen {
                zone: zone.to_string(),
                slot,
                sender_spk_hex: String::new(),
                msg_id_hex: String::new(),
                decision: "signature_invalid".into(),
                note: format!("TXT under {name} did not parse / verify"),
            };
        };
        let sender_hex = hex::encode(manifest.sender_spk);
        let msg_id_hex = hex::encode(manifest.msg_id);
        let (decision, note) = if manifest.recipient_id != *self.recipient_id {
            (
                "recipient_mismatch",
                format!(
                    "manifest names recipient_id {} but ours is {}",
                    hex::encode(manifest.recipient_id),
                    self.recipient_id_hex,
                ),
            )
        } else if manifest.is_expired(Some(self.now)) {
            (
                "expired",
                format!("exp={} (now={})", manifest.exp, self.now),
            )
        } else if self.pinned.contains(&manifest.sender_spk) {
            ("deliverable_pinned", String::new())
        } else if self.tofu_mode {
            (
                "deliverable_tofu",
                "no pinned contacts; TOFU mode accepts any verified manifest".into(),
            )
        } else {
            (
                "quarantine_intro",
                format!(
                    "sender {} is not in your pinned contacts — would land in the intro queue",
                    &sender_hex[..16.min(sender_hex.len())],
                ),
            )
        };
        ManifestSeen {
            zone: zone.to_string(),
            slot,
            sender_spk_hex: sender_hex,
            msg_id_hex,
            decision: decision.into(),
            note,
        }
    }
}

/// Run a verbose receive walk and report what the SDK would see.
///
/// Iterates the same zone/slot space as `DmpClient::receive_messages`.
/// The replay cache is `pub(crate)` inside the SDK, so
/// `decision = "deliverable_*"` describes a cold-cache outcome and a
/// re-run in the same process will still mark a previously-delivered
/// payload as deliverable. The SDK call is invoked at the end so
/// `inbox_count` reflects what actually landed.
#[tauri::command]
pub async fn receive_messages_diagnostic(
    state: State<'_, AppState>,
) -> CommandResult<ReceiveDiagnostic> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    let identity = format!("{}@{}", active.username, active.domain);
    let recipient_id = active.client.user_id();
    let recipient_id_hex = hex::encode(recipient_id);

    // The SDK's reader is `pub(crate)`. Build a parallel one using the
    // same resolver-overrides policy.
    let cfg = state
        .load_identity_config(&active.username)
        .map_err(CommandError::from)?;
    let mut notes: Vec<String> = Vec::new();
    let reader = match build_reader(cfg.resolvers.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            notes.push(format!(
                "diagnostic reader build failed: {e:#}; falling back to inbox-only count",
            ));
            // Inbox-only fallback: still useful to surface what the
            // live SDK reader can pull.
            let messages = active.client.receive_messages().await?;
            return Ok(ReceiveDiagnostic {
                identity,
                recipient_id_hex,
                zones_polled: vec![active.domain.clone()],
                slots_per_zone: SLOT_COUNT,
                manifests_found: Vec::new(),
                pinned_contacts: 0,
                tofu_mode: true,
                inbox_count: messages.len(),
                notes,
            });
        }
    };

    // Same zone walk `receive_messages` uses: own zone first, then
    // deduped pinned-contact zones.
    let contacts = active.client.list_contacts().await?;
    let pinned: HashSet<[u8; 32]> = contacts.iter().map(|c| c.ed25519_spk).collect();
    let tofu_mode = pinned.is_empty();

    let mut zones: Vec<String> = Vec::new();
    let mut seen_zone: HashSet<String> = HashSet::new();
    zones.push(active.domain.clone());
    seen_zone.insert(active.domain.clone());
    for c in &contacts {
        if c.domain.is_empty() {
            continue;
        }
        if seen_zone.insert(c.domain.clone()) {
            zones.push(c.domain.clone());
        }
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    let walker = DiagnosticWalker {
        recipient_id: &recipient_id,
        recipient_id_hex: &recipient_id_hex,
        pinned: &pinned,
        tofu_mode,
        now,
    };
    let mut manifests_found: Vec<ManifestSeen> = Vec::new();
    for zone in &zones {
        walker
            .walk_zone(reader.as_ref(), zone, &mut manifests_found, &mut notes)
            .await;
    }

    // Run the live SDK call so the diagnostic reports the same numbers
    // a regular Refresh would. Replay cache flips after this.
    let messages = active.client.receive_messages().await?;
    let inbox_count = messages.len();

    Ok(ReceiveDiagnostic {
        identity,
        recipient_id_hex,
        zones_polled: zones,
        slots_per_zone: SLOT_COUNT,
        manifests_found,
        pinned_contacts: contacts.len(),
        tofu_mode,
        inbox_count,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_message_view_round_trips_hex() {
        let m = InboxMessage {
            sender_signing_pk: [0xAB; 32],
            plaintext: b"hello".to_vec(),
            timestamp: 1_700_000_000,
            msg_id: [0xCD; 16],
        };
        let view: InboxMessageView = m.into();
        assert_eq!(view.sender_signing_pk_hex, "ab".repeat(32));
        assert_eq!(view.msg_id_hex, "cd".repeat(16));
        assert_eq!(view.plaintext_utf8, "hello");
    }

    /// Lock the wire shape consumed by the diagnostic panel.
    #[test]
    fn receive_diagnostic_serializes_with_expected_keys() {
        let diag = ReceiveDiagnostic {
            identity: "alice@mesh.example.com".into(),
            recipient_id_hex: "ab".repeat(32),
            zones_polled: vec!["mesh.example.com".into(), "other.zone".into()],
            slots_per_zone: SLOT_COUNT,
            manifests_found: vec![ManifestSeen {
                zone: "mesh.example.com".into(),
                slot: 3,
                sender_spk_hex: "cd".repeat(32),
                msg_id_hex: "ef".repeat(16),
                decision: "deliverable_pinned".into(),
                note: String::new(),
            }],
            pinned_contacts: 2,
            tofu_mode: false,
            inbox_count: 1,
            notes: vec!["heads up".into()],
        };
        let s = serde_json::to_string(&diag).expect("serialize");
        for needle in [
            "\"identity\":",
            "\"recipient_id_hex\":",
            "\"zones_polled\":",
            "\"slots_per_zone\":10",
            "\"manifests_found\":",
            "\"pinned_contacts\":2",
            "\"tofu_mode\":false",
            "\"inbox_count\":1",
            "\"notes\":",
            "\"zone\":\"mesh.example.com\"",
            "\"slot\":3",
            "\"sender_spk_hex\":",
            "\"msg_id_hex\":",
            "\"decision\":\"deliverable_pinned\"",
        ] {
            assert!(s.contains(needle), "expected {needle:?} in {s}");
        }
    }

    /// Decision strings drive the per-manifest status badge in the
    /// Inbox page — the TS switch must stay in sync with this set.
    #[test]
    fn manifest_seen_decisions_have_stable_strings() {
        let cases = [
            "signature_invalid",
            "recipient_mismatch",
            "expired",
            "deliverable_pinned",
            "deliverable_tofu",
            "quarantine_intro",
        ];
        for c in cases {
            let m = ManifestSeen {
                zone: "z".into(),
                slot: 0,
                sender_spk_hex: String::new(),
                msg_id_hex: String::new(),
                decision: c.into(),
                note: String::new(),
            };
            let s = serde_json::to_string(&m).unwrap();
            assert!(s.contains(&format!("\"decision\":\"{c}\"")));
        }
    }
}
