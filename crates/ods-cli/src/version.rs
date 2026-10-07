//! `ods version`: build and compatibility information.

use ods_core::SchemaVersion;
use serde::Serialize;

use crate::plugins::Listed;
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
    /// The plugins this `ods` was built with: built in, or added by a custom build
    /// (ADR-0031 §2).
    pub plugins: Vec<Listed>,
}

impl VersionInfo {
    /// Information about the running binary.
    pub fn current() -> Self {
        Self {
            ods_version: env!("CARGO_PKG_VERSION"),
            sdk_version: ods_sdk::SDK_VERSION,
            output_schema_version: OUTPUT_SCHEMA_VERSION,
            plugins: crate::plugins::installed().listing(),
        }
    }
}

/// One plugin, for people: `change_provider 0.3 for databricks (ods-provider-databricks
/// 0.0.2, built in)`.
pub(crate) fn plugin_line(plugin: &Listed) -> String {
    format!(
        "{} {} for {} ({}{})",
        plugin.contract,
        plugin.contract_version,
        plugin.name,
        plugin.from,
        if plugin.builtin { ", built in" } else { "" }
    )
}

/// A schema or contract version as `major.minor`.
pub(crate) fn dotted(version: SchemaVersion) -> String {
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
            (
                "plugins".into(),
                vec![Span::plain(if self.plugins.is_empty() {
                    "none".to_owned()
                } else {
                    self.plugins
                        .iter()
                        .map(plugin_line)
                        .collect::<Vec<_>>()
                        .join("; ")
                })],
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
            plugins: crate::plugins::Plugins::builtin().listing(),
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
        assert_eq!(keys, ["ods", "sdk", "output schema", "plugins"]);
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
