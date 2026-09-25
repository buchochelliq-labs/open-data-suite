//! Configuration errors. Each carries a stable code (see `docs/cli.md`) and says which
//! file, variable or flag caused it.

use std::path::PathBuf;

use crate::source::Source;

/// Why configuration could not be loaded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// A configuration file exists but could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// A configuration file is not valid TOML.
    #[error("{path} is not valid TOML: {message}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// The parser's message, including line and column.
        message: String,
    },
    /// A value does not match the schema (unknown key, wrong type, bad value).
    #[error("invalid configuration at `{key}` (from {origin}): {message}")]
    Schema {
        /// Dotted key path.
        key: String,
        /// The layer that set the offending value.
        origin: Box<Source>,
        /// What is wrong.
        message: String,
    },
    /// A credential is written as plaintext instead of a secret reference.
    #[error(
        "`{key}` (from {origin}) looks like a credential and must be a secret reference, \
         e.g. {{ secret = \"env:VAR\" }}; plaintext secrets are not allowed in configuration"
    )]
    PlaintextSecret {
        /// Dotted key path.
        key: String,
        /// The layer that set it.
        origin: Box<Source>,
    },
    /// A `{ secret = … }` reference is malformed. The reference text is never shown.
    #[error("invalid secret reference at `{key}` (from {origin}): {reason}")]
    InvalidSecretRef {
        /// Dotted key path.
        key: String,
        /// The layer that set it.
        origin: Box<Source>,
        /// What is wrong, without the reference text.
        reason: String,
    },
    /// The selected profile is not defined in any configuration file.
    #[error("profile `{name}` (selected by {selected_by}) is not defined{}", available_suffix(.available))]
    UnknownProfile {
        /// Requested profile.
        name: String,
        /// What selected it, e.g. `--profile` or `ODS_PROFILE`.
        selected_by: String,
        /// Profiles that are defined.
        available: Vec<String>,
    },
}

fn available_suffix(available: &[String]) -> String {
    if available.is_empty() {
        "; no profiles are defined".to_owned()
    } else {
        format!("; defined profiles: {}", available.join(", "))
    }
}

impl ConfigError {
    /// Stable error code (ADR-0004 §4).
    pub fn code(&self) -> &'static str {
        match self {
            ConfigError::Read { .. } | ConfigError::Parse { .. } => "ODS-E0101",
            ConfigError::Schema { .. } => "ODS-E0102",
            ConfigError::PlaintextSecret { .. } | ConfigError::InvalidSecretRef { .. } => {
                "ODS-E0103"
            }
            ConfigError::UnknownProfile { .. } => "ODS-E0104",
        }
    }
}

/// Joins a key path for display. Segments that are empty or contain `.`, spaces or
/// quotes are quoted, so `providers."a.b".kind` is never confused with a nested key.
pub fn display_key(key: &[String]) -> String {
    key.iter()
        .map(|segment| {
            let bare = !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '[' | ']'));
            if bare {
                segment.clone()
            } else {
                format!("{segment:?}")
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}
