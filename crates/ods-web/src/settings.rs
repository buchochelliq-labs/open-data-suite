//! The Settings page (#351): a read-only view of the effective configuration, the
//! project, target and state store it resolves to, the providers it configures, and
//! `ods doctor`'s checks that only read. The binary resolves all of it and fills in
//! [`SettingsInput`]; this crate only presents it (ADR-0001).
//!
//! Nothing here writes, and nothing secret is shown: credentials are references by
//! construction (ADR-0005), shown as such, and the binary removes any credential a
//! plain value carries. Beyond loopback, values, paths and the checks' details are
//! left out, as on every other page.

use ods_core::CheckResult;
use serde::Serialize;

use crate::dashboard::DASHBOARD_SCHEMA_VERSION;

/// The configuration and what it resolves to, filled in by the binary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SettingsInput {
    /// The active profile and what selected it, if any.
    pub profile: Option<ProfileFacts>,
    /// The configuration files considered, lowest precedence first.
    pub files: Vec<FileFacts>,
    /// Every effective key, sorted, as `ods config explain` lists them.
    pub entries: Vec<SettingEntry>,
    /// The dbt project and target in use, as a run resolves them.
    pub project: Vec<Resolved>,
    /// The state store, as a run resolves it.
    pub state: Vec<Resolved>,
    /// The configured providers.
    pub providers: Vec<ProviderFacts>,
    /// `ods doctor`'s checks that only read, in its order.
    pub checks: Vec<CheckResult>,
    /// Why the configuration couldn't be read, if it couldn't; then nothing else is set.
    pub unreadable: Option<String>,
}

/// The active profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProfileFacts {
    /// Its name.
    pub name: String,
    /// What selected it, e.g. `ODS_PROFILE`.
    pub selected_by: String,
}

impl ProfileFacts {
    /// The profile `name`, selected by `selected_by`.
    pub fn new(name: impl Into<String>, selected_by: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            selected_by: selected_by.into(),
        }
    }
}

/// A configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct FileFacts {
    /// What it is: `user`, `project` or `local`.
    pub kind: String,
    /// Where it is.
    pub path: String,
    /// Whether it exists and was read.
    pub loaded: bool,
}

impl FileFacts {
    /// The `kind` file at `path`.
    pub fn new(kind: impl Into<String>, path: impl Into<String>, loaded: bool) -> Self {
        Self {
            kind: kind.into(),
            path: path.into(),
            loaded,
        }
    }
}

/// One effective key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SettingEntry {
    /// The dotted key, e.g. `providers.dbt.settings.target`.
    pub key: String,
    /// The value as ODS shows it: a secret as its reference, anything else without
    /// the credentials it may carry.
    pub value: String,
    /// Where it came from, as `ods config explain` says, e.g. `project file /p/ods.toml`.
    pub source: String,
    /// The same, without a path, e.g. `project file`.
    pub source_short: String,
    /// It is a secret reference.
    pub secret: bool,
    /// How many values it replaced, from lower-precedence sources.
    pub overrides: usize,
}

impl SettingEntry {
    /// `key` = `value`, from `source`.
    pub fn new(
        key: impl Into<String>,
        value: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            source_short: String::new(),
            source: source.into(),
            secret: false,
            overrides: 0,
        }
    }
}

/// A setting as a run resolves it: from a flag, the environment, the configuration
/// or a default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Resolved {
    /// What it is, for people, e.g. `Target`.
    pub label: String,
    /// Its value; `None` when nothing sets it (dbt's own default applies).
    pub value: Option<String>,
    /// Where it came from, e.g. `project config` or `DBT_TARGET`.
    pub origin: Option<String>,
    /// The configuration key that sets it, e.g. `state.db`.
    pub key: String,
    /// Whether it is a path, left out beyond loopback.
    pub path: bool,
}

impl Resolved {
    /// `label`, set at `key`, to `value` from `origin`.
    pub fn new(
        label: impl Into<String>,
        key: impl Into<String>,
        value: Option<String>,
        origin: Option<String>,
    ) -> Self {
        Self {
            label: label.into(),
            value,
            origin,
            key: key.into(),
            path: false,
        }
    }

    /// Marks it as a path.
    #[must_use]
    pub fn path(mut self) -> Self {
        self.path = true;
        self
    }
}

/// A configured provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProviderFacts {
    /// Its name in `[providers.<name>]`.
    pub name: String,
    /// Its kind, e.g. `dbt`.
    pub kind: String,
    /// What it can do, if the binary knows (ADR-0006).
    pub capabilities: Vec<String>,
    /// A warehouse plugin serves its kind: the About page says what it offers.
    pub warehouse_plugin: bool,
}

impl ProviderFacts {
    /// The provider `name`, of `kind`.
    pub fn new(name: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            capabilities: Vec::new(),
            warehouse_plugin: false,
        }
    }
}

/// The Settings page's view model, and `/api/settings`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SettingsView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Whether values, paths and the checks' details are shown: on loopback only.
    pub details: bool,
    /// The configuration, as given; beyond loopback without values, paths or the
    /// checks' details.
    #[serde(flatten)]
    pub settings: SettingsInput,
}

impl crate::Dashboard {
    /// The Settings page. `details` shows values, paths and the checks' details (on
    /// loopback only).
    pub fn settings(&self, details: bool) -> SettingsView {
        let mut settings = self.settings.clone();
        for provider in &mut settings.providers {
            provider.warehouse_plugin =
                self.about.plugins.iter().any(|p| {
                    p.kind == crate::about::PluginKind::Warehouse && p.name == provider.kind
                });
        }
        if !details {
            hide_details(&mut settings);
        }
        SettingsView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            details,
            settings,
        }
    }
}

/// What may be shown beyond loopback: keys, sources, names and statuses. A value can be
/// a path or a host, and a check's message or evidence can name either.
fn hide_details(settings: &mut SettingsInput) {
    for file in &mut settings.files {
        file.path = String::new();
    }
    for entry in &mut settings.entries {
        entry.value = String::new();
        entry.source = entry.source_short.clone();
    }
    for resolved in settings.project.iter_mut().chain(&mut settings.state) {
        resolved.value = None;
    }
    for check in &mut settings.checks {
        check.message = String::new();
        check.evidence.clear();
        check.hint = None;
    }
    settings.unreadable = settings.unreadable.as_ref().map(|_| String::new());
}
