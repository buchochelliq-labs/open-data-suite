//! The validated configuration schema (ADR-0005 §3).
//!
//! Every table rejects unknown keys, so a typo is an error rather than a silently
//! ignored setting. Provider and policy settings stay provider-neutral: their inner
//! keys are validated by the provider (#2) or policy engine (#9) that consumes them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The only configuration format version this build understands.
pub const CONFIG_VERSION: u32 = 1;

/// Effective configuration after all layers are merged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct Config {
    /// Configuration format version; must be [`CONFIG_VERSION`] when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// Profile used when neither `--profile` nor `ODS_PROFILE` selects one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    /// Project identity.
    #[serde(default)]
    pub project: ProjectConfig,
    /// Output defaults; command-line flags override them.
    #[serde(default)]
    pub output: OutputConfig,
    /// Logging defaults; `-v`/`-q` and `ODS_LOG` override them.
    #[serde(default)]
    pub log: LogConfig,
    /// Named provider instances, e.g. `providers.warehouse`.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Policy settings, consumed by the policy framework (#9).
    #[serde(default)]
    pub policy: PolicyConfig,
}

/// `[project]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProjectConfig {
    /// Display name of the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `[output]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct OutputConfig {
    /// Default output format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<OutputFormat>,
    /// Default colour choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<ColorPreference>,
    /// Default render width in columns (at least 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u16>,
}

/// Output format names, as in `--output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// Styled terminal output.
    Human,
    /// Stable, uncoloured text.
    Plain,
    /// Versioned JSON envelope.
    Json,
}

/// Colour choices, as in `--color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorPreference {
    /// Colour on terminals.
    Auto,
    /// Always colour.
    Always,
    /// Never colour.
    Never,
}

/// `[log]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct LogConfig {
    /// Default log level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<LogLevel>,
}

/// Log level names, as in `ODS_LOG`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// No logging.
    Off,
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// Informational.
    Info,
    /// Debugging detail.
    Debug,
    /// Everything.
    Trace,
}

/// `[providers.<name>]`: one configured provider instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProviderConfig {
    /// Which provider implementation, e.g. `dbt` or `databricks`. ODS core never
    /// branches on this value; the CLI uses it to pick a provider (ADR-0001).
    pub kind: String,
    /// Provider-specific settings, validated by the provider. Credentials must be
    /// secret references (ADR-0005 §4).
    #[serde(default)]
    pub settings: toml::Table,
}

/// `[policy]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct PolicyConfig {
    /// Policy rules, interpreted by the policy framework (#9).
    #[serde(default)]
    pub rules: toml::Table,
}
