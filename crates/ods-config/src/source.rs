//! Where a configuration value came from (ADR-0005 §2).

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

/// Which configuration file a value was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Per-user file, e.g. `~/.config/ods/config.toml`.
    User,
    /// Project file, `ods.toml`, committed with the project.
    Project,
    /// Local overrides, `.ods/local.toml`, not committed.
    Local,
}

impl FileKind {
    /// Short name used in explanations.
    pub const fn name(self) -> &'static str {
        match self {
            FileKind::User => "user",
            FileKind::Project => "project",
            FileKind::Local => "local",
        }
    }
}

/// The layer that set a value, in ascending precedence order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "layer")]
#[non_exhaustive]
pub enum Source {
    /// Built into ODS.
    Default,
    /// A configuration file.
    File {
        /// Which file.
        kind: FileKind,
        /// Its path.
        path: PathBuf,
    },
    /// The active profile's section in a configuration file.
    Profile {
        /// Profile name.
        name: String,
        /// Which file defined it.
        kind: FileKind,
        /// Its path.
        path: PathBuf,
    },
    /// An `ODS__…` environment variable.
    Env {
        /// Variable name.
        var: String,
    },
    /// A command-line flag.
    Flag {
        /// Flag as written, e.g. `--output`.
        flag: String,
    },
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Default => f.write_str("default"),
            Source::File { kind, path } => write!(f, "{} file {}", kind.name(), path.display()),
            Source::Profile { name, kind, path } => {
                write!(
                    f,
                    "profile `{name}` in {} file {}",
                    kind.name(),
                    path.display()
                )
            }
            Source::Env { var } => write!(f, "environment {var}"),
            Source::Flag { flag } => write!(f, "flag {flag}"),
        }
    }
}
