//! Real OS credential store behaviour for the TSIG secret.
//!
//! The unit tests in `tsig_secret` use an in-memory stub, because they must
//! not write to a developer's keychain and CI runners have no Secret
//! Service. That leaves the actual backend untested, which is exactly where
//! a silent-stub bug already hid once.
//!
//! Integration tests link the library without `cfg(test)`, so these hit the
//! real store. Ignored by default; run deliberately:
//!
//! ```sh
//! cargo test --test tsig_keychain_real -- --ignored --nocapture
//! ```

use dnsmesh_app_lib::tsig_secret;

const KEY: [u8; 32] = [0x2a; 32];

fn probe_user(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("kctest-{tag}-{}-{}", std::process::id(), nanos)
}

/// Re-registering with a node mints a fresh TSIG secret and overwrites the
/// stored one. If the credential store silently keeps the first value, the
/// config names the new key while the stored secret is the old one, every
/// signed DNS UPDATE is rejected, and publish fails with no clue why.
#[test]
#[ignore = "writes to the real OS credential store"]
fn overwriting_a_stored_secret_actually_replaces_it() {
    let dir = tempfile::tempdir().unwrap();
    let user = probe_user("overwrite");

    tsig_secret::store(dir.path(), &user, b"base64:FIRSTsecretAAAA", &KEY).expect("first store");
    let first = tsig_secret::peek(dir.path(), &user, &KEY, None).expect("peek 1");
    assert_eq!(first.as_deref(), Some(&b"base64:FIRSTsecretAAAA"[..]));

    // Second registration for the same identity.
    tsig_secret::store(dir.path(), &user, b"base64:SECONDsecretBBB", &KEY).expect("second store");
    let second = tsig_secret::peek(dir.path(), &user, &KEY, None).expect("peek 2");

    assert_eq!(
        second.as_deref(),
        Some(&b"base64:SECONDsecretBBB"[..]),
        "the credential store returned a stale secret after an overwrite. \
         A re-registered identity would sign DNS UPDATEs with the previous \
         key's secret and every publish would be refused.",
    );

    tsig_secret::delete(dir.path(), &user).expect("cleanup");
}

/// Plain round trip against the real backend, so a broken or absent store
/// is caught rather than silently degrading.
#[test]
#[ignore = "writes to the real OS credential store"]
fn real_backend_round_trips_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let user = probe_user("roundtrip");

    assert!(
        tsig_secret::peek(dir.path(), &user, &KEY, None)
            .expect("peek on empty")
            .is_none(),
        "a name never stored should read back as absent",
    );

    tsig_secret::store(dir.path(), &user, b"hex:6b6579", &KEY).expect("store");
    assert_eq!(
        tsig_secret::peek(dir.path(), &user, &KEY, None)
            .expect("peek")
            .as_deref(),
        Some(&b"hex:6b6579"[..]),
        "the real credential store did not persist the secret",
    );

    tsig_secret::delete(dir.path(), &user).expect("delete");
    assert!(
        tsig_secret::peek(dir.path(), &user, &KEY, None)
            .expect("peek after delete")
            .is_none(),
        "delete did not remove the entry",
    );
}

/// Adoption must leave nothing readable behind and must return what the
/// plaintext held, byte for byte.
#[test]
#[ignore = "writes to the real OS credential store"]
fn adoption_moves_plaintext_into_the_real_store() {
    let dir = tempfile::tempdir().unwrap();
    let user = probe_user("adopt");
    let plain = tsig_secret::plaintext_path(dir.path());
    std::fs::write(&plain, b"base64:c2VjcmV0").unwrap();

    tsig_secret::adopt_plaintext(dir.path(), &user, &KEY, None).expect("adopt");
    assert!(!plain.exists(), "plaintext survived adoption");
    assert_eq!(
        tsig_secret::peek(dir.path(), &user, &KEY, None)
            .expect("peek")
            .as_deref(),
        Some(&b"base64:c2VjcmV0"[..]),
        "adoption lost the secret; publishing would break with the plaintext \
         already deleted",
    );

    tsig_secret::delete(dir.path(), &user).expect("cleanup");
}
