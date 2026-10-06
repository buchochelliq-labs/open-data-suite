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
    /// State settings, used by `ods state` (#214); flags override them.
    #[serde(default)]
    pub state: StateConfig,
    /// Named provider instances, e.g. `providers.warehouse`.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Policy settings, consumed by the policy framework (#9).
    #[serde(default)]
    pub policy: PolicyConfig,
    /// Health checks: which run, how severe a failure is, and on which nodes
    /// (ADR-0030, #392). Check ids are validated by the health engine that reads them.
    #[serde(default)]
    pub health: HealthConfig,
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

/// `[state]`: defaults for `ods state` commands (#214).
///
/// Relative paths are read against the directory of the file that sets them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct StateConfig {
    /// The state database, as in `--state-db`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<String>,
    /// The environment whose state is kept, as in `--environment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// What an hour of build time costs, for `ods state savings` (ADR-0029).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostConfig>,
}

/// `[state.cost]`: what an hour of build time costs, to put a figure on what reuse saved
/// (ADR-0029). A rate and a label of the unit, never a price list or a credential: a
/// warehouse's own pricing stays out of ODS.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct CostConfig {
    /// The cost of one hour of build time, in [`unit`](Self::unit); at least 0.
    pub rate_per_hour: f64,
    /// What the rate is counted in, as shown, e.g. `USD` or `credits`.
    pub unit: String,
}

impl CostConfig {
    /// A rate of `rate_per_hour` `unit`s per hour of build time.
    pub fn new(rate_per_hour: f64, unit: impl Into<String>) -> Self {
        Self {
            rate_per_hour,
            unit: unit.into(),
        }
    }

    /// What `ms` of build time costs, in [`unit`](Self::unit).
    pub fn cost_of(&self, ms: u64) -> f64 {
        #[allow(
            clippy::cast_precision_loss,
            reason = "build times are far below 2^52 ms; the cost is an estimate"
        )]
        let hours = ms as f64 / 3_600_000.0;
        hours * self.rate_per_hour
    }
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

/// `[health]`: tunes the health checks (ADR-0030).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct HealthConfig {
    /// What a check that couldn't decide does to a node's badge: `unknown` (the
    /// default) or `warning`. Never healthy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_counts_as: Option<UnknownCountsAs>,
    /// The built-in checks, by id, e.g. `[health.builtin.tests_required]`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub builtin: BTreeMap<String, HealthCheckConfig>,
    /// Checks the project declares, e.g. `[[health.checks]]` requiring a description
    /// and a `unique` test of every mart (ADR-0030 §3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<DeclaredCheckConfig>,
}

/// A check declared in `[[health.checks]]` (ADR-0030 §3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct DeclaredCheckConfig {
    /// Its id, unique among the checks: lowercase letters, digits, `_`, `-` and `.`,
    /// starting with a letter, e.g. `marts.documented`.
    pub id: String,
    /// What kind of check it is. Only `declarative`, the default, is supported yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// What every selected node must have: `description`, `tests`, `test:<type>` (e.g.
    /// `test:unique`), `constraints` or `tag:<tag>`.
    #[serde(default)]
    pub require: Vec<String>,
    /// How severe its failure is (default `warn`), or `off` to not run it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<HealthSeverity>,
    /// The nodes it checks; every node when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<HealthSelector>,
    /// Nodes it never checks, even when selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<HealthSelector>,
}

/// What a check that couldn't decide makes a node's badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownCountsAs {
    /// Unknown: nothing vouches for it either way.
    Unknown,
    /// A warning.
    Warning,
}

/// One check's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct HealthCheckConfig {
    /// How severe its failure is, or `off` to not run it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<HealthSeverity>,
    /// The nodes it checks; replaces the check's own default scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<HealthSelector>,
    /// Nodes it never checks, even when selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<HealthSelector>,
}

/// A check's severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthSeverity {
    /// A failure makes the node failing, and fails `ods health check`.
    Error,
    /// A failure makes the node a warning.
    Warn,
    /// A failure is shown, and changes nothing.
    Info,
    /// The check doesn't run.
    Off,
}

/// Which nodes a check applies to: a node matches when it matches every field that
/// is set, and any value within a field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
#[non_exhaustive]
pub struct HealthSelector {
    /// Resource types, e.g. `model`, `seed`, `snapshot`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resource_type: Vec<String>,
    /// Tags; a node with any of them matches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Glob patterns over the node's file path, e.g. `models/marts/**`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    /// Node names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub name: Vec<String>,
}
