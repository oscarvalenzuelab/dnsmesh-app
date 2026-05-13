//! Intro-queue Tauri commands.
//!
//! Mirrors the SDK's `list_intros / accept_intro / trust_intro /
//! block_intro` surface and shapes the JSON the SvelteKit `/intro`
//! page consumes. The queue itself lives in `dnsmesh-storage` and is
//! populated by the receive path whenever a pinned-mode receiver
//! decrypts a message from an un-pinned sender (the "first contact"
//! case).
//!
//! Trust UX 3-A:
//!
//! - `intro_list` returns the pending rows. `sender_label` (if set by
//!   the DMPv2 envelope verification) is the canonical `user@host`
//!   the UI should display.
//! - `intro_accept(intro_id)` promotes the row into the regular
//!   inbox without pinning the sender — handy when the user wants to
//!   read the message but isn't ready to add the sender as a
//!   contact.
//! - `intro_trust(intro_id, address)` accepts AND pins. The SDK
//!   re-resolves `address` via DNS, verifies the resolved
//!   `ed25519_spk` matches the queued one, then `add_contact`s; on
//!   mismatch the queue row stays and the contact list is
//!   untouched.
//! - `intro_block(intro_id, note)` drops the row + adds the sender
//!   SPK to a local denylist so future manifests from the same key
//!   skip the decrypt and the queue entirely.
//!
//! Accept and Trust both append the promoted message to the
//! persistent inbox via [`crate::commands::inbox`], so the user
//! sees it in their regular inbox feed alongside other delivered
//! messages.

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::inbox::PersistedInboxMessage;
use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

/// One quarantined intro the user can review.
///
/// `sender_spk_hex` is the lookup key for `intro_block` / contact
/// matching. `sender_label` is the SPK-verified `user@host` from
/// the inbound DMPv2 envelope when present — the UI should render
/// it instead of (or alongside) the SPK hex.
#[derive(Debug, Clone, Serialize)]
pub struct IntroView {
    pub intro_id: i64,
    pub sender_spk_hex: String,
    pub sender_label: Option<String>,
    pub msg_id_hex: String,
    pub plaintext_utf8: String,
    pub plaintext_bytes: Vec<u8>,
    pub received_at: u64,
    pub expires_at: u64,
}

/// Promote-to-inbox payload returned by `intro_accept` /
/// `intro_trust`. The frontend appends `message` to its persistent
/// inbox so the promoted plaintext shows up alongside regular inbox
/// rows after the user closes the intro dialog.
#[derive(Debug, Clone, Serialize)]
pub struct DeliveredIntroView {
    pub intro_id: i64,
    pub message: PersistedInboxMessage,
}

/// List every pending intro for the active identity, newest first.
#[tauri::command]
pub async fn intro_list(state: State<'_, AppState>) -> CommandResult<Vec<IntroView>> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    let pending = active.client.list_intros().await?;
    Ok(pending
        .into_iter()
        .map(|p| IntroView {
            intro_id: p.intro_id,
            sender_spk_hex: hex::encode(&p.sender_spk),
            sender_label: p.sender_username,
            msg_id_hex: hex::encode(&p.msg_id),
            plaintext_utf8: String::from_utf8_lossy(&p.payload).into_owned(),
            plaintext_bytes: p.payload,
            received_at: p.received_at,
            expires_at: p.expires_at,
        })
        .collect())
}

/// Args for [`intro_accept`].
#[derive(Debug, Clone, Deserialize)]
pub struct IntroAcceptArgs {
    pub intro_id: i64,
}

/// Accept an intro into the inbox WITHOUT pinning the sender.
///
/// Returns `None` if the `intro_id` is unknown (e.g. already taken by
/// a concurrent accept). The returned [`PersistedInboxMessage`] is
/// shaped so the frontend can hand it straight to `inbox_append`.
#[tauri::command]
pub async fn intro_accept(
    args: IntroAcceptArgs,
    state: State<'_, AppState>,
) -> CommandResult<Option<DeliveredIntroView>> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    let Some(delivered) = active.client.accept_intro(args.intro_id).await? else {
        return Ok(None);
    };
    Ok(Some(DeliveredIntroView {
        intro_id: delivered.intro_id,
        message: persisted_from(&delivered.message),
    }))
}

/// Args for [`intro_trust`].
#[derive(Debug, Clone, Deserialize)]
pub struct IntroTrustArgs {
    pub intro_id: i64,
    /// Canonical `user@host` for the sender. The SDK fetches the
    /// IdentityRecord at this address and refuses to pin unless its
    /// `ed25519_spk` matches the queued intro — that's the load-
    /// bearing trust check (the envelope's `from` claim is already
    /// SPK-verified at receive time, but `trust_intro` runs an
    /// independent fresh lookup so a stale cached label can't
    /// silently pin the wrong key).
    pub address: String,
}

/// Accept an intro AND pin the sender as a trusted contact.
///
/// Returns `Err(verify_failed)` when `address` resolves to a
/// different `ed25519_spk` than the queued intro — in that case the
/// queue row stays and the contact list is untouched.
#[tauri::command]
pub async fn intro_trust(
    args: IntroTrustArgs,
    state: State<'_, AppState>,
) -> CommandResult<Option<DeliveredIntroView>> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    let Some(delivered) = active
        .client
        .trust_intro(args.intro_id, &args.address)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(DeliveredIntroView {
        intro_id: delivered.intro_id,
        message: persisted_from(&delivered.message),
    }))
}

/// Args for [`intro_block`].
#[derive(Debug, Clone, Deserialize)]
pub struct IntroBlockArgs {
    pub intro_id: i64,
    /// Free-form local annotation stored next to the denylist entry.
    /// Never published to DNS; purely for the user's own record.
    #[serde(default)]
    pub note: String,
}

/// Result of [`intro_block`].
#[derive(Debug, Clone, Serialize)]
pub struct IntroBlockResult {
    /// `true` if a queue row was actually removed; `false` if the
    /// `intro_id` was unknown.
    pub removed: bool,
}

/// Drop the intro and add the sender SPK to the local denylist.
#[tauri::command]
pub async fn intro_block(
    args: IntroBlockArgs,
    state: State<'_, AppState>,
) -> CommandResult<IntroBlockResult> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    let removed = active.client.block_intro(args.intro_id, &args.note).await?;
    Ok(IntroBlockResult { removed })
}

/// Shape an SDK [`dnsmesh_client::InboxMessage`] into the
/// [`PersistedInboxMessage`] the persistent inbox accepts. Mirrors
/// [`crate::commands::messaging::InboxMessageView::from`] — kept
/// inline rather than going through that intermediate so the intro
/// flow doesn't pull a different module's view shape.
fn persisted_from(m: &dnsmesh_client::InboxMessage) -> PersistedInboxMessage {
    PersistedInboxMessage {
        sender_signing_pk_hex: hex::encode(m.sender_signing_pk),
        msg_id_hex: hex::encode(m.msg_id),
        timestamp: m.timestamp,
        plaintext_utf8: String::from_utf8_lossy(&m.plaintext).into_owned(),
        plaintext_bytes: m.plaintext.clone(),
        sender_label: m.sender_label.clone(),
    }
}
