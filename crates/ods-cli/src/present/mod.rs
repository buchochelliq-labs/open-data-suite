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

/// A non-fatal message attached to a JSON response.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    /// Severity: `info`, `warning` or `error`.
    pub level: &'static str,
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Human-readable explanation.
    pub message: String,
}

/// The single object written to stdout in JSON mode.
#[derive(Debug, Serialize)]
struct Envelope<'a, T: Serialize> {
    schema_version: SchemaVersion,
    command: &'static str,
    ods_version: &'static str,
    result: &'a T,
    diagnostics: &'a [Diagnostic],
}

/// Renders `result` according to `settings` and writes it to `out`.
///
/// # Errors
/// Returns any error from writing to `out` or serialising the result.
pub fn emit<T: Present>(
    result: &T,
    settings: &OutputSettings,
    out: &mut dyn Write,
) -> io::Result<()> {
    match settings.mode {
        Mode::Json => {
            let envelope = Envelope {
                schema_version: OUTPUT_SCHEMA_VERSION,
                command: T::COMMAND,
                ods_version: env!("CARGO_PKG_VERSION"),
                result,
                diagnostics: &[],
            };
            serde_json::to_writer_pretty(&mut *out, &envelope)?;
            writeln!(out)
        }
        Mode::Plain => out.write_all(backend::plain::render(&result.view()).as_bytes()),
        Mode::Human => {
            let renderer = backend::rich::RichRenderer::new(settings.color, settings.width);
            out.write_all(renderer.render(&result.view()).as_bytes())
        }
    }
}
