//! The plugin set (ADR-0031): the health checks, and the providers for each warehouse,
//! that this `ods` was built with.
//!
//! The CLI is the composition root (ADR-0001), so it alone maps a project's warehouse to
//! the providers that read it. The released `ods` holds the built-ins; a custom build
//! adds its own through [`Ods`](crate::Ods), and [`Ods::run`](crate::Ods::run) installs
//! the set once, as logging is set up once, for every command to read.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use ods_provider_databricks::{DeltaVersions, UnityCatalog};
use ods_sdk::Contract;
use ods_sdk::contracts::changes::{CHANGE_PROVIDER, ChangeProvider};
use ods_sdk::contracts::health_check::{CheckInfo, HEALTH_CHECK, HealthCheck};
use ods_sdk::contracts::privileges::{PrivilegedProbe, RELATION_PRIVILEGES};
use ods_sdk::contracts::probe::RelationProbe;
use serde::Serialize;

/// The crate a plugin comes from, for people: `ods version` and `ods doctor` name it,
/// so a custom build never passes for the released one (ADR-0031 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Origin {
    /// The crate's name.
    pub name: &'static str,
    /// The crate's version.
    pub version: &'static str,
}

/// The [`Origin`] of the crate this is written in.
#[macro_export]
macro_rules! origin {
    () => {
        $crate::plugins::Origin {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        }
    };
}

/// The providers one warehouse offers, built over the connection the project's own
/// tool already has (for dbt, its executor: ADR-0022 §1). ODS picks the plugin whose
/// [`warehouse`](Self::warehouse) is the project's warehouse kind, by exact match, here
/// in the CLI only (rule 1, ADR-0031 §3).
///
/// Every provider method defaults to `None`: a plugin offers only what it has, and
/// without one ODS behaves as if the warehouse had no plugin (no table versions;
/// probes need `--allow-elevated-login`).
pub trait WarehousePlugin: Send + Sync {
    /// Where the plugin comes from.
    fn origin(&self) -> Origin;

    /// The warehouse kind it serves, as the project names it (for dbt, the manifest's
    /// `adapter_type`, e.g. `snowflake`).
    fn warehouse(&self) -> &str;

    /// The contracts it offers, for listing: [`CHANGE_PROVIDER`] when
    /// [`changes`](Self::changes) gives one, [`RELATION_PRIVILEGES`] when
    /// [`privileges`](Self::privileges) does.
    fn provides(&self) -> Vec<Contract>;

    /// Reads sources' data versions through `probe` (ADR-0022).
    fn changes(&self, probe: Arc<dyn RelationProbe>) -> Option<Arc<dyn ChangeProvider>> {
        let _ = probe;
        None
    }

    /// Probes through `probe` and reports what its login may do (ADR-0030 §4c).
    fn privileges(&self, probe: Arc<dyn RelationProbe>) -> Option<Arc<dyn PrivilegedProbe>> {
        let _ = probe;
        None
    }
}

/// Why a plugin can't be added.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PluginError {
    /// A health check's id isn't well formed.
    #[error(
        "the health check id `{0}` isn't valid: lowercase letters, digits, `_`, `-` and `.`, starting with a letter"
    )]
    InvalidId(String),
    /// Two health checks share an id.
    #[error("two health checks are called `{0}`")]
    DuplicateCheck(String),
    /// Two plugins serve one warehouse.
    #[error(
        "a plugin for the warehouse `{warehouse}` is already registered (from {existing}); use `replacing_warehouse` to replace it"
    )]
    DuplicateWarehouse {
        /// The warehouse kind.
        warehouse: String,
        /// Where the registered plugin comes from.
        existing: String,
    },
    /// The plugin set was installed already.
    #[error("the plugin set is installed once per process, and already was")]
    AlreadyInstalled,
}

/// The health checks and warehouse plugins this `ods` runs with.
#[derive(Clone, Default)]
pub struct Plugins {
    checks: Vec<(Arc<dyn HealthCheck>, bool)>,
    warehouses: BTreeMap<String, (Arc<dyn WarehousePlugin>, bool)>,
}

impl std::fmt::Debug for Plugins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.listing()).finish()
    }
}

/// One plugin as `ods version` and `ods doctor` list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Listed {
    /// The contract it implements, e.g. `health_check`.
    pub contract: &'static str,
    /// That contract's version, e.g. `0.2`.
    pub contract_version: String,
    /// The check's id, or the warehouse kind.
    pub name: String,
    /// Where it comes from: a crate and its version.
    pub from: String,
    /// Whether the released `ods` has it.
    pub builtin: bool,
}

impl Plugins {
    /// No plugins at all.
    pub fn none() -> Self {
        Self::default()
    }

    /// What the released `ods` has: Databricks' table versions and login check.
    pub fn builtin() -> Self {
        let mut plugins = Self::none();
        plugins
            .warehouses
            .insert(DATABRICKS.to_owned(), (Arc::new(Databricks), true));
        plugins
    }

    /// Adds a health check.
    ///
    /// # Errors
    /// Its id isn't well formed, or another plugin check has it. A clash with a
    /// built-in or configured check is found when the checks are set up (exit 4).
    pub fn add_health_check(&mut self, check: Arc<dyn HealthCheck>) -> Result<(), PluginError> {
        let id = check.describe().id;
        if !CheckInfo::valid_id(&id) {
            return Err(PluginError::InvalidId(id));
        }
        if self.checks.iter().any(|(c, _)| c.describe().id == id) {
            return Err(PluginError::DuplicateCheck(id));
        }
        self.checks.push((check, false));
        Ok(())
    }

    /// Adds a warehouse plugin.
    ///
    /// # Errors
    /// Another plugin serves its warehouse: which one runs must never depend on the
    /// order they were added in. [`replace_warehouse`](Self::replace_warehouse) says so
    /// explicitly.
    pub fn add_warehouse(&mut self, plugin: Arc<dyn WarehousePlugin>) -> Result<(), PluginError> {
        let warehouse = plugin.warehouse().to_owned();
        if let Some((existing, _)) = self.warehouses.get(&warehouse) {
            let o = existing.origin();
            return Err(PluginError::DuplicateWarehouse {
                warehouse,
                existing: format!("{} {}", o.name, o.version),
            });
        }
        self.warehouses.insert(warehouse, (plugin, false));
        Ok(())
    }

    /// Adds a warehouse plugin in place of the one serving its warehouse, if any.
    pub fn replace_warehouse(&mut self, plugin: Arc<dyn WarehousePlugin>) {
        self.warehouses
            .insert(plugin.warehouse().to_owned(), (plugin, false));
    }

    /// The registered health checks, in the order they were added.
    pub fn health_checks(&self) -> impl Iterator<Item = &Arc<dyn HealthCheck>> {
        self.checks.iter().map(|(c, _)| c)
    }

    /// The plugin serving `warehouse`, if any.
    pub fn warehouse(&self, warehouse: Option<&str>) -> Option<&dyn WarehousePlugin> {
        self.warehouses.get(warehouse?).map(|(p, _)| p.as_ref())
    }

    /// Whether `warehouse` has a source-version provider.
    pub fn has_changes(&self, warehouse: Option<&str>) -> bool {
        self.warehouse(warehouse)
            .is_some_and(|p| p.provides().contains(&CHANGE_PROVIDER))
    }

    /// `warehouse`'s source-version provider, reading through `probe`.
    pub fn changes(
        &self,
        warehouse: Option<&str>,
        probe: Arc<dyn RelationProbe>,
    ) -> Option<Arc<dyn ChangeProvider>> {
        self.warehouse(warehouse)?.changes(probe)
    }

    /// `warehouse`'s login check, probing through `probe`.
    pub fn privileges(
        &self,
        warehouse: Option<&str>,
        probe: Arc<dyn RelationProbe>,
    ) -> Option<Arc<dyn PrivilegedProbe>> {
        self.warehouse(warehouse)?.privileges(probe)
    }

    /// Every plugin, for `ods version` and `ods doctor`: health checks by id, then each
    /// warehouse's contracts.
    pub fn listing(&self) -> Vec<Listed> {
        let mut listed: Vec<Listed> = self
            .checks
            .iter()
            .map(|(check, builtin)| {
                let info = check.info();
                Listed {
                    contract: HEALTH_CHECK.name,
                    contract_version: crate::version::dotted(HEALTH_CHECK.version),
                    name: check.describe().id,
                    from: format!("{} {}", info.kind, info.version),
                    builtin: *builtin,
                }
            })
            .collect();
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        for (warehouse, (plugin, builtin)) in &self.warehouses {
            let origin = plugin.origin();
            let mut contracts = plugin.provides();
            contracts.sort_by_key(|c| c.name);
            for contract in contracts {
                listed.push(Listed {
                    contract: contract.name,
                    contract_version: crate::version::dotted(contract.version),
                    name: warehouse.clone(),
                    from: format!("{} {}", origin.name, origin.version),
                    builtin: *builtin,
                });
            }
        }
        listed
    }
}

static INSTALLED: OnceLock<Plugins> = OnceLock::new();

/// Installs `plugins` for this process.
///
/// # Errors
/// A set was installed already, or a command already read the built-ins.
pub fn install(plugins: Plugins) -> Result<(), PluginError> {
    INSTALLED
        .set(plugins)
        .map_err(|_| PluginError::AlreadyInstalled)
}

/// The installed plugin set: the built-ins, unless [`install`] installed another.
pub fn installed() -> &'static Plugins {
    INSTALLED.get_or_init(Plugins::builtin)
}

/// The warehouse kind dbt calls Databricks.
const DATABRICKS: &str = "databricks";

/// Databricks: Delta table versions (ADR-0022) and Unity Catalog's login check
/// (ADR-0030 §4e).
struct Databricks;

impl WarehousePlugin for Databricks {
    fn origin(&self) -> Origin {
        Origin {
            name: "ods-provider-databricks",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn warehouse(&self) -> &str {
        DATABRICKS
    }

    fn provides(&self) -> Vec<Contract> {
        vec![CHANGE_PROVIDER, RELATION_PRIVILEGES]
    }

    fn changes(&self, probe: Arc<dyn RelationProbe>) -> Option<Arc<dyn ChangeProvider>> {
        Some(Arc::new(DeltaVersions::new(probe)))
    }

    fn privileges(&self, probe: Arc<dyn RelationProbe>) -> Option<Arc<dyn PrivilegedProbe>> {
        Some(Arc::new(UnityCatalog::new(probe)))
    }
}

#[cfg(test)]
mod tests {
    use ods_provider_fake::{FakeHealthCheck, FakeRelationProbe};
    use ods_sdk::contracts::health_check::Severity;

    use super::*;

    struct Other(&'static str);

    impl WarehousePlugin for Other {
        fn origin(&self) -> Origin {
            crate::origin!()
        }
        fn warehouse(&self) -> &str {
            self.0
        }
        fn provides(&self) -> Vec<Contract> {
            Vec::new()
        }
    }

    fn probe() -> Arc<dyn RelationProbe> {
        Arc::new(FakeRelationProbe::new())
    }

    #[test]
    fn the_builtins_serve_databricks_and_nothing_else() {
        let plugins = Plugins::builtin();
        assert!(plugins.has_changes(Some("databricks")));
        assert!(plugins.changes(Some("databricks"), probe()).is_some());
        assert!(plugins.privileges(Some("databricks"), probe()).is_some());
        for other in [None, Some("duckdb"), Some("Databricks")] {
            assert!(!plugins.has_changes(other));
            assert!(plugins.changes(other, probe()).is_none());
            assert!(plugins.privileges(other, probe()).is_none());
        }
        assert_eq!(
            plugins
                .listing()
                .iter()
                .map(|l| (l.contract, l.name.as_str(), l.builtin))
                .collect::<Vec<_>>(),
            [
                ("change_provider", "databricks", true),
                ("relation_privileges", "databricks", true),
            ]
        );
    }

    #[test]
    fn a_warehouse_is_served_once_unless_replaced_explicitly() {
        let mut plugins = Plugins::builtin();
        let err = plugins
            .add_warehouse(Arc::new(Other("databricks")))
            .unwrap_err();
        assert!(err.to_string().contains("replacing_warehouse"), "{err}");
        plugins.add_warehouse(Arc::new(Other("duckdb"))).unwrap();

        plugins.replace_warehouse(Arc::new(Other("databricks")));
        // The replacement offers nothing, so Databricks now has no table versions.
        assert!(!plugins.has_changes(Some("databricks")));
        assert!(plugins.listing().iter().all(|l| !l.builtin));
    }

    #[test]
    fn health_checks_need_a_valid_unique_id() {
        let mut plugins = Plugins::none();
        let bad =
            plugins.add_health_check(Arc::new(FakeHealthCheck::new("Owner!", Severity::Warn)));
        assert_eq!(bad, Err(PluginError::InvalidId("Owner!".to_owned())));
        plugins
            .add_health_check(Arc::new(FakeHealthCheck::new("owner", Severity::Warn)))
            .unwrap();
        let twice =
            plugins.add_health_check(Arc::new(FakeHealthCheck::new("owner", Severity::Warn)));
        assert_eq!(twice, Err(PluginError::DuplicateCheck("owner".to_owned())));
        let listed = &plugins.listing()[0];
        assert_eq!(
            (listed.contract, listed.name.as_str(), listed.builtin),
            ("health_check", "owner", false)
        );
    }
}
