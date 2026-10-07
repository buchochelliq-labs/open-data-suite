//! The About page (ADR-0031 §3c): this `ods`, and the plugins it runs with, as
//! detected, from the same detection as `ods plugin list`. The binary detects them and
//! fills in [`AboutInput`]; this crate only presents it (ADR-0001).

use serde::Serialize;

use crate::dashboard::DASHBOARD_SCHEMA_VERSION;

/// This `ods` and its plugins, filled in by the binary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct AboutInput {
    /// The `ods` binary's version.
    pub ods_version: String,
    /// The plugin SDK's contract version it was built against, e.g. `0.1`.
    pub sdk_version: String,
    /// Every plugin, as detected: health checks by id, then each warehouse.
    pub plugins: Vec<PluginFacts>,
}

impl AboutInput {
    /// `ods` at `ods_version`, built against SDK `sdk_version`, with `plugins`.
    pub fn new(
        ods_version: impl Into<String>,
        sdk_version: impl Into<String>,
        plugins: Vec<PluginFacts>,
    ) -> Self {
        Self {
            ods_version: ods_version.into(),
            sdk_version: sdk_version.into(),
            plugins,
        }
    }
}

/// What kind of plugin it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// A health check (ADR-0030 §5).
    HealthCheck,
    /// What ODS knows about a warehouse (ADR-0031 §3).
    Warehouse,
}

/// One plugin, as `ods plugin show` describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PluginFacts {
    /// The check's id, or the warehouse kind.
    pub name: String,
    /// What kind of plugin it is.
    pub kind: PluginKind,
    /// Where it comes from: a crate and its version.
    pub from: String,
    /// Whether the released `ods` has it.
    pub builtin: bool,
    /// The warehouses a warehouse is built on, nearest first (ADR-0031 §3b).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
    /// Who names them: the plugin, or `[warehouses.<kind>] extends`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parents_from: Option<String>,
    /// What it offers itself.
    pub features: Vec<FeatureFacts>,
    /// What a warehouse takes from those it is built on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inherited: Vec<InheritedFacts>,
}

impl PluginFacts {
    /// The plugin `name`, of `kind`, from `from`.
    pub fn new(
        name: impl Into<String>,
        kind: PluginKind,
        from: impl Into<String>,
        builtin: bool,
    ) -> Self {
        Self {
            name: name.into(),
            kind,
            from: from.into(),
            builtin,
            parents: Vec::new(),
            parents_from: None,
            features: Vec::new(),
            inherited: Vec::new(),
        }
    }
}

/// One thing a plugin offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct FeatureFacts {
    /// The feature, e.g. `source_versions`.
    pub name: String,
    /// The SDK contract it implements, if any (a dialect implements none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,
    /// That contract's version, e.g. `0.3`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_version: Option<String>,
    /// What a person needs to know: what it reads, its catalogue, its dialect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Why it is offered but can't be used as configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

impl FeatureFacts {
    /// The feature `name`, with nothing else known.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            contract: None,
            contract_version: None,
            detail: None,
            unavailable: None,
        }
    }
}

/// A capability a warehouse takes from one it is built on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct InheritedFacts {
    /// `errors` or `dialect`.
    pub feature: String,
    /// The warehouse it comes from.
    pub from: String,
    /// Its catalogue or dialect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl InheritedFacts {
    /// `feature`, taken from the warehouse `from`.
    pub fn new(feature: impl Into<String>, from: impl Into<String>) -> Self {
        Self {
            feature: feature.into(),
            from: from.into(),
            detail: None,
        }
    }
}

/// The About page's view model, and `/api/settings/about`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct AboutView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The `ods` binary's version.
    pub ods_version: String,
    /// The plugin SDK's contract version.
    pub sdk_version: String,
    /// The dashboard's HTTP API version ([`crate::API_VERSION`]).
    pub api_version: u32,
    /// The warehouse this project builds on, as dbt names it, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warehouse: Option<String>,
    /// Whether a plugin serves that warehouse. Without one, the warehouse still works:
    /// ODS uses the project's own connection, and only knows less about it (ADR-0031 §3).
    pub warehouse_served: bool,
    /// The warehouse plugins, in detection order.
    pub warehouses: Vec<PluginFacts>,
    /// The health-check plugins, in detection order.
    pub health_checks: Vec<PluginFacts>,
}

impl crate::Dashboard {
    /// The About page: this `ods`, and its plugins with the one serving this project's
    /// warehouse named.
    pub fn about(&self) -> AboutView {
        let warehouse = self.target.as_ref().and_then(|t| t.kind.clone());
        let (warehouses, health_checks): (Vec<_>, Vec<_>) = self
            .about
            .plugins
            .iter()
            .cloned()
            .partition(|p| p.kind == PluginKind::Warehouse);
        AboutView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            ods_version: self.about.ods_version.clone(),
            sdk_version: self.about.sdk_version.clone(),
            api_version: crate::API_VERSION,
            warehouse_served: warehouse
                .as_ref()
                .is_some_and(|w| warehouses.iter().any(|p| &p.name == w)),
            warehouse,
            warehouses,
            health_checks,
        }
    }
}
