//! Tauri host for the DNSMesh desktop client.
//!
//! Wraps the [`dnsmesh_client::DmpClient`] async API in a small set of
//! Tauri commands so the SvelteKit frontend can invoke identity,
//! contact, messaging, and diagnostic operations over the IPC bridge.
//!
//! The active [`DmpClient`] lives behind `RwLock<Option<…>>` inside
//! [`AppState`]. The on-disk identity index at
//! `~/.dmp/identities/index.yaml` is the source of truth for which
//! identities exist; the in-memory state only tracks the unlocked one.

pub mod commands;
pub mod error;
pub mod state;

use crate::state::AppState;

/// Returns the host crate version. Used by the frontend `+layout` to
/// confirm the IPC bridge is alive without unlocking an identity.
#[tauri::command]
fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Build the Tauri runtime.
///
/// Identities-root resolution differs by platform:
///   - Desktop: `$DMP_DESKTOP_HOME` > `$DMP_CONFIG_HOME/identities` >
///     `$HOME/.dmp/identities` (mirrors the CLI).
///   - Mobile (Android / iOS): the per-app local-data directory the OS
///     hands us via `app.path().app_local_data_dir()`. Mobile sandboxes
///     don't expose `$HOME` so the env-driven resolution can't run, and
///     identities have to live inside the app's private storage.
///
/// The mobile branch needs the `App` handle, so root resolution is
/// deferred into the `setup()` callback rather than computed before the
/// builder runs.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            use tauri::Manager;
            let root = if cfg!(any(target_os = "android", target_os = "ios")) {
                app.path()
                    .app_local_data_dir()
                    .map_err(|e| format!("app_local_data_dir unavailable: {e}"))?
                    .join("identities")
            } else {
                AppState::default_root()
                    .map_err(|e| format!("cannot resolve identities root: {e:#}"))?
            };
            std::fs::create_dir_all(&root)
                .map_err(|e| format!("cannot create identities root {}: {e}", root.display()))?;
            app.manage(AppState::new(root));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            version,
            // identity
            commands::identity::init_or_unlock,
            commands::identity::get_identity_info,
            commands::identity::publish_identity,
            commands::identity::refresh_prekeys,
            commands::identity::list_identities,
            commands::identity::switch_identity,
            commands::identity::lock_identity,
            commands::identity::get_identity_config,
            commands::identity::update_publish_config,
            commands::identity::is_identity_published,
            commands::identity::maybe_republish_identity,
            // contacts
            commands::contacts::list_contacts,
            commands::contacts::add_contact,
            commands::contacts::delete_contact,
            commands::contacts::fetch_identity,
            commands::contacts::fetch_and_add_contact,
            // messaging
            commands::messaging::send_message,
            commands::messaging::receive_messages,
            commands::messaging::receive_messages_diagnostic,
            // inbox (per-identity persistent store)
            commands::inbox::inbox_load,
            commands::inbox::inbox_append,
            commands::inbox::inbox_mark_read,
            commands::inbox::inbox_mark_all_read,
            commands::inbox::inbox_delete,
            // nodes
            commands::nodes::list_known_resolvers,
            commands::nodes::list_known_nodes,
            commands::nodes::doctor,
            commands::nodes::discover_nodes,
            commands::nodes::register_tsig,
            commands::nodes::effective_resolvers,
            commands::nodes::refresh_network,
            // import / backup
            commands::import_cli::import_from_cli,
            commands::backup::export_identity_backup,
            commands::backup::import_identity_backup,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Initialise tracing. Defaults to `info`; `RUST_LOG` overrides.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_command_returns_pkg_version() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }
}
