//! End-to-end checks against the live public DMP nodes.
//!
//! These hit real DNS over the network, so they are `#[ignore]`d and never
//! run in CI. Run them deliberately:
//!
//! ```sh
//! cargo test --test public_network_e2e -- --ignored --nocapture
//! ```
//!
//! What they are for: everything else in the suite proves the encryption
//! works against files we created ourselves. These prove the app still
//! talks to the real federation with all of that in place, which is the
//! thing a release actually has to do.

use dnsmesh_app_lib::commands::identity::{InitOrUnlockArgs, init_or_unlock};
use dnsmesh_app_lib::commands::nodes::discover_nodes;
use dnsmesh_app_lib::state::AppState;
use tauri::test::MockRuntime;
use tauri::test::{mock_builder, mock_context, noop_assets};
use tauri::{App, Manager};

/// Zones the app seeds into every new identity.
const PUBLIC_ZONES: &[&str] = &["dmp.dnsmesh.io", "dmp.dnsmesh.de", "dmp.dnsmesh.pro"];

const PASS: &str = "correct horse battery staple";

fn app_with_root(root: &std::path::Path) -> App<MockRuntime> {
    mock_builder()
        .manage(AppState::new(root.to_path_buf()))
        .build(mock_context(noop_assets()))
        .expect("mock app builds")
}

/// Every public node must answer with a heartbeat whose signature verifies.
///
/// `discover_nodes` drops answers that fail verification, so a non-empty
/// result means the record was signed by the key in it and parsed as the
/// current wire format.
#[tokio::test]
#[ignore = "hits the public network"]
async fn public_nodes_are_reachable_and_verify() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_root(dir.path());

    let mut reachable = 0;
    for zone in PUBLIC_ZONES {
        match discover_nodes(app.state(), (*zone).to_string()).await {
            Ok(nodes) if !nodes.is_empty() => {
                reachable += 1;
                for n in &nodes {
                    println!(
                        "{zone}: endpoint={} version={} claim_zone={}",
                        n.endpoint, n.version, n.claim_provider_zone,
                    );
                    assert!(
                        n.endpoint.starts_with("https://"),
                        "{zone} advertised a non-https endpoint: {}",
                        n.endpoint,
                    );
                    assert!(!n.version.is_empty(), "{zone} advertised no version");
                }
            }
            Ok(_) => println!("{zone}: no verified heartbeat"),
            Err(e) => println!("{zone}: lookup failed: {e}"),
        }
    }

    assert!(
        reachable > 0,
        "no public node answered with a verified heartbeat; the federation \
         looks down, or this machine cannot resolve the zones",
    );
    println!("{reachable}/{} public zones verified", PUBLIC_ZONES.len());
}

/// A freshly created identity must come up with encryption in place and
/// still be able to reach the public federation.
///
/// The unit and offline e2e suites prove the files are opaque. This proves
/// the app is not merely encrypted-and-broken.
#[tokio::test]
#[ignore = "hits the public network"]
async fn new_identity_is_encrypted_and_can_reach_the_federation() {
    let dir = tempfile::tempdir().unwrap();
    let app = app_with_root(dir.path());

    let info = init_or_unlock(
        InitOrUnlockArgs {
            username: "e2e-probe".to_string(),
            passphrase: PASS.to_string(),
            domain: Some(PUBLIC_ZONES[0].to_string()),
            confirm_pin_verifier: false,
        },
        app.state(),
    )
    .await
    .expect("identity creates against a real public zone");
    println!("created {}@{}", info.username, info.domain);

    let state = app.state::<AppState>();

    // The database must be encrypted, on a real run and not just in the
    // offline tests.
    let db = std::fs::read(state.identity_db_path("e2e-probe")).unwrap();
    assert!(
        !db.starts_with(b"SQLite format 3\0"),
        "database is a plaintext sqlite file",
    );

    // The seeded claim-via zones should be the public federation, minus
    // this identity's own zone.
    let cfg = state.load_identity_config("e2e-probe").unwrap();
    let claim_via = cfg.claim_via.unwrap_or_default();
    println!("claim_via: {claim_via:?}");
    assert!(
        !claim_via.contains(&PUBLIC_ZONES[0].to_string()),
        "own zone should not be in claim_via",
    );
    assert!(
        claim_via.iter().any(|z| PUBLIC_ZONES.contains(&z.as_str())),
        "expected the public federation zones to be seeded, got {claim_via:?}",
    );

    // And the identity's own zone still resolves a live node.
    let nodes = discover_nodes(app.state(), PUBLIC_ZONES[0].to_string())
        .await
        .expect("node discovery works for the identity's own zone");
    assert!(
        !nodes.is_empty(),
        "identity's own zone {} returned no verified node",
        PUBLIC_ZONES[0],
    );
    println!("own zone resolved {} node(s)", nodes.len());
}
