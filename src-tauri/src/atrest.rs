//! At-rest encryption for the per-identity files this app keeps outside
//! the SDK's database.
//!
//! The database is SQLCipher-encrypted by `dnsmesh-storage`, but the
//! desktop also persists the received message history (`inbox.jsonl`) and
//! the sent log (`sent.jsonl`). Those are plain files, so they get sealed
//! here under the same key — `DmpClient::storage_key()`, an HKDF output
//! over the identity's passphrase-derived seed.
//!
//! One record per line, each sealed independently:
//!
//! ```text
//! base64( nonce[12] || ChaCha20-Poly1305(plaintext) )
//! ```
//!
//! Per-record rather than whole-file so a single damaged line costs one
//! message instead of the entire history — matching how the loader already
//! treated a truncated write.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use rand::RngCore as _;

/// ChaCha20-Poly1305 nonce length.
const NONCE_LEN: usize = 12;

/// Seal one record. Returns the base64 line to write.
///
/// A fresh random nonce per call: these records are rewritten in bulk
/// (the whole file is re-emitted on append), so a counter would risk
/// reuse under the same key after a partial write.
pub fn seal(key: &[u8], plaintext: &[u8]) -> Result<String, String> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|_| "storage key must be 32 bytes".to_string())?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|_| "sealing record failed".to_string())?;
    let mut framed = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    framed.extend_from_slice(&nonce_bytes);
    framed.extend_from_slice(&ciphertext);
    Ok(BASE64_STANDARD.encode(framed))
}

/// Open one sealed record.
///
/// `None` for anything that doesn't decrypt cleanly — a truncated line, a
/// record from a different identity, or a leftover plaintext line from a
/// build that predates this. Callers skip those rather than failing the
/// whole load, so one bad record can't take the history down.
pub fn open(key: &[u8], line: &str) -> Option<Vec<u8>> {
    let framed = BASE64_STANDARD.decode(line.trim()).ok()?;
    if framed.len() <= NONCE_LEN {
        return None;
    }
    let (nonce, ciphertext) = framed.split_at(NONCE_LEN);
    let cipher = ChaCha20Poly1305::new_from_slice(key).ok()?;
    cipher.decrypt(Nonce::from_slice(nonce), ciphertext).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [0x2a; 32];

    #[test]
    fn seal_open_round_trip() {
        let sealed = seal(&KEY, b"hello world").unwrap();
        assert_eq!(open(&KEY, &sealed).as_deref(), Some(&b"hello world"[..]));
    }

    /// The point of the exercise: the plaintext must not be recoverable
    /// from the line itself.
    #[test]
    fn sealed_line_does_not_contain_the_plaintext() {
        let sealed = seal(&KEY, b"SENSITIVE-BODY").unwrap();
        assert!(!sealed.contains("SENSITIVE-BODY"));
        let raw = BASE64_STANDARD.decode(&sealed).unwrap();
        assert!(
            !raw.windows(14).any(|w| w == b"SENSITIVE-BODY"),
            "plaintext survived into the sealed bytes",
        );
    }

    #[test]
    fn wrong_key_does_not_open() {
        let sealed = seal(&KEY, b"hello").unwrap();
        assert_eq!(open(&[0x99; 32], &sealed), None);
    }

    /// Nonces must differ per record, or identical messages would be
    /// distinguishable and key reuse would bite.
    #[test]
    fn each_seal_uses_a_fresh_nonce() {
        let a = seal(&KEY, b"same").unwrap();
        let b = seal(&KEY, b"same").unwrap();
        assert_ne!(a, b, "identical plaintexts produced identical lines");
        assert_eq!(open(&KEY, &a), open(&KEY, &b));
    }

    /// Loader relies on this: junk must be skippable, never fatal.
    #[test]
    fn malformed_input_returns_none() {
        assert_eq!(open(&KEY, "not base64 at all !!!"), None);
        assert_eq!(open(&KEY, ""), None);
        assert_eq!(open(&KEY, &BASE64_STANDARD.encode([0u8; 4])), None);
        // A plaintext JSON line from a pre-encryption build.
        assert_eq!(open(&KEY, r#"{"msg_id_hex":"aa"}"#), None);
    }

    #[test]
    fn short_key_is_refused() {
        assert!(seal(&[0u8; 16], b"x").is_err());
    }
}
