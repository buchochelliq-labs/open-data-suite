//! Exit statuses and command errors (ADR-0004 §3, §4).
//!
//! Exit codes are a public contract: scripts and CI branch on them. Values never change
//! meaning; new meanings get new numbers below 64.

use std::io;

/// How `ods` exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExitStatus {
    /// The command did what was asked (also used when stdout closes early).
    Success,
    /// The command could not complete: I/O, provider or internal error.
    Failure,
    /// Invalid arguments or flags.
    Usage,
    /// The command is on the roadmap but not available yet.
    NotImplemented,
    /// Configuration or profile is invalid or missing (#7).
    Config,
    /// The command ran correctly and its verdict is negative (e.g. a CI gate failed).
    CheckFailed,
}

impl ExitStatus {
    /// Every status, for documentation and contract tests.
    pub const ALL: [ExitStatus; 6] = [
        ExitStatus::Success,
        ExitStatus::Failure,
        ExitStatus::Usage,
        ExitStatus::NotImplemented,
        ExitStatus::Config,
        ExitStatus::CheckFailed,
    ];

    /// The process exit code.
    pub const fn code(self) -> u8 {
        match self {
            ExitStatus::Success => 0,
            ExitStatus::Failure => 1,
            ExitStatus::Usage => 2,
            ExitStatus::NotImplemented => 3,
            ExitStatus::Config => 4,
            ExitStatus::CheckFailed => 5,
        }
    }

    /// Short name used in documentation and `--help`.
    pub const fn name(self) -> &'static str {
        match self {
            ExitStatus::Success => "success",
            ExitStatus::Failure => "failure",
            ExitStatus::Usage => "usage",
            ExitStatus::NotImplemented => "not implemented",
            ExitStatus::Config => "config",
            ExitStatus::CheckFailed => "check failed",
        }
    }
}

/// Stable identifiers for errors `ods` reports. Documented in `docs/cli.md`.
pub mod codes {
    /// Writing command output failed.
    pub const OUTPUT_WRITE: &str = "ODS-E0001";
    /// An unexpected internal error.
    pub const INTERNAL: &str = "ODS-E0002";
    /// The command is planned but not implemented yet.
    pub const NOT_IMPLEMENTED: &str = "ODS-E0003";
    /// `ODS_LOG` holds an unknown log level.
    pub const INVALID_LOG_LEVEL: &str = "ODS-E0004";
    /// dbt artifacts are missing, unreadable or unsupported, or output can't be written.
    pub const LINEAGE_ARTIFACTS: &str = "ODS-E0201";
    /// The project graph is inconsistent (duplicate ids or relations, a cycle).
    pub const LINEAGE_BUILD: &str = "ODS-E0202";
    /// A model, column, dialect or change named on the command line doesn't exist.
    pub const LINEAGE_TARGET: &str = "ODS-E0203";
    /// `ods serve` can't bind its address or stopped with an I/O error.
    pub const SERVE: &str = "ODS-E0301";
}

/// A failure returned by a command. Rendered once, by the framework, in the active
/// output mode; commands never print errors or exit themselves.
#[derive(Debug, thiserror::Error)]
#[error("error[{code}]: {message}{}", hint_suffix(.hint.as_deref()))]
pub struct CliError {
    /// Exit status to use.
    pub status: ExitStatus,
    /// Stable error identifier, e.g. `ODS-E0003`.
    pub code: &'static str,
    /// What went wrong.
    pub message: String,
    /// Optional next step for the user.
    pub hint: Option<String>,
    /// The underlying I/O error kind, when the failure came from I/O.
    io_kind: Option<io::ErrorKind>,
}

impl CliError {
    /// Creates an error.
    pub fn new(status: ExitStatus, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            hint: None,
            io_kind: None,
        }
    }

    /// Adds a hint.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Whether the failure came from writing output (so stdout cannot carry the report).
    pub fn is_output_failure(&self) -> bool {
        self.io_kind.is_some()
    }

    /// Whether the failure was stdout closing early (e.g. `ods … | head`), which is not
    /// an error for the user.
    pub fn is_broken_pipe(&self) -> bool {
        self.io_kind == Some(io::ErrorKind::BrokenPipe)
    }
}

impl From<io::Error> for CliError {
    fn from(err: io::Error) -> Self {
        Self {
            status: ExitStatus::Failure,
            code: codes::OUTPUT_WRITE,
            message: format!("failed to write output: {err}"),
            hint: None,
            io_kind: Some(err.kind()),
        }
    }
}

fn hint_suffix(hint: Option<&str>) -> String {
    hint.map(|hint| format!("\n  hint: {hint}"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_a_stable_contract() {
        let table: Vec<(u8, &str)> = ExitStatus::ALL
            .iter()
            .map(|s| (s.code(), s.name()))
            .collect();
        assert_eq!(
            table,
            [
                (0, "success"),
                (1, "failure"),
                (2, "usage"),
                (3, "not implemented"),
                (4, "config"),
                (5, "check failed"),
            ]
        );
    }

    #[test]
    fn display_includes_code_and_hint() {
        let err =
            CliError::new(ExitStatus::Failure, codes::INTERNAL, "boom").with_hint("try again");
        assert_eq!(err.to_string(), "error[ODS-E0002]: boom\n  hint: try again");
    }

    #[test]
    fn broken_pipe_is_recognised() {
        let err = CliError::from(io::Error::from(io::ErrorKind::BrokenPipe));
        assert!(err.is_broken_pipe());
        assert!(!CliError::from(io::Error::other("x")).is_broken_pipe());
    }
}
