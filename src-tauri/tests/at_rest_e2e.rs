//! End-to-end tests for identity unlock and at-rest encryption.
//!
//! These drive the real `#[tauri::command]` functions against a real
//! `AppState` over a tempdir, so they exercise the whole path the UI does:
//! config.yaml handling, the passphrase verifier, `DmpClient` construction,
//! and the SQLCipher-backed database underneath it.
//!
//! Tauri's mock runtime supplies the `State` the commands need. Nothing here
//! touches the network — `DmpClient::new` is documented network-free, and no
//! test calls a publish or lookup path.

use dnsmesh_app_lib::commands::identity::{
    InitOrUnlockArgs, get_identity_info, init_or_unlock, lock_identity,
};
use dnsmesh_app_lib::state::AppState;
use tauri::test::MockRuntime;
use tauri::test::{mock_builder, mock_context, noop_assets};
use tauri::{App, Manager};

const PASS: &str = "correct horse battery staple";
const WRONG: &str = "x";
const ZONE: &str = "dmp.example.com";

/// Build an app with `AppState` rooted at `root`, mirroring how `lib.rs`
/// manages it in production.
fn app_with_root(root: &std::path::Path) -> App<MockRuntime> {
    mock_builder()
        .manage(AppState::new(root.to_path_buf()))
        .build(mock_context(noop_assets()))
        .expect("mock app builds")
}

fn create_args(username: &str, passphrase: &str) -> InitOrUnlockArgs {
    InitOrUnlockArgs {
        username: username.to_string(),
        passphrase: passphrase.to_string(),
        domain: Some(ZONE.to_string()),
        confirm_pin_verifier: false,
    }
}

fn unlock_args(username: &str, passphrase: &str) -> InitOrUnlockArgs {
    InitOrUnlockArgs {
        username: username.to_string(),
        passphrase: passphrase.to_string(),
        domain: None,
        confirm_pin_verifier: false,
    }
}

/// The reported bug, end to end: create an identity, lock it, then try to
/// re-open it with a different passphrase. That must fail rather than
/// silently present a second identity under the same name — and the
/// previously-active state must be left alone.
#[tokio::test]
async fn wrong_passphrase_is_rejected_and_leaves_state_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_root(dir.path());

    let created = init_or_unlock(create_args("alice", PASS), app.state())
        .await
        .expect("identity creates");
    let good_uid = created.user_id_hex.clone();

    lock_identity(app.state()).await.expect("locks");
    assert!(
        get_identity_info(app.state()).await.unwrap().is_none(),
        "nothing should be active after lock",
    );

    let err = init_or_unlock(unlock_args("alice", WRONG), app.state())
        .await
        .expect_err("wrong passphrase must not unlock");
    assert_eq!(err.kind, "wrong_passphrase", "got {err:?}");

    // The failed unlock must not have installed an active identity.
    assert!(
        get_identity_info(app.state()).await.unwrap().is_none(),
        "a rejected unlock must not populate state.active",
    );

    // `lock_identity` clears `index.active`, so it is None going in. The
    // property under test is that a rejected unlock does not set it — that
    // is what would otherwise mark a wrong-passphrase identity as the one
    // to auto-open on next launch. The identity itself must survive.
    let index = app.state::<AppState>().load_index().unwrap();
    assert_eq!(
        index.active, None,
        "a rejected unlock must not write index.active",
    );
    assert!(
        index.identities.iter().any(|e| e.username == "alice"),
        "the identity must still be registered",
    );

    // The right passphrase still opens the *same* identity.
    let reopened = init_or_unlock(unlock_args("alice", PASS), app.state())
        .await
        .expect("correct passphrase reopens");
    assert_eq!(
        reopened.user_id_hex, good_uid,
        "reopening must yield the same identity, not a new one",
    );
}

/// A wrong passphrase used to derive a *valid but different* keypair and
/// present it as the same identity. Assert the two are never conflated.
#[tokio::test]
async fn wrong_passphrase_never_yields_a_second_identity() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_root(dir.path());

    let created = init_or_unlock(create_args("alice", PASS), app.state())
        .await
        .expect("identity creates");
    lock_identity(app.state()).await.unwrap();

    for attempt in [
        "x",
        "correct horse battery stapl",
        "",
        "CORRECT HORSE BATTERY STAPLE",
    ] {
        let res = init_or_unlock(unlock_args("alice", attempt), app.state()).await;
        match res {
            Err(e) => assert!(
                e.kind == "wrong_passphrase" || e.kind == "validation",
                "unexpected error kind {:?} for attempt {attempt:?}",
                e.kind,
            ),
            Ok(info) => panic!(
                "attempt {attempt:?} unlocked and produced user_id {} (real is {})",
                info.user_id_hex, created.user_id_hex,
            ),
        }
    }
}

/// The database on disk must not surrender its contents to someone holding
/// the file but not the passphrase.
#[tokio::test]
async fn database_on_disk_is_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_root(dir.path());

    init_or_unlock(create_args("alice", PASS), app.state())
        .await
        .expect("identity creates");

    let db_path = app.state::<AppState>().identity_db_path("alice");
    assert!(
        db_path.is_file(),
        "db should exist at {}",
        db_path.display()
    );
    let raw = std::fs::read(&db_path).unwrap();

    // A plaintext sqlite file starts with this header; an encrypted one
    // does not. Checking the header rather than hunting for a canary means
    // this catches "not encrypted at all" regardless of table contents.
    assert!(
        !raw.starts_with(b"SQLite format 3\0"),
        "database is a plaintext sqlite file",
    );
    // Belt and braces: the username shouldn't be sitting in the file either.
    assert!(
        !raw.windows(5).any(|w| w == b"alice"),
        "found plaintext identity data in the database file",
    );
}

/// Creating an identity must pin a verifier, and a full-config rewrite
/// elsewhere must not be able to silently drop it. Reading it back through
/// the state layer is what the unlock path depends on.
#[tokio::test]
async fn creation_pins_a_verifier_matching_the_identity() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_root(dir.path());

    let created = init_or_unlock(create_args("alice", PASS), app.state())
        .await
        .expect("identity creates");

    let cfg = app
        .state::<AppState>()
        .load_identity_config("alice")
        .expect("config loads");
    assert_eq!(
        cfg.verifier_spk_hex.as_deref(),
        Some(created.ed25519_signing_public_key_hex.as_str()),
        "pinned verifier must be the identity's own signing key",
    );
    assert!(
        cfg.kdf_salt_base64.is_some(),
        "a per-identity salt must be persisted alongside the verifier",
    );
}

/// A database written before at-rest encryption cannot be opened by this
/// build. It has to be reported as its own thing, so the UI can say
/// "re-create this identity" instead of blaming the passphrase.
#[tokio::test]
async fn legacy_plaintext_database_surfaces_its_own_error_kind() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_root(dir.path());

    // Create normally so config.yaml carries a salt and verifier, then
    // replace the encrypted database with a plaintext one, the way an
    // identity from a pre-encryption build would look.
    let created = init_or_unlock(create_args("alice", PASS), app.state())
        .await
        .expect("identity creates");
    lock_identity(app.state()).await.unwrap();

    let db_path = app.state::<AppState>().identity_db_path("alice");
    for suffix in ["", "-wal", "-shm"] {
        let mut p = db_path.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(p));
    }
    {
        // Plain sqlite header + a table, with no SQLCipher key applied.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE t (x TEXT); INSERT INTO t VALUES ('legacy');")
            .unwrap();
    }

    let err = init_or_unlock(unlock_args("alice", PASS), app.state())
        .await
        .expect_err("a plaintext database must not open");
    assert_eq!(
        err.kind, "legacy_plaintext_db",
        "expected the dedicated kind so the UI can explain it, got {err:?}",
    );
    assert!(
        err.message.contains("unencrypted"),
        "message should say why: {}",
        err.message,
    );
    // Correct passphrase, so this must NOT be reported as a passphrase problem.
    assert_ne!(err.kind, "wrong_passphrase");
    assert!(
        get_identity_info(app.state()).await.unwrap().is_none(),
        "a failed unlock must leave nothing active",
    );
    drop(created);
}

/// Lockout regression: an identity whose database is already encrypted but
/// which has no verifier yet — what a CLI import looks like.
///
/// Confirming the prompt with a typo'd passphrase must NOT record that
/// passphrase's verifier. If it did, the real passphrase would be measured
/// against the wrong verifier on every later unlock and the identity would
/// be permanently unopenable.
#[tokio::test]
async fn confirming_a_wrong_passphrase_cannot_pin_a_lockout_verifier() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_root(dir.path());

    // Create normally, then strip the verifier to mimic an import: the
    // database stays encrypted under PASS, config keeps its salt.
    init_or_unlock(create_args("alice", PASS), app.state())
        .await
        .expect("identity creates");
    lock_identity(app.state()).await.unwrap();
    {
        let st = app.state::<AppState>();
        let mut cfg = st.load_identity_config("alice").unwrap();
        cfg.verifier_spk_hex = None;
        st.save_identity_config("alice", &cfg).unwrap();
    }

    // Unpinned, so an ordinary unlock asks for confirmation first.
    let err = init_or_unlock(unlock_args("alice", WRONG), app.state())
        .await
        .expect_err("unpinned identity must ask before pinning");
    assert_eq!(err.kind, "verifier_unpinned", "got {err:?}");

    // Now the dangerous path: confirm, but with the WRONG passphrase.
    let mut confirmed = unlock_args("alice", WRONG);
    confirmed.confirm_pin_verifier = true;
    let err = init_or_unlock(confirmed, app.state())
        .await
        .expect_err("a wrong passphrase must not open the encrypted database");
    assert_ne!(
        err.kind, "verifier_unpinned",
        "confirmation was given, so this should have moved past the prompt",
    );

    // The critical assertion: nothing was pinned.
    let cfg = app
        .state::<AppState>()
        .load_identity_config("alice")
        .unwrap();
    assert_eq!(
        cfg.verifier_spk_hex, None,
        "a failed unlock must not persist a verifier — doing so locks the \
         identity out permanently",
    );

    // And the real passphrase still works, pinning the correct verifier.
    let mut good = unlock_args("alice", PASS);
    good.confirm_pin_verifier = true;
    let opened = init_or_unlock(good, app.state())
        .await
        .expect("the real passphrase must still open the identity");
    let cfg = app
        .state::<AppState>()
        .load_identity_config("alice")
        .unwrap();
    assert_eq!(
        cfg.verifier_spk_hex.as_deref(),
        Some(opened.ed25519_signing_public_key_hex.as_str()),
        "a successful unlock should pin the correct verifier",
    );
}

/// The persisted message history must be opaque on disk. This is the
/// largest plaintext exposure the encryption work set out to close: the
/// database holds only pending intros, `inbox.jsonl` holds everything
/// received.
#[tokio::test]
async fn inbox_history_is_encrypted_on_disk() {
    use dnsmesh_app_lib::commands::inbox::{InboxAppendArgs, PersistedInboxMessage, inbox_append};

    const BODY: &str = "SENSITIVE-MESSAGE-BODY-CANARY";
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_root(dir.path());

    init_or_unlock(create_args("alice", PASS), app.state())
        .await
        .expect("identity creates");

    let appended = inbox_append(
        InboxAppendArgs {
            messages: vec![PersistedInboxMessage {
                sender_signing_pk_hex: "ab".repeat(32),
                msg_id_hex: "cd".repeat(16),
                timestamp: 1_700_000_000,
                plaintext_utf8: BODY.to_string(),
                plaintext_bytes: BODY.as_bytes().to_vec(),
                sender_label: Some("bob@dmp.example.com".to_string()),
            }],
        },
        app.state(),
    )
    .await
    .expect("append succeeds");
    assert_eq!(appended.appended, 1);

    let inbox_file = app
        .state::<AppState>()
        .identity_dir("alice")
        .join("inbox.jsonl");
    let raw = std::fs::read(&inbox_file).expect("inbox file exists");

    assert!(
        !raw.windows(BODY.len()).any(|w| w == BODY.as_bytes()),
        "message body found in plaintext in inbox.jsonl",
    );
    // The sender label is metadata but just as revealing, so check it too.
    assert!(
        !raw.windows(3).any(|w| w == b"bob"),
        "sender label found in plaintext in inbox.jsonl",
    );

    // And it still round-trips through the app for the unlocked identity.
    let rows = dnsmesh_app_lib::commands::inbox::inbox_load(app.state())
        .await
        .expect("inbox loads");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].plaintext_utf8, BODY);
}

/// Two identities under the same passphrase must still be distinct, since
/// each gets its own random Argon2id salt.
#[tokio::test]
async fn same_passphrase_yields_distinct_identities_per_username() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_root(dir.path());

    let alice = init_or_unlock(create_args("alice", PASS), app.state())
        .await
        .expect("alice creates");
    let bob = init_or_unlock(create_args("bob", PASS), app.state())
        .await
        .expect("bob creates");

    assert_ne!(
        alice.user_id_hex, bob.user_id_hex,
        "per-identity salts must keep same-passphrase identities distinct",
    );
}
