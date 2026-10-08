//! What a warehouse plugin reads from configuration and the environment (ADR-0031 §3a).

use std::collections::BTreeMap;
use std::fmt;

/// The settings of every `[providers.<name>]` whose `kind` is one warehouse, by name,
/// as configured, and the environment the plugin may read.
///
/// Secret references stay references (rule 9): a plugin is never handed a resolved
/// secret. `Debug` names the providers and their keys, never a value, since a value
/// such as a URL can carry a user part.
#[derive(Clone, Default)]
pub struct WarehouseSettings {
    providers: BTreeMap<String, toml::Table>,
    /// The environment, when set explicitly (tests, detection); else the process's.
    env: Option<BTreeMap<String, String>>,
}

impl fmt::Debug for WarehouseSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(
                self.providers
                    .iter()
                    .map(|(name, settings)| (name, settings.keys().collect::<Vec<_>>())),
            )
            .finish()
    }
}

impl WarehouseSettings {
    /// No providers, and an empty environment: what [detection](super::Detected) uses,
    /// so what a plugin offers never depends on where `ods` runs.
    pub fn empty() -> Self {
        Self {
            providers: BTreeMap::new(),
            env: Some(BTreeMap::new()),
        }
    }

    /// The providers of kind `warehouse` in `config`, and the process's environment.
    pub fn from_config(config: &ods_config::Config, warehouse: &str) -> Self {
        Self {
            providers: config
                .providers
                .iter()
                .filter(|(_, p)| p.kind == warehouse)
                .map(|(name, p)| (name.clone(), p.settings.clone()))
                .collect(),
            env: None,
        }
    }

    /// With the provider `name` configured with `settings`.
    #[must_use]
    pub fn with_provider(mut self, name: impl Into<String>, settings: toml::Table) -> Self {
        self.providers.insert(name.into(), settings);
        self
    }

    /// With exactly `vars` as the environment, in place of the process's.
    #[must_use]
    pub fn with_env<K: Into<String>, V: Into<String>>(
        mut self,
        vars: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        self.env = Some(
            vars.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        );
        self
    }

    /// The configured providers of this warehouse, by name, with their settings.
    pub fn providers(&self) -> &BTreeMap<String, toml::Table> {
        &self.providers
    }

    /// The environment variable `name`, if set.
    pub fn env(&self, name: &str) -> Option<String> {
        match &self.env {
            Some(vars) => vars.get(name).cloned(),
            None => std::env::var(name).ok(),
        }
    }
}
