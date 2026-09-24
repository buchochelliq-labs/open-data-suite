//! `ods version`: build and compatibility information.

use ods_core::SchemaVersion;
use serde::Serialize;

use crate::present::{OUTPUT_SCHEMA_VERSION, Present, Span, Tone, ViewNode};

/// Result model for `ods version`.
#[derive(Debug, Serialize)]
pub struct VersionInfo {
    /// Version of the `ods` binary.
    pub ods_version: &'static str,
    /// Plugin SDK contract version this binary was built against.
    pub sdk_version: SchemaVersion,
    /// Version of the JSON output envelope.
    pub output_schema_version: SchemaVersion,
}

impl VersionInfo {
    /// Information about the running binary.
    pub fn current() -> Self {
        Self {
            ods_version: env!("CARGO_PKG_VERSION"),
            sdk_version: ods_sdk::SDK_VERSION,
            output_schema_version: OUTPUT_SCHEMA_VERSION,
        }
    }
}

fn dotted(version: SchemaVersion) -> String {
    format!("{}.{}", version.major, version.minor)
}

impl Present for VersionInfo {
    const COMMAND: &'static str = "version";

    fn view(&self) -> ViewNode {
        ViewNode::KeyValue(vec![
            (
                "ods".into(),
                vec![Span::toned(self.ods_version, Tone::Code)],
            ),
            ("sdk".into(), vec![Span::plain(dotted(self.sdk_version))]),
            (
                "output schema".into(),
                vec![Span::plain(dotted(self.output_schema_version))],
            ),
        ])
    }
}
