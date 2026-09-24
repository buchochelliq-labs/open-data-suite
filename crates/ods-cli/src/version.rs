//! `ods version`: build and compatibility information.

use ods_core::SchemaVersion;
use serde::Serialize;

use crate::present::{OUTPUT_SCHEMA_VERSION, Present, Span, Tone, ViewNode};

/// Result model for `ods version`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{ColorChoice, Mode, OutputSettings};
    use crate::present::emit;

    fn fixed() -> VersionInfo {
        VersionInfo {
            ods_version: "1.2.3",
            sdk_version: SchemaVersion::new(0, 1),
            output_schema_version: SchemaVersion::new(0, 1),
        }
    }

    fn render(mode: Mode) -> String {
        let settings = OutputSettings {
            mode,
            color: ColorChoice::Never,
            width: Some(100),
        };
        let mut out = Vec::new();
        emit(&fixed(), &settings, &mut out).unwrap();
        // The envelope's own `ods_version` is the build version; pin it for the snapshot.
        String::from_utf8(out)
            .unwrap()
            .replace(env!("CARGO_PKG_VERSION"), "[ods-version]")
    }

    #[test]
    fn view_lists_every_version() {
        let ViewNode::KeyValue(pairs) = fixed().view() else {
            panic!("expected key/value view")
        };
        let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, ["ods", "sdk", "output schema"]);
    }

    #[test]
    fn version_json() {
        insta::assert_snapshot!(render(Mode::Json));
    }

    #[test]
    fn version_plain() {
        insta::assert_snapshot!(render(Mode::Plain));
    }
}
