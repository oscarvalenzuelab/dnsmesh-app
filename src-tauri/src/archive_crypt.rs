//! Passphrase encryption for the backup archive.
//!
//! The archive carries everything needed to be this identity: the
//! database, the message history, and the TSIG secret. It used to be a
//! plain `.tar.gz` with a warning in the UI, which made it the one place
//! all of that sat readable in a single file.
//!
//! Keyed by its own passphrase rather than the identity's. A backup is
//! restored on a machine that by definition does not have the identity
//! yet, so the identity's key is not available; and an archive that
//! outlives a passphrase rotation should not stay readable under the old
//! one.
//!
//! Envelope:
//!
//! ```text
//! "DMPBAK" || version(1) || salt(16) || nonce(12) || ciphertext
//! ```
//!
//! The salt and nonce are in the clear by necessity — they are inputs to
//! decryption, and neither is secret. Argon2id parameters match the SDK's
//! identity derivation, so archive passphrases are stretched as hard as
//! identity ones.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore as _;
use zeroize::Zeroizing;

use crate::error::{CommandError, CommandResult};

/// Envelope magic. Lets import tell an encrypted archive from a legacy
/// plaintext `.tar.gz` without guessing.
const MAGIC: &[u8; 6] = b"DMPBAK";

/// Envelope version. Bump on any layout change.
const VERSION: u8 = 1;

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// Byte offset where the ciphertext starts.
const HEADER_LEN: usize = MAGIC.len() + 1 + SALT_LEN + NONCE_LEN;

/// Argon2id parameters. Mirrors `dnsmesh-core`'s identity derivation so an
/// archive passphrase is no cheaper to attack than an identity one.
const ARGON2_MEMORY_KIB: u32 = 32 * 1024;
const ARGON2_TIME_COST: u32 = 2;
const ARGON2_PARALLELISM: u32 = 2;

fn derive_key(passphrase: &str, salt: &[u8]) -> CommandResult<Zeroizing<[u8; KEY_LEN]>> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_TIME_COST,
        ARGON2_PARALLELISM,
        Some(KEY_LEN),
    )
    .map_err(|e| CommandError::new("internal", format!("argon2 params: {e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|e| CommandError::new("internal", format!("deriving archive key: {e}")))?;
    Ok(key)
}

/// True if `bytes` looks like an encrypted archive.
///
/// Used so import can give a plaintext archive from an older build a clear
/// message instead of failing as though the passphrase were wrong.
#[must_use]
pub fn is_encrypted(bytes: &[u8]) -> bool {
    bytes.len() > HEADER_LEN && bytes.starts_with(MAGIC)
}

/// Wrap `plaintext` under `passphrase`.
pub fn encrypt(passphrase: &str, plaintext: &[u8]) -> CommandResult<Vec<u8>> {
    if passphrase.is_empty() {
        return Err(CommandError::new(
            "validation",
            "an archive passphrase is required",
        ));
    }
    let mut salt = [0u8; SALT_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    let mut nonce = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce);

    let key = derive_key(passphrase, &salt)?;
    let cipher = ChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|_| CommandError::new("internal", "archive key length"))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| CommandError::new("internal", "encrypting archive failed"))?;

    let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Unwrap an archive produced by [`encrypt`].
///
/// A wrong passphrase and a corrupt file are indistinguishable to the AEAD,
/// so both report `wrong_archive_passphrase`. That is the overwhelmingly
/// more likely cause, and the message says the file may also be damaged
/// rather than asserting which.
pub fn decrypt(passphrase: &str, bytes: &[u8]) -> CommandResult<Vec<u8>> {
    if !bytes.starts_with(MAGIC) {
        return Err(CommandError::new(
            "legacy_plaintext_archive",
            "this archive is not encrypted — it was exported by an older \
             version. Restore it with that version, or re-export it from an \
             install that can still open the identity.",
        ));
    }
    if bytes.len() <= HEADER_LEN {
        return Err(CommandError::new(
            "validation",
            "archive is truncated: no ciphertext after the header",
        ));
    }
    let version = bytes[MAGIC.len()];
    if version != VERSION {
        return Err(CommandError::new(
            "validation",
            format!("archive envelope version {version} is not supported (expected {VERSION})"),
        ));
    }
    let salt_at = MAGIC.len() + 1;
    let nonce_at = salt_at + SALT_LEN;
    let salt = &bytes[salt_at..nonce_at];
    let nonce = &bytes[nonce_at..HEADER_LEN];
    let ciphertext = &bytes[HEADER_LEN..];

    let key = derive_key(passphrase, salt)?;
    let cipher = ChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|_| CommandError::new("internal", "archive key length"))?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| {
            CommandError::new(
                "wrong_archive_passphrase",
                "could not decrypt the archive: wrong passphrase, or the file \
                 is damaged",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASS: &str = "correct horse battery staple";

    #[test]
    fn round_trips() {
        let sealed = encrypt(PASS, b"tar.gz bytes here").unwrap();
        assert_eq!(decrypt(PASS, &sealed).unwrap(), b"tar.gz bytes here");
    }

    #[test]
    fn wrong_passphrase_is_rejected_with_its_own_kind() {
        let sealed = encrypt(PASS, b"payload").unwrap();
        let err = decrypt("not the passphrase", &sealed).unwrap_err();
        assert_eq!(err.kind, "wrong_archive_passphrase");
    }

    /// The archive must not leak its contents to someone holding the file.
    #[test]
    fn ciphertext_does_not_contain_the_plaintext() {
        const CANARY: &[u8] = b"SENSITIVE-ARCHIVE-CONTENT-CANARY";
        let sealed = encrypt(PASS, CANARY).unwrap();
        assert!(!sealed.windows(CANARY.len()).any(|w| w == CANARY));
    }

    /// Two exports of identical content must not produce identical files,
    /// or an observer could tell that a backup was unchanged.
    #[test]
    fn each_encryption_uses_fresh_salt_and_nonce() {
        let a = encrypt(PASS, b"same").unwrap();
        let b = encrypt(PASS, b"same").unwrap();
        assert_ne!(a, b);
        assert_eq!(decrypt(PASS, &a).unwrap(), decrypt(PASS, &b).unwrap());
    }

    /// A plaintext archive from an older build gets its own error, so the
    /// UI can explain it rather than blaming the passphrase.
    #[test]
    fn legacy_plaintext_archive_is_named_as_such() {
        // Real gzip magic, as a legacy .tar.gz would start.
        let err = decrypt(PASS, &[0x1f, 0x8b, 0x08, 0x00, 0x00]).unwrap_err();
        assert_eq!(err.kind, "legacy_plaintext_archive");
    }

    #[test]
    fn truncated_and_unsupported_envelopes_are_rejected() {
        let mut short = MAGIC.to_vec();
        short.push(VERSION);
        assert_eq!(decrypt(PASS, &short).unwrap_err().kind, "validation");

        let mut wrong_version = encrypt(PASS, b"x").unwrap();
        wrong_version[MAGIC.len()] = 99;
        assert_eq!(
            decrypt(PASS, &wrong_version).unwrap_err().kind,
            "validation"
        );
    }

    #[test]
    fn empty_passphrase_is_refused() {
        assert_eq!(encrypt("", b"x").unwrap_err().kind, "validation");
    }

    #[test]
    fn is_encrypted_discriminates() {
        assert!(is_encrypted(&encrypt(PASS, b"payload").unwrap()));
        assert!(!is_encrypted(&[0x1f, 0x8b, 0x08, 0x00]));
        assert!(!is_encrypted(MAGIC), "header alone is not a valid archive");
    }

    /// Tampering must be caught, not silently decrypted.
    #[test]
    fn modified_ciphertext_fails_authentication() {
        let mut sealed = encrypt(PASS, b"payload").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xff;
        assert_eq!(
            decrypt(PASS, &sealed).unwrap_err().kind,
            "wrong_archive_passphrase",
        );
    }
}
