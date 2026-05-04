//! Error type surfaced to JavaScript via the Tauri command boundary.
//!
//! [`dnsmesh_client::ClientError`] and `anyhow::Error` are not directly
//! serialisable, so every command funnels through [`CommandError`].
//! The `kind` discriminant mirrors `dnsmesh_ffi::FfiError` so Tauri and
//! UniFFI consumers can share the same switch logic. Construction is
//! via `From` impls so commands can `?` SDK errors freely.

use serde::Serialize;
use serde_json::{json, Value as JsonValue};

/// Error envelope returned by every Tauri command.
#[derive(Debug, Clone, Serialize)]
pub struct CommandError {
    /// Stable kind string the UI switches on. One of: `validation`,
    /// `not_initialized`, `publish_unconfigured`, `invalid_config`,
    /// `contact_not_found`, `no_record_found`, `verify_failed`,
    /// `publish_failed`, `io`, `net`, `config`, `internal`, `sdk`.
    pub kind: String,
    /// Human-readable message. Safe to render verbatim.
    pub message: String,
    /// Optional structured payload mirroring the FFI variant fields,
    /// e.g. `{ "username": "alice" }` for `contact_not_found`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

impl CommandError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(
        kind: impl Into<String>,
        message: impl Into<String>,
        details: JsonValue,
    ) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            details: Some(details),
        }
    }

    pub fn not_initialized() -> Self {
        Self::new(
            "not_initialized",
            "no identity is unlocked — call init_or_unlock first",
        )
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for CommandError {}

impl From<dnsmesh_client::ClientError> for CommandError {
    fn from(err: dnsmesh_client::ClientError) -> Self {
        // Mirror the variant set in `dnsmesh_ffi::FfiError` so Tauri
        // and UniFFI consumers can share the same switch logic.
        use dnsmesh_client::ClientError as CE;
        let message = err.to_string();
        match err {
            CE::InvalidConfig(msg) => Self::new("invalid_config", msg),
            CE::InvalidAddress { address } => Self::with_details(
                "invalid_config",
                format!("invalid address `{address}`: must be in the form user@host"),
                json!({ "address": address }),
            ),
            CE::ContactNotFound { username } => Self::with_details(
                "contact_not_found",
                format!("contact `{username}` is not pinned"),
                json!({ "username": username }),
            ),
            CE::NoRecordFound { name } => Self::with_details(
                "no_record_found",
                format!("dns record not found at `{name}`"),
                json!({ "name": name }),
            ),
            CE::VerifyFailed { name } => Self::with_details(
                "verify_failed",
                format!("record at `{name}` failed verification"),
                json!({ "name": name }),
            ),
            CE::PublishFailed { kind, name } => Self::with_details(
                "publish_failed",
                format!("publish failed for {kind} at {name}"),
                json!({ "publish_kind": kind, "name": name }),
            ),
            CE::Net(_) | CE::Storage(_) => Self::new("io", message),
            CE::Crypto(_)
            | CE::Identity(_)
            | CE::Prekey(_)
            | CE::Manifest(_)
            | CE::Erasure(_)
            | CE::Chunking(_) => Self::new("internal", message),
        }
    }
}

impl From<anyhow::Error> for CommandError {
    fn from(err: anyhow::Error) -> Self {
        Self::new("internal", format!("{err:#}"))
    }
}

impl From<std::io::Error> for CommandError {
    fn from(err: std::io::Error) -> Self {
        Self::new("io", err.to_string())
    }
}

impl From<serde_yaml::Error> for CommandError {
    fn from(err: serde_yaml::Error) -> Self {
        Self::new("config", err.to_string())
    }
}

impl From<dnsmesh_net::NetError> for CommandError {
    fn from(err: dnsmesh_net::NetError) -> Self {
        Self::new("net", err.to_string())
    }
}

/// Convenience alias used by every command.
pub type CommandResult<T> = Result<T, CommandError>;
