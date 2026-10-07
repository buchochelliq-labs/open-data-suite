//! The plugin set (ADR-0031): the health checks, and the providers for each warehouse,
//! that this `ods` was built with.
//!
//! The CLI is the composition root (ADR-0001), so it alone maps a project's warehouse to
//! the providers that read it. The released `ods` holds the built-ins; a custom build
//! adds its own through [`Ods`](crate::Ods), and [`Ods::run`](crate::Ods::run) installs
//! the set once, as logging is set up once, for every command to read.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use ods_sdk::ProviderError;
use ods_sdk::contracts::changes::ChangeProvider;
use ods_sdk::contracts::health_check::{CheckInfo, HealthCheck};
use ods_sdk::contracts::observed_lineage::ObservedLineageSource;
use ods_sdk::contracts::privileges::PrivilegedProbe;
use ods_sdk::contracts::probe::RelationProbe;
use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLinker};
use serde::Serialize;

mod databricks;
mod detect;
mod settings;

pub use detect::{Detected, Feature};
pub use settings::WarehouseSettings;

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

/// What ODS knows about one warehouse, built over the connection the project's own
/// tool already has (for dbt, its executor: ADR-0022 §1). ODS picks the plugin whose
/// [`warehouse`](Self::warehouse) is the project's warehouse kind, by exact match, here
/// in the CLI only (rule 1, ADR-0031 §3, §3a).
///
/// Every method but the first two is a factory with a default meaning "not offered":
/// a plugin offers only what it has, and without one ODS behaves as if the warehouse
/// had no plugin (no table versions; probes need `--allow-elevated-login`; no links).
/// What a plugin offers is detected by calling them (§3c), never declared.
pub trait WarehousePlugin: Send + Sync {
    /// Where the plugin comes from.
    fn origin(&self) -> Origin;

    /// The warehouse kind it serves, as the project names it (for dbt, the manifest's
    /// `adapter_type`, e.g. `snowflake`).
    fn warehouse(&self) -> &str;

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

    /// What [`changes`](Self::changes) reads, for people: the Freshness screen says
    /// runs read it. Without one, it says the plugin's warehouse and crate.
    fn versions_read(&self) -> Option<String> {
        None
    }

    /// Links from relations to where the warehouse's own UI shows them (#329), built
    /// from `settings`; or why there are none. A reason other than
    /// [`NoRelationLink::Unsupported`] means links are offered but can't be built, as
    /// when a host isn't configured: the reason is what people see.
    ///
    /// # Errors
    /// Why there are no links.
    fn links(
        &self,
        settings: &WarehouseSettings,
    ) -> Result<Arc<dyn RelationLinker>, NoRelationLink> {
        let _ = settings;
        Err(NoRelationLink::Unsupported {
            warehouse: Some(self.warehouse().to_owned()),
        })
    }

    /// Reads the observed lineage the user exported from the warehouse to `export`
    /// (ADR-0008). `Some` means the plugin reads exports, even if this one can't be read.
    fn observed_lineage(
        &self,
        export: &Path,
    ) -> Option<Result<Arc<dyn ObservedLineageSource>, ProviderError>> {
        let _ = export;
        None
    }

    /// The SQL dialect its SQL is parsed in, as the shared parser names it (e.g.
    /// `databricks`). Without one, the warehouse kind is mapped as before. A name the
    /// parser doesn't know refuses the plugin when it is added.
    fn dialect(&self) -> Option<&str> {
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
    /// A warehouse plugin names a dialect the SQL parser doesn't know.
    #[error(
        "the plugin for the warehouse `{warehouse}` names the SQL dialect `{dialect}`, which isn't one of: {known}"
    )]
    UnknownDialect {
        /// The warehouse kind.
        warehouse: String,
        /// The dialect it named.
        dialect: String,
        /// The dialects the parser knows.
        known: String,
    },
    /// The plugin set was installed already.
    #[error("the plugin set is installed once per process, and already was")]
    AlreadyInstalled,
}

/// A reader of observed lineage, or why the export can't be read.
pub type ObservedReader = Result<Arc<dyn ObservedLineageSource>, ProviderError>;

/// The health checks and warehouse plugins this `ods` runs with.
#[derive(Clone, Default)]
pub struct Plugins {
    checks: Vec<(Arc<dyn HealthCheck>, Origin, bool)>,
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

    /// What the released `ods` has: the Databricks plugin.
    pub fn builtin() -> Self {
        let mut plugins = Self::none();
        let databricks: Arc<dyn WarehousePlugin> = Arc::new(databricks::Databricks);
        plugins
            .warehouses
            .insert(databricks.warehouse().to_owned(), (databricks, true));
        plugins
    }

    /// Adds a health check.
    ///
    /// # Errors
    /// Its id isn't well formed, or another plugin check has it. A clash with a
    /// built-in or configured check is found when the checks are set up (exit 4).
    pub fn add_health_check(
        &mut self,
        origin: Origin,
        check: Arc<dyn HealthCheck>,
    ) -> Result<(), PluginError> {
        let id = check.describe().id;
        if !CheckInfo::valid_id(&id) {
            return Err(PluginError::InvalidId(id));
        }
        if self.checks.iter().any(|(c, _, _)| c.describe().id == id) {
            return Err(PluginError::DuplicateCheck(id));
        }
        self.checks.push((check, origin, false));
        Ok(())
    }

    /// Adds a warehouse plugin.
    ///
    /// # Errors
    /// Another plugin serves its warehouse: which one runs must never depend on the
    /// order they were added in. [`replace_warehouse`](Self::replace_warehouse) says so
    /// explicitly.
    /// Its dialect isn't one the SQL parser knows.
    pub fn add_warehouse(&mut self, plugin: Arc<dyn WarehousePlugin>) -> Result<(), PluginError> {
        known_dialect(plugin.as_ref())?;
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
    ///
    /// # Errors
    /// Its dialect isn't one the SQL parser knows.
    pub fn replace_warehouse(
        &mut self,
        plugin: Arc<dyn WarehousePlugin>,
    ) -> Result<(), PluginError> {
        known_dialect(plugin.as_ref())?;
        self.warehouses
            .insert(plugin.warehouse().to_owned(), (plugin, false));
        Ok(())
    }

    /// The registered health checks, in the order they were added.
    pub fn health_checks(&self) -> impl Iterator<Item = &Arc<dyn HealthCheck>> {
        self.checks.iter().map(|(c, _, _)| c)
    }

    /// The plugin serving `warehouse`, if any.
    pub fn warehouse(&self, warehouse: Option<&str>) -> Option<&dyn WarehousePlugin> {
        self.warehouses.get(warehouse?).map(|(p, _)| p.as_ref())
    }

    /// What runs read as `warehouse`'s source versions, when its plugin gives them: as
    /// the plugin describes it, or else which plugin reads them.
    pub fn versions_read(&self, warehouse: Option<&str>) -> Option<String> {
        let plugin = self.warehouse(warehouse)?;
        detect::changes(plugin).then(|| {
            plugin.versions_read().unwrap_or_else(|| {
                let o = plugin.origin();
                format!(
                    "data version read by the `{}` plugin ({} {})",
                    plugin.warehouse(),
                    o.name,
                    o.version
                )
            })
        })
    }

    /// Whether `warehouse` has a source-version provider.
    pub fn has_changes(&self, warehouse: Option<&str>) -> bool {
        self.warehouse(warehouse).is_some_and(detect::changes)
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

    /// `warehouse`'s links, built from `settings`, or why there are none.
    ///
    /// # Errors
    /// Why there are no links: no plugin for the warehouse, or the plugin's reason.
    pub fn links(
        &self,
        warehouse: Option<&str>,
        settings: &WarehouseSettings,
    ) -> Result<Arc<dyn RelationLinker>, NoRelationLink> {
        match self.warehouse(warehouse) {
            Some(plugin) => plugin.links(settings),
            None => Err(NoRelationLink::Unsupported {
                warehouse: warehouse.map(str::to_owned),
            }),
        }
    }

    /// A reader for the observed lineage exported to `export`, with the warehouse whose
    /// plugin reads it: `warehouse`'s plugin if it reads exports, else the only plugin
    /// that does. `None` when no plugin reads exports, or several do and the project's
    /// warehouse isn't one of them.
    pub fn observed_lineage(
        &self,
        warehouse: Option<&str>,
        export: &Path,
    ) -> Option<(String, ObservedReader)> {
        let read = |plugin: &dyn WarehousePlugin| {
            plugin
                .observed_lineage(export)
                .map(|found| (plugin.warehouse().to_owned(), found))
        };
        if let Some(found) = self.warehouse(warehouse).and_then(read) {
            return Some(found);
        }
        let mut readers = self
            .warehouses
            .values()
            .filter(|(p, _)| detect::observed_lineage(p.as_ref()));
        match (readers.next(), readers.next()) {
            (Some((only, _)), None) => read(only.as_ref()),
            _ => None,
        }
    }

    /// The warehouses whose plugins read observed lineage exports.
    pub fn observed_lineage_readers(&self) -> Vec<&str> {
        self.warehouses
            .iter()
            .filter(|(_, (p, _))| detect::observed_lineage(p.as_ref()))
            .map(|(w, _)| w.as_str())
            .collect()
    }

    /// The SQL dialect for `warehouse`'s SQL: its plugin's, else the warehouse kind
    /// itself, for the parser to map (`None` when there is neither).
    pub fn dialect(&self, warehouse: Option<&str>) -> Option<String> {
        self.warehouse(warehouse)
            .and_then(|p| p.dialect().map(str::to_owned))
            .or_else(|| warehouse.map(str::to_owned))
    }

    /// What every plugin offers, as detected (§3c): health checks by id, then each
    /// warehouse with its features.
    pub fn detected(&self) -> Vec<Detected> {
        let mut checks: Vec<Detected> = self
            .checks
            .iter()
            .map(|(check, origin, builtin)| detect::check(check.as_ref(), *origin, *builtin))
            .collect();
        checks.sort_by(|a, b| a.name.cmp(&b.name));
        checks.extend(
            self.warehouses
                .values()
                .map(|(plugin, builtin)| detect::warehouse(plugin.as_ref(), *builtin)),
        );
        checks
    }

    /// Every plugin, for `ods version` and `ods doctor`: health checks by id, then each
    /// warehouse's contracts, as detected.
    pub fn listing(&self) -> Vec<Listed> {
        self.detected()
            .into_iter()
            .flat_map(|d| {
                // A feature that implements no contract (a dialect) isn't a provider.
                d.features
                    .into_iter()
                    .filter_map(|f| f.contract)
                    .map(move |contract| Listed {
                        contract: contract.name,
                        contract_version: crate::version::dotted(contract.version),
                        name: d.name.clone(),
                        from: d.from.clone(),
                        builtin: d.builtin,
                    })
            })
            .collect()
    }
}

/// Refuses a plugin whose dialect the SQL parser doesn't know.
fn known_dialect(plugin: &dyn WarehousePlugin) -> Result<(), PluginError> {
    match plugin.dialect() {
        Some(d) if ods_provider_sqlparser::SqlDialect::from_name(d).is_none() => {
            Err(PluginError::UnknownDialect {
                warehouse: plugin.warehouse().to_owned(),
                dialect: d.to_owned(),
                known: ods_provider_sqlparser::SqlDialect::ALL
                    .iter()
                    .map(|d| d.name())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
        }
        _ => Ok(()),
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
    }

    fn probe() -> Arc<dyn RelationProbe> {
        Arc::new(FakeRelationProbe::new())
    }

    #[test]
    fn a_plugin_that_doesnt_say_what_it_reads_is_named_instead() {
        struct Versions;
        impl WarehousePlugin for Versions {
            fn origin(&self) -> Origin {
                Origin {
                    name: "acme-ods",
                    version: "1.2.3",
                }
            }
            fn warehouse(&self) -> &'static str {
                "acme"
            }
            fn changes(&self, probe: Arc<dyn RelationProbe>) -> Option<Arc<dyn ChangeProvider>> {
                let _ = probe;
                Some(Arc::new(ods_provider_fake::FakeChangeProvider::new()))
            }
        }
        let mut plugins = Plugins::none();
        plugins.add_warehouse(Arc::new(Versions)).unwrap();
        // Never another warehouse's wording (e.g. Delta's history).
        assert_eq!(
            plugins.versions_read(Some("acme")).as_deref(),
            Some("data version read by the `acme` plugin (acme-ods 1.2.3)")
        );
    }

    #[test]
    fn a_check_is_listed_with_the_crate_that_registered_it() {
        let mut plugins = Plugins::none();
        let origin = Origin {
            name: "acme-checks",
            version: "0.4.0",
        };
        plugins
            .add_health_check(
                origin,
                Arc::new(FakeHealthCheck::new("owner", Severity::Warn)),
            )
            .unwrap();
        let listed = plugins.listing();
        assert_eq!(listed[0].name, "owner");
        assert_eq!(listed[0].from, "acme-checks 0.4.0");
    }

    #[test]
    fn the_builtins_serve_databricks_and_nothing_else() {
        let plugins = Plugins::builtin();
        assert!(plugins.has_changes(Some("databricks")));
        assert!(plugins.changes(Some("databricks"), probe()).is_some());
        assert!(plugins.privileges(Some("databricks"), probe()).is_some());
        assert_eq!(
            plugins.versions_read(Some("databricks")).as_deref(),
            Some("table version from the Delta history")
        );
        for other in [None, Some("duckdb"), Some("Databricks")] {
            assert!(plugins.versions_read(other).is_none());
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
                ("relation_linker", "databricks", true),
                ("observed_lineage_source", "databricks", true),
            ]
        );
        assert_eq!(
            plugins.dialect(Some("databricks")).as_deref(),
            Some("databricks")
        );
        // Without a plugin, the kind is the dialect's name, for the parser to map.
        assert_eq!(plugins.dialect(Some("duckdb")).as_deref(), Some("duckdb"));
        assert_eq!(plugins.dialect(None), None);
    }

    /// A plugin that offers links (not configured here), a dialect, and maybe a reader
    /// of observed lineage.
    struct Acme {
        dialect: &'static str,
        reads_exports: bool,
    }

    impl WarehousePlugin for Acme {
        fn origin(&self) -> Origin {
            crate::origin!()
        }
        fn warehouse(&self) -> &'static str {
            "acme"
        }
        fn links(
            &self,
            _settings: &WarehouseSettings,
        ) -> Result<Arc<dyn RelationLinker>, NoRelationLink> {
            Err(NoRelationLink::NotConfigured {
                setting: "providers.acme.settings.host".to_owned(),
            })
        }
        fn observed_lineage(
            &self,
            _export: &Path,
        ) -> Option<Result<Arc<dyn ObservedLineageSource>, ProviderError>> {
            self.reads_exports
                .then(|| Err(ProviderError::Other("acme".to_owned())))
        }
        fn dialect(&self) -> Option<&str> {
            Some(self.dialect)
        }
    }

    #[test]
    fn features_are_detected_not_declared() {
        let mut plugins = Plugins::none();
        plugins
            .add_warehouse(Arc::new(Acme {
                dialect: "snowflake",
                reads_exports: false,
            }))
            .unwrap();
        let detected = plugins.detected();
        let acme = &detected[0];
        let names: Vec<&str> = acme.features.iter().map(|f| f.name).collect();
        assert_eq!(names, ["links", "dialect"]);
        // Offered, but not usable as configured: listed, with the reason.
        assert!(
            acme.features[0]
                .unavailable
                .as_deref()
                .unwrap()
                .contains("providers.acme.settings.host")
        );
        assert_eq!(acme.features[1].detail.as_deref(), Some("snowflake"));
        // A dialect isn't a provider: only links are listed as a contract.
        assert_eq!(
            plugins
                .listing()
                .iter()
                .map(|l| l.contract)
                .collect::<Vec<_>>(),
            ["relation_linker"]
        );
        assert_eq!(plugins.dialect(Some("acme")).as_deref(), Some("snowflake"));
    }

    #[test]
    fn a_dialect_the_parser_doesnt_know_refuses_the_plugin() {
        let mut plugins = Plugins::none();
        let err = plugins
            .add_warehouse(Arc::new(Acme {
                dialect: "acmesql",
                reads_exports: false,
            }))
            .unwrap_err();
        assert!(matches!(err, PluginError::UnknownDialect { .. }), "{err}");
        assert!(err.to_string().contains("databricks"), "{err}");
        let replaced = plugins.replace_warehouse(Arc::new(Acme {
            dialect: "acmesql",
            reads_exports: false,
        }));
        assert!(replaced.is_err());
    }

    #[test]
    fn observed_lineage_is_read_by_the_projects_plugin_or_the_only_reader() {
        let export = Path::new("missing.csv");
        let mut plugins = Plugins::builtin();
        // Databricks reads exports, for its projects and, as the only reader, others.
        for warehouse in [Some("databricks"), Some("duckdb"), None] {
            assert!(plugins.observed_lineage(warehouse, export).is_some());
        }
        assert_eq!(plugins.observed_lineage_readers(), ["databricks"]);
        // A second reader: each reads its own projects', and no one guesses for others.
        plugins
            .add_warehouse(Arc::new(Acme {
                dialect: "snowflake",
                reads_exports: true,
            }))
            .unwrap();
        let (reader, acme) = plugins.observed_lineage(Some("acme"), export).unwrap();
        assert_eq!(reader, "acme");
        assert!(acme.err().unwrap().to_string().contains("acme"));
        assert_eq!(
            plugins
                .observed_lineage(Some("databricks"), export)
                .unwrap()
                .0,
            "databricks"
        );
        assert!(plugins.observed_lineage(Some("duckdb"), export).is_none());
        assert_eq!(plugins.observed_lineage_readers(), ["acme", "databricks"]);
    }

    #[test]
    fn a_warehouse_is_served_once_unless_replaced_explicitly() {
        let mut plugins = Plugins::builtin();
        let err = plugins
            .add_warehouse(Arc::new(Other("databricks")))
            .unwrap_err();
        assert!(err.to_string().contains("replacing_warehouse"), "{err}");
        plugins.add_warehouse(Arc::new(Other("duckdb"))).unwrap();

        plugins
            .replace_warehouse(Arc::new(Other("databricks")))
            .unwrap();
        // The replacement offers nothing, so Databricks now has no table versions.
        assert!(!plugins.has_changes(Some("databricks")));
        assert!(plugins.listing().iter().all(|l| !l.builtin));
    }

    #[test]
    fn health_checks_need_a_valid_unique_id() {
        let mut plugins = Plugins::none();
        let bad = plugins.add_health_check(
            crate::origin!(),
            Arc::new(FakeHealthCheck::new("Owner!", Severity::Warn)),
        );
        assert_eq!(bad, Err(PluginError::InvalidId("Owner!".to_owned())));
        plugins
            .add_health_check(
                crate::origin!(),
                Arc::new(FakeHealthCheck::new("owner", Severity::Warn)),
            )
            .unwrap();
        let twice = plugins.add_health_check(
            crate::origin!(),
            Arc::new(FakeHealthCheck::new("owner", Severity::Warn)),
        );
        assert_eq!(twice, Err(PluginError::DuplicateCheck("owner".to_owned())));
        let listed = &plugins.listing()[0];
        assert_eq!(
            (listed.contract, listed.name.as_str(), listed.builtin),
            ("health_check", "owner", false)
        );
    }
}
