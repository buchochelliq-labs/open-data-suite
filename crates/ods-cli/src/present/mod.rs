//! Presentation boundary (ADR-0003): result models in, rendered output out.
//!
//! Modules hand `ods-cli` a serialisable result model. This module turns it into a
//! [`ViewNode`] tree for the human backends, or wraps it in the versioned JSON envelope.

pub mod backend;
pub mod view;

use std::io::{self, Write};

use ods_core::SchemaVersion;
use serde::Serialize;

pub use view::{Level, Line, Span, Tone, TreeItem, ViewNode};

use crate::exit::CliError;
use crate::output::{Mode, OutputSettings};

/// Version of the JSON envelope and every command's `result` shape (ADR-0003 §3).
pub const OUTPUT_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(0, 1);

/// A command result that can be shown to people and machines.
pub trait Present: Serialize {
    /// Dotted command identifier used in the JSON envelope, e.g. `state.plan`.
    const COMMAND: &'static str;

    /// Builds the human-facing view. Must be a pure function of `self`.
    fn view(&self) -> ViewNode;
}

/// Severity of a [`Diagnostic`]. Part of the JSON contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Severity {
    /// Informational.
    Info,
    /// Needs attention but did not fail.
    Warning,
    /// A failure.
    Error,
}

/// A non-fatal message attached to a JSON response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Diagnostic {
    /// How severe the message is.
    pub level: Severity,
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Human-readable explanation.
    pub message: String,
    /// Optional next step for the user; omitted from JSON when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// The single object written to stdout in JSON mode.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct Envelope<'a, T: Serialize> {
    schema_version: SchemaVersion,
    command: &'a str,
    ods_version: &'static str,
    /// `null` when the command failed; the failure is then in `diagnostics`.
    result: Option<&'a T>,
    diagnostics: &'a [Diagnostic],
}

fn write_envelope<T: Serialize>(
    out: &mut dyn Write,
    command: &str,
    result: Option<&T>,
    diagnostics: &[Diagnostic],
) -> io::Result<()> {
    let envelope = Envelope {
        schema_version: OUTPUT_SCHEMA_VERSION,
        command,
        ods_version: env!("CARGO_PKG_VERSION"),
        result,
        diagnostics,
    };
    serde_json::to_writer_pretty(&mut *out, &envelope)?;
    writeln!(out)
}

/// Renders `result` according to `settings` and writes it to `out`.
///
/// Human and plain backends render the whole view to a string first: rs-rich can only
/// be pointed at our stream through capture, and command results are small. JSON
/// serialises the result model directly, never the view (ADR-0003 §1).
///
/// # Errors
/// Returns any error from writing to `out` or serialising the result.
pub fn emit<T: Present>(
    result: &T,
    settings: &OutputSettings,
    out: &mut dyn Write,
) -> io::Result<()> {
    match settings.mode {
        Mode::Json => write_envelope(out, T::COMMAND, Some(result), &[]),
        Mode::Plain => out.write_all(backend::plain::render(&result.view()).as_bytes()),
        Mode::Human => {
            let renderer = backend::rich::RichRenderer::new(settings.color, settings.width);
            out.write_all(renderer.render(&result.view()).as_bytes())
        }
    }
}

/// Like [`emit`], for a command that produced a result but failed: in JSON mode the
/// envelope carries both the result and the error as a diagnostic. Other modes render
/// the result; the error goes to stderr as usual.
///
/// # Errors
/// Returns any error from writing to `out` or serialising the result.
pub fn emit_with_error<T: Present>(
    result: &T,
    error: &CliError,
    settings: &OutputSettings,
    out: &mut dyn Write,
) -> io::Result<()> {
    if settings.mode != Mode::Json {
        return emit(result, settings, out);
    }
    let diagnostic = Diagnostic {
        level: Severity::Error,
        code: error.code,
        message: error.message.clone(),
        hint: error.hint.clone(),
    };
    write_envelope(out, T::COMMAND, Some(result), &[diagnostic])
}

/// Writes a failed command's JSON envelope: `result` is `null` and the error is the
/// only diagnostic (ADR-0004 §4). Only used in JSON mode; other modes report errors
/// as text on stderr.
///
/// # Errors
/// Returns any error from writing to `out`.
pub fn emit_failure(out: &mut dyn Write, command: &str, error: &CliError) -> io::Result<()> {
    let diagnostic = Diagnostic {
        level: Severity::Error,
        code: error.code,
        message: error.message.clone(),
        hint: error.hint.clone(),
    };
    write_envelope::<()>(out, command, None, &[diagnostic])
}
