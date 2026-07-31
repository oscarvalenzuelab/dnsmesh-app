//! Storage for the per-identity TSIG secret.
//!
//! The TSIG key authorises DNS UPDATE against the identity's zone — with
//! it, anyone can rewrite that identity's published records. It used to sit
//! in `<identity-dir>/tsig.key` as plaintext, readable by anything that
//! could read the directory.
//!
//! Two backends, because the platforms genuinely differ:
//!
//! - **Desktop** (macOS Keychain, Windows Credential Manager, Linux Secret
//!   Service) via `keyring`. The secret leaves the identity directory
//!   entirely and is guarded by the OS.
//! - **Android**: sealed in place under the identity's at-rest storage key,
//!   the same treatment `inbox.jsonl` gets.
//!
//! Android does not get the keychain because `keyring` has no Android
//! backend — it falls through to an in-memory `mock` that persists nothing.
//! That compiles cleanly and silently protects nothing, which is worse than
//! an honest sealed file. A real Android Keystore path is possible later;
//! it needs JNI or a Tauri plugin, and is tracked separately.
//!
//! Either way the on-disk plaintext is removed once the secret is stored.

use std::path::Path;

use crate::error::{CommandError, CommandResult};

/// In-memory stand-in used by the test suite.
///
/// Tests must not touch the developer's real keychain — they would leave
/// entries behind under names like `alice` — and CI has no credential store
/// at all: GitHub's Linux runners are headless with no D-Bus Secret
/// Service, so every `keyring` call there would fail.
///
/// This does mean the automated suite exercises the fallback rather than
/// the OS backend. That gap is worth naming, because it is exactly where a
/// real bug hid: `keyring` compiles fine without its per-platform backend
/// features and silently resolves to its own in-memory mock, so an earlier
/// version of this module "stored" secrets nowhere on macOS too. The
/// backend features in Cargo.toml are what prevent that, and they are load-
/// bearing — a round-trip against the real Keychain is what caught it.
#[cfg(test)]
static TEST_STORE: std::sync::Mutex<Option<std::collections::HashMap<String, Vec<u8>>>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn with_test_store<T>(f: impl FnOnce(&mut std::collections::HashMap<String, Vec<u8>>) -> T) -> T {
    let mut guard = TEST_STORE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(guard.get_or_insert_with(Default::default))
}

/// Legacy plaintext file name, still the import/export interchange format.
pub const TSIG_FILE: &str = "tsig.key";

/// Sealed file name used on platforms without a credential store.
const SEALED_FILE: &str = "tsig.key.sealed";

/// Service name under which desktop credential stores file our secrets.
#[cfg(all(has_credential_store, not(test)))]
const KEYRING_SERVICE: &str = "io.dnsmesh.app.tsig";

/// Where the sealed fallback lives for `username`.
fn sealed_path(identity_dir: &Path) -> std::path::PathBuf {
    identity_dir.join(SEALED_FILE)
}

/// Where the legacy plaintext lives for `username`.
pub fn plaintext_path(identity_dir: &Path) -> std::path::PathBuf {
    identity_dir.join(TSIG_FILE)
}

/// Persist `secret` for `username`, then remove any plaintext copy.
///
/// `storage_key` is only used by the sealed fallback; desktop ignores it.
pub fn store(
    identity_dir: &Path,
    username: &str,
    secret: &[u8],
    storage_key: &[u8],
) -> CommandResult<()> {
    store_inner(identity_dir, username, secret, storage_key)?;
    // Retire any plaintext copy; leaving one behind would mean the
    // credential stays readable despite this call reporting success.
    let plain = plaintext_path(identity_dir);
    if plain.exists() {
        std::fs::remove_file(&plain).map_err(|e| {
            CommandError::new(
                "io",
                format!(
                    "stored the TSIG secret but could not remove the plaintext at {}: {e}",
                    plain.display()
                ),
            )
        })?;
    }
    Ok(())
}

#[cfg(all(has_credential_store, not(test)))]
fn store_inner(
    _identity_dir: &Path,
    username: &str,
    secret: &[u8],
    _storage_key: &[u8],
) -> CommandResult<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, username)
        .map_err(|e| CommandError::new("io", format!("opening credential store: {e}")))?;
    entry.set_secret(secret).map_err(|e| {
        CommandError::new(
            "io",
            format!("writing TSIG secret to credential store: {e}"),
        )
    })
}

#[cfg(all(not(has_credential_store), not(test)))]
fn store_inner(
    identity_dir: &Path,
    _username: &str,
    secret: &[u8],
    storage_key: &[u8],
) -> CommandResult<()> {
    let sealed = crate::atrest::seal(storage_key, secret)
        .map_err(|e| CommandError::new("internal", format!("sealing TSIG secret: {e}")))?;
    std::fs::create_dir_all(identity_dir).map_err(CommandError::from)?;
    let path = sealed_path(identity_dir);
    let tmp = path.with_extension("sealed.tmp");
    std::fs::write(&tmp, sealed.as_bytes()).map_err(CommandError::from)?;
    std::fs::rename(&tmp, &path).map_err(CommandError::from)?;
    Ok(())
}

/// Read the secret for `username` without changing anything on disk.
///
/// Checks the store first, then falls back to a plaintext file — either
/// `<identity-dir>/tsig.key` or `configured_path`, since a publish block may
/// point somewhere else entirely. Identities imported from the CLI or
/// restored from an archive arrive as plaintext.
///
/// Deliberately does NOT adopt what it finds. Adoption re-keys the secret
/// and deletes the original, and this runs before the database has proved
/// the passphrase is right: on a platform without a credential store, a
/// wrong-but-confirmed passphrase would seal the secret under a key nobody
/// can reproduce and then remove the only readable copy. Call
/// [`adopt_plaintext`] once the unlock has succeeded.
///
/// `None` when no secret is configured at all.
pub fn peek(
    identity_dir: &Path,
    username: &str,
    storage_key: &[u8],
    configured_path: Option<&Path>,
) -> CommandResult<Option<Vec<u8>>> {
    if let Some(secret) = load_inner(identity_dir, username, storage_key)? {
        return Ok(Some(secret));
    }
    for candidate in plaintext_candidates(identity_dir, configured_path) {
        if candidate.is_file() {
            return Ok(Some(std::fs::read(&candidate).map_err(CommandError::from)?));
        }
    }
    Ok(None)
}

/// Move any plaintext secret into the store and delete the original.
///
/// Safe to call unconditionally after a successful unlock; a no-op when
/// there is nothing plaintext to adopt.
pub fn adopt_plaintext(
    identity_dir: &Path,
    username: &str,
    storage_key: &[u8],
    configured_path: Option<&Path>,
) -> CommandResult<()> {
    for candidate in plaintext_candidates(identity_dir, configured_path) {
        if candidate.is_file() {
            let raw = std::fs::read(&candidate).map_err(CommandError::from)?;
            store_inner(identity_dir, username, &raw, storage_key)?;
            std::fs::remove_file(&candidate).map_err(|e| {
                CommandError::new(
                    "io",
                    format!(
                        "stored the TSIG secret but could not remove the plaintext at {}: {e}",
                        candidate.display()
                    ),
                )
            })?;
        }
    }
    Ok(())
}

/// Plaintext locations to consider, in priority order.
///
/// The configured path first: when a publish block names one it is the
/// authority, and it may sit outside the identity directory.
fn plaintext_candidates(
    identity_dir: &Path,
    configured_path: Option<&Path>,
) -> Vec<std::path::PathBuf> {
    let default = plaintext_path(identity_dir);
    match configured_path {
        Some(p) if p != default.as_path() => vec![p.to_path_buf(), default],
        _ => vec![default],
    }
}

#[cfg(all(has_credential_store, not(test)))]
fn load_inner(
    _identity_dir: &Path,
    username: &str,
    _storage_key: &[u8],
) -> CommandResult<Option<Vec<u8>>> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, username)
        .map_err(|e| CommandError::new("io", format!("opening credential store: {e}")))?;
    match entry.get_secret() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(CommandError::new(
            "io",
            format!("reading TSIG secret from credential store: {e}"),
        )),
    }
}

#[cfg(all(not(has_credential_store), not(test)))]
fn load_inner(
    identity_dir: &Path,
    _username: &str,
    storage_key: &[u8],
) -> CommandResult<Option<Vec<u8>>> {
    let path = sealed_path(identity_dir);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(CommandError::from)?;
    Ok(crate::atrest::open(storage_key, raw.trim()))
}

/// Forget the secret for `username`, plaintext and sealed copies included.
/// Best-effort: a missing entry is success.
pub fn delete(identity_dir: &Path, username: &str) -> CommandResult<()> {
    delete_inner(username)?;
    let _ = std::fs::remove_file(sealed_path(identity_dir));
    let _ = std::fs::remove_file(plaintext_path(identity_dir));
    Ok(())
}

#[cfg(all(has_credential_store, not(test)))]
fn delete_inner(username: &str) -> CommandResult<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, username)
        .map_err(|e| CommandError::new("io", format!("opening credential store: {e}")))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CommandError::new(
            "io",
            format!("removing TSIG secret from credential store: {e}"),
        )),
    }
}

#[cfg(all(not(has_credential_store), not(test)))]
fn delete_inner(_username: &str) -> CommandResult<()> {
    Ok(())
}

// Signatures mirror the real backends so the call sites are identical;
// these simply never fail.
#[allow(clippy::unnecessary_wraps)]
#[cfg(test)]
fn store_inner(
    _identity_dir: &Path,
    username: &str,
    secret: &[u8],
    _storage_key: &[u8],
) -> CommandResult<()> {
    with_test_store(|m| m.insert(username.to_string(), secret.to_vec()));
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
#[cfg(test)]
fn load_inner(
    _identity_dir: &Path,
    username: &str,
    _storage_key: &[u8],
) -> CommandResult<Option<Vec<u8>>> {
    Ok(with_test_store(|m| m.get(username).cloned()))
}

#[allow(clippy::unnecessary_wraps)]
#[cfg(test)]
fn delete_inner(username: &str) -> CommandResult<()> {
    with_test_store(|m| m.remove(username));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const KEY: [u8; 32] = [0x2a; 32];

    /// Reading must not re-key or delete anything: it runs before the
    /// database has proved the passphrase, so adopting there could seal the
    /// secret under a key nobody can reproduce and destroy the only
    /// readable copy.
    #[test]
    fn peek_reads_plaintext_without_consuming_it() {
        let dir = TempDir::new().unwrap();
        let identity_dir = dir.path();
        let plain = plaintext_path(identity_dir);
        std::fs::write(&plain, b"base64:c2VjcmV0").unwrap();
        let username = format!("test-peek-{}", std::process::id());

        let seen = peek(identity_dir, &username, &KEY, None).unwrap();
        assert_eq!(seen.as_deref(), Some(&b"base64:c2VjcmV0"[..]));
        assert!(plain.exists(), "peek must leave the plaintext in place");

        delete(identity_dir, &username).unwrap();
    }

    /// Adoption is the step that re-keys and removes the plaintext, and it
    /// only runs after a successful unlock.
    #[test]
    fn adopt_moves_plaintext_into_the_store() {
        let dir = TempDir::new().unwrap();
        let identity_dir = dir.path();
        let plain = plaintext_path(identity_dir);
        std::fs::write(&plain, b"base64:c2VjcmV0").unwrap();
        let username = format!("test-adopt-{}", std::process::id());

        adopt_plaintext(identity_dir, &username, &KEY, None).unwrap();
        assert!(!plain.exists(), "adoption must retire the plaintext");
        assert_eq!(
            peek(identity_dir, &username, &KEY, None)
                .unwrap()
                .as_deref(),
            Some(&b"base64:c2VjcmV0"[..]),
        );

        // Idempotent: nothing left to adopt is not an error.
        adopt_plaintext(identity_dir, &username, &KEY, None).unwrap();
        delete(identity_dir, &username).unwrap();
    }

    /// A publish block may point the secret outside the identity
    /// directory. Ignoring that path silently disables publishing despite
    /// a perfectly good secret being configured.
    #[test]
    fn configured_path_outside_the_identity_dir_is_honoured() {
        let dir = TempDir::new().unwrap();
        let identity_dir = dir.path().join("identity");
        std::fs::create_dir_all(&identity_dir).unwrap();
        let elsewhere = dir.path().join("custom-tsig.key");
        std::fs::write(&elsewhere, b"hex:6b6579").unwrap();
        let username = format!("test-custom-{}", std::process::id());

        let seen = peek(&identity_dir, &username, &KEY, Some(&elsewhere)).unwrap();
        assert_eq!(seen.as_deref(), Some(&b"hex:6b6579"[..]));

        adopt_plaintext(&identity_dir, &username, &KEY, Some(&elsewhere)).unwrap();
        assert!(
            !elsewhere.exists(),
            "the configured file should be retired too"
        );
        assert_eq!(
            peek(&identity_dir, &username, &KEY, None)
                .unwrap()
                .as_deref(),
            Some(&b"hex:6b6579"[..]),
        );
        delete(&identity_dir, &username).unwrap();
    }

    #[test]
    fn absent_secret_reports_none() {
        let dir = TempDir::new().unwrap();
        let username = format!("test-absent-{}", std::process::id());
        assert!(peek(dir.path(), &username, &KEY, None).unwrap().is_none());
    }

    #[test]
    fn store_then_peek_round_trips() {
        let dir = TempDir::new().unwrap();
        let username = format!("test-roundtrip-{}", std::process::id());
        store(dir.path(), &username, b"hunter2-secret", &KEY).unwrap();
        assert_eq!(
            peek(dir.path(), &username, &KEY, None).unwrap().as_deref(),
            Some(&b"hunter2-secret"[..]),
        );
        delete(dir.path(), &username).unwrap();
        assert!(peek(dir.path(), &username, &KEY, None).unwrap().is_none());
    }

    /// Whatever the backend, nothing readable may be left in the identity
    /// directory.
    #[test]
    fn identity_dir_holds_no_readable_secret_after_store() {
        let dir = TempDir::new().unwrap();
        let username = format!("test-noplain-{}", std::process::id());
        store(dir.path(), &username, b"TOPSECRETVALUE", &KEY).unwrap();

        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let p = entry.unwrap().path();
            if p.is_file() {
                let raw = std::fs::read(&p).unwrap();
                assert!(
                    !raw.windows(14).any(|w| w == b"TOPSECRETVALUE"),
                    "secret readable in {}",
                    p.display(),
                );
            }
        }
        delete(dir.path(), &username).unwrap();
    }
}
