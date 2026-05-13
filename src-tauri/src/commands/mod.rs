//! Tauri command shims that wrap the dnsmesh-rs SDK.
//!
//! Every command takes `tauri::State<'_, AppState>` to reach the active
//! client, awaits the SDK call, and converts errors into
//! [`crate::error::CommandError`] for serialisation. Read paths take
//! the `RwLock` read guard; identity-mutation paths take the write
//! guard because they replace the active [`DmpClient`].

pub mod backup;
pub mod contacts;
pub mod identity;
pub mod import_cli;
pub mod inbox;
pub mod intro;
pub mod messaging;
pub mod nodes;
