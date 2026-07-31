//! The desktop create-and-register flow, driven through the real Tauri
//! commands in the same order the Identities page calls them.
//!
//! Hits a live public node and publishes to real DNS, so it is `#[ignore]`d.
//! Run deliberately:
//!
//! ```sh
//! cargo test --test app_registration_e2e -- --ignored --nocapture
//! ```
//!
//! Why this exists: the CLI path was verified end to end, but the desktop
//! goes through different commands (`register_tsig`, `update_publish_config`)
//! and users were reporting registration failures the CLI never showed. A
//! test that calls what the UI calls is the only way to tell the two apart.

use dnsmesh_app_lib::commands::identity::{
    InitOrUnlockArgs, PublishConfigInput, UpdatePublishArgs, init_or_unlock, publish_identity,
    update_publish_config,
};
use dnsmesh_app_lib::commands::nodes::{list_known_nodes, register_tsig};
use dnsmesh_app_lib::state::AppState;
use tauri::test::MockRuntime;
use tauri::test::{mock_builder, mock_context, noop_assets};
use tauri::{App, Manager};

const ZONE: &str = "dmp.dnsmesh.io";

fn app_with_root(root: &std::path::Path) -> App<MockRuntime> {
    mock_builder()
        .manage(AppState::new(root.to_path_buf()))
        .build(mock_context(noop_assets()))
        .expect("mock app builds")
}

/// Unique per run so repeated runs do not collide on the node or in DNS.
fn probe_username() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("appreg{}{}", std::process::id(), nanos % 1000)
}

/// Create, register, wire publish config, publish. The exact sequence the
/// Identities page performs, with the same arguments.
#[tokio::test]
#[ignore = "hits the public network and publishes real DNS records"]
async fn desktop_create_register_publish_flow() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_root(dir.path());
    let username = probe_username();
    let passphrase = "desktop flow probe passphrase";
    println!("username: {username}");

    // The page picks a node from this list, so resolve the endpoint the
    // same way rather than hardcoding it.
    let nodes = list_known_nodes().await.expect("known nodes");
    let node = nodes
        .iter()
        .find(|n| n.zone == ZONE)
        .expect("curated list should contain the reference node");
    let endpoint = node
        .live
        .as_ref()
        .map(|l| l.endpoint.clone())
        .expect("reference node should be advertising a live endpoint");
    println!("endpoint: {endpoint}");

    // Stage 1: create.
    let created = init_or_unlock(
        InitOrUnlockArgs {
            username: username.clone(),
            passphrase: passphrase.to_string(),
            domain: Some(ZONE.to_string()),
            confirm_pin_verifier: false,
        },
        app.state(),
    )
    .await
    .expect("stage 1: identity creation");
    println!("created {}@{}", created.username, created.domain);

    // Stage 2: TSIG register. The page passes subject as user@domain and
    // the same passphrase, with the identity already unlocked.
    let registered = register_tsig(
        app.state(),
        endpoint.clone(),
        format!("{}@{}", created.username, created.domain),
        passphrase.to_string(),
    )
    .await
    .expect("stage 2: TSIG registration");
    println!(
        "registered key={} zone={} server={}",
        registered.key_name, registered.dns_zone, registered.dns_server
    );

    // The zone the node hands back must match the identity's own zone, or
    // every later publish targets a zone the key has no authority over.
    assert_eq!(
        registered.dns_zone, created.domain,
        "node returned a zone different from the identity's own; publishing \
         would be attempted against a zone the TSIG key cannot write",
    );

    // Stage 3: wire the publish block, exactly as the page does.
    let cfg = update_publish_config(
        UpdatePublishArgs {
            username: created.username.clone(),
            publish: Some(PublishConfigInput {
                zone: registered.dns_zone.clone(),
                server: format!("{}:53", registered.dns_server),
                tsig_key_name: registered.key_name.clone(),
                tsig_algorithm: registered.algorithm.clone(),
                tsig_secret_path: String::new(),
                tsig_secret_base64: Some(registered.secret_base64.clone()),
            }),
            resolvers: None,
            claim_via: None,
        },
        app.state(),
    )
    .await
    .expect("stage 3: wiring the publish block");
    assert!(cfg.publish.is_some(), "publish block should be persisted");

    // Stage 4: the page re-unlocks so the new writer takes effect.
    init_or_unlock(
        InitOrUnlockArgs {
            username: username.clone(),
            passphrase: passphrase.to_string(),
            domain: None,
            confirm_pin_verifier: false,
        },
        app.state(),
    )
    .await
    .expect("stage 4: re-unlock after wiring publish");

    // Stage 5: publish. This is what users report failing.
    publish_identity(app.state())
        .await
        .expect("stage 5: identity publish");
    println!("published");

    // And confirm it is actually in DNS, not merely reported as sent.
    let expected = {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(username.as_bytes());
        format!("id-{}.{}", &hex::encode(digest)[..16], ZONE)
    };
    println!("expecting record at {expected}");
    let found = dnsmesh_app_lib::state::build_reader(None)
        .expect("resolver pool")
        .query_txt_record(&expected)
        .await
        .expect("txt lookup");
    assert!(
        found.is_some_and(|v| !v.is_empty()),
        "identity was reported published but nothing resolves at {expected}",
    );
    println!("verified live in DNS");
}
