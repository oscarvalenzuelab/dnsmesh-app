//! Contact commands: list, manual add (hex pin), DNS fetch, delete.

use serde::{Deserialize, Serialize};
use tauri::State;

use dnsmesh_client::Contact;
use dnsmesh_storage::{ContactStore, OpenedDb};

use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

/// JS-friendly view of a [`Contact`]. Hex-encoded pubkeys; an empty
/// `domain` means "use the active client's own zone" per the SDK.
#[derive(Debug, Clone, Serialize)]
pub struct ContactView {
    pub username: String,
    pub domain: String,
    pub x25519_public_key_hex: String,
    pub ed25519_signing_public_key_hex: String,
}

impl From<Contact> for ContactView {
    fn from(c: Contact) -> Self {
        Self {
            username: c.username,
            domain: c.domain,
            x25519_public_key_hex: hex::encode(c.x25519_pk),
            ed25519_signing_public_key_hex: hex::encode(c.ed25519_spk),
        }
    }
}

/// List every pinned contact for the active identity.
#[tauri::command]
pub async fn list_contacts(state: State<'_, AppState>) -> CommandResult<Vec<ContactView>> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    let contacts = active.client.list_contacts().await?;
    Ok(contacts.into_iter().map(ContactView::from).collect())
}

/// Manually pin a contact whose pubkeys are known out-of-band (QR,
/// secure channel, etc.).
#[derive(Debug, Clone, Deserialize)]
pub struct AddContactArgs {
    pub username: String,
    pub domain: String,
    pub x25519_public_key_hex: String,
    pub ed25519_signing_public_key_hex: String,
}

/// Result of [`add_contact`].
#[derive(Debug, Clone, Serialize)]
pub struct AddContactResult {
    /// True iff this is the first time the username was pinned. False
    /// if an existing entry was overwritten.
    pub newly_added: bool,
    pub contact: ContactView,
}

fn parse_hex_32(field: &str, value: &str) -> CommandResult<[u8; 32]> {
    let bytes = hex::decode(value)
        .map_err(|e| CommandError::new("validation", format!("{field} is not valid hex: {e}")))?;
    if bytes.len() != 32 {
        return Err(CommandError::new(
            "validation",
            format!("{field} must decode to 32 bytes (got {})", bytes.len()),
        ));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[tauri::command]
pub async fn add_contact(
    args: AddContactArgs,
    state: State<'_, AppState>,
) -> CommandResult<AddContactResult> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;

    let username = args.username.trim().to_string();
    let domain = args.domain.trim().to_string();
    if username.is_empty() {
        return Err(CommandError::new(
            "validation",
            "username must not be empty",
        ));
    }
    if domain.is_empty() {
        return Err(CommandError::new("validation", "domain must not be empty"));
    }
    let x25519_pk = parse_hex_32("x25519_public_key_hex", &args.x25519_public_key_hex)?;
    let ed25519_spk = parse_hex_32(
        "ed25519_signing_public_key_hex",
        &args.ed25519_signing_public_key_hex,
    )?;
    let contact = Contact {
        username,
        x25519_pk,
        ed25519_spk,
        domain,
    };
    let newly_added = active.client.add_contact(contact.clone()).await?;
    Ok(AddContactResult {
        newly_added,
        contact: contact.into(),
    })
}

/// Fetch a verified [`Contact`] over DNS without persisting it. Used
/// by the "review before pin" flow.
#[derive(Debug, Clone, Deserialize)]
pub struct FetchIdentityArgs {
    /// `user@host`.
    pub address: String,
}

#[tauri::command]
pub async fn fetch_identity(
    args: FetchIdentityArgs,
    state: State<'_, AppState>,
) -> CommandResult<ContactView> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    let contact = active.client.fetch_identity(&args.address).await?;
    Ok(contact.into())
}

/// Drop a pinned contact from the active identity's local store.
///
/// `dnsmesh-client` doesn't yet expose `remove_contact`, so this opens
/// the per-identity sqlite directly via
/// `dnsmesh-storage::ContactStore`. SQLite WAL keeps the second
/// connection safe alongside the active `DmpClient`.
#[derive(Debug, Clone, Deserialize)]
pub struct DeleteContactArgs {
    pub username: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeleteContactResult {
    /// True iff the contact was present and removed; false on a missing
    /// username (idempotent).
    pub removed: bool,
}

#[tauri::command]
pub async fn delete_contact(
    args: DeleteContactArgs,
    state: State<'_, AppState>,
) -> CommandResult<DeleteContactResult> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    let username = args.username.trim().to_string();
    if username.is_empty() {
        return Err(CommandError::new(
            "validation",
            "username must not be empty",
        ));
    }
    let db_path = state.identity_db_path(&active.username);
    let db = OpenedDb::open(&db_path).map_err(|e| {
        CommandError::new(
            "storage",
            format!(
                "could not open contacts store at {}: {e}",
                db_path.display()
            ),
        )
    })?;
    let store = ContactStore::new(db);
    let removed = store
        .remove_contact(&username)
        .map_err(|e| CommandError::new("storage", format!("removing contact {username}: {e}")))?;
    Ok(DeleteContactResult { removed })
}

/// Fetch and pin in one round trip. Drives the "Add by address" flow.
#[tauri::command]
pub async fn fetch_and_add_contact(
    args: FetchIdentityArgs,
    state: State<'_, AppState>,
) -> CommandResult<AddContactResult> {
    let guard = state.active.read().await;
    let active = guard.as_ref().ok_or_else(CommandError::not_initialized)?;
    let contact = active.client.fetch_identity(&args.address).await?;
    let newly_added = active.client.add_contact(contact.clone()).await?;
    Ok(AddContactResult {
        newly_added,
        contact: contact.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_32_accepts_valid_hex() {
        let v = parse_hex_32("test", &"a".repeat(64)).unwrap();
        assert_eq!(v, [0xaa; 32]);
    }

    #[test]
    fn parse_hex_32_rejects_wrong_length() {
        let err = parse_hex_32("test", "aabb").unwrap_err();
        assert_eq!(err.kind, "validation");
    }

    #[test]
    fn parse_hex_32_rejects_invalid_hex() {
        let err = parse_hex_32("test", "zz").unwrap_err();
        assert_eq!(err.kind, "validation");
    }
}
