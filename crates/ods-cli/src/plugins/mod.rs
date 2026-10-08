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
use ods_sdk::contracts::error_catalogue::ErrorCatalogue;
use ods_sdk::contracts::health_check::{CheckInfo, HealthCheck};
use ods_sdk::contracts::observed_lineage::ObservedLineageSource;
use ods_sdk::contracts::privileges::PrivilegedProbe;
use ods_sdk::contracts::probe::RelationProbe;
use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLinker};
use serde::Serialize;

mod databricks;
mod detect;
mod duckdb;
mod settings;

pub use detect::{Detected, Feature, PluginKind};
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

    /// The warehouses this one is built on, nearest first, as a dbt adapter names the
    /// adapters it depends on (Databricks on Spark, Redshift on Postgres). Error
    /// patterns and the dialect are inherited from them, nothing else (ADR-0031 §3b).
    /// `[warehouses.<kind>] extends` replaces it.
    fn parents(&self) -> Vec<String> {
        Vec::new()
    }

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

    /// Its engine's own error patterns (ADR-0025): consulted before dbt's, which adds
    /// its steps to what this recognises. Patterns several warehouses share stay in
    /// dbt's catalogue.
    fn errors(&self) -> Option<Arc<dyn ErrorCatalogue>> {
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

/// `[warehouses.<kind>]` as configured (ADR-0031 §3b).
pub type Warehouses = BTreeMap<String, ods_config::WarehouseConfig>;

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

    /// What the released `ods` has: the Databricks and `DuckDB` plugins.
    pub fn builtin() -> Self {
        let mut plugins = Self::none();
        let builtins: [Arc<dyn WarehousePlugin>; 2] =
            [Arc::new(databricks::Databricks), Arc::new(duckdb::Duckdb)];
        for plugin in builtins {
            plugins
                .warehouses
                .insert(plugin.warehouse().to_owned(), (plugin, true));
        }
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

    /// The warehouses `warehouse` is built on, nearest first: `[warehouses.<kind>]
    /// extends` when configured, else its plugin's [`parents`](WarehousePlugin::parents).
    pub fn parents_of(&self, warehouse: &str, configured: &Warehouses) -> Vec<String> {
        match configured.get(warehouse).and_then(|w| w.extends.clone()) {
            Some(extends) => extends,
            None => self
                .warehouse(Some(warehouse))
                .map(WarehousePlugin::parents)
                .unwrap_or_default(),
        }
    }

    /// `warehouse`, then the warehouses it is built on, depth first, each once: the
    /// order in which its error patterns and dialect are looked for, as dbt's adapter
    /// dispatch looks for a macro (ADR-0031 §3b).
    pub fn chain(&self, warehouse: &str, configured: &Warehouses) -> Vec<String> {
        let mut chain = Vec::new();
        let mut stack = vec![warehouse.to_owned()];
        while let Some(kind) = stack.pop() {
            if chain.contains(&kind) {
                continue;
            }
            let parents = self.parents_of(&kind, configured);
            chain.push(kind);
            stack.extend(parents.into_iter().rev());
        }
        chain
    }

    /// A cycle among the warehouses' parents, as `a → b → a`, if there is one: a
    /// configuration error.
    pub fn cycle(&self, configured: &Warehouses) -> Option<String> {
        let mut kinds: Vec<String> = configured.keys().cloned().collect();
        kinds.extend(self.warehouses.keys().cloned());
        kinds.sort();
        kinds.dedup();
        for start in &kinds {
            let mut path = vec![start.clone()];
            if self.cycle_from(&mut path, configured) {
                return Some(path.join(" → "));
            }
        }
        None
    }

    /// Whether a path of parents from the end of `path` comes back to a kind on it,
    /// leaving that path in `path`.
    fn cycle_from(&self, path: &mut Vec<String>, configured: &Warehouses) -> bool {
        let last = path.last().cloned().unwrap_or_default();
        for parent in self.parents_of(&last, configured) {
            let seen = path.contains(&parent);
            path.push(parent);
            if seen || self.cycle_from(path, configured) {
                return true;
            }
            path.pop();
        }
        false
    }

    /// The error catalogues for a project on `warehouse`: its plugin's, then each
    /// parent's that has one, nearest first.
    pub fn errors(
        &self,
        warehouse: Option<&str>,
        configured: &Warehouses,
    ) -> Vec<Arc<dyn ErrorCatalogue>> {
        warehouse.map_or_else(Vec::new, |w| {
            self.chain(w, configured)
                .iter()
                .filter_map(|kind| self.warehouse(Some(kind))?.errors())
                .collect()
        })
    }

    /// The SQL dialect for `warehouse`'s SQL: the first in its chain that a plugin
    /// names, else the first kind in it the parser knows, else the warehouse kind
    /// itself, for the parser to refuse (`None` without a warehouse).
    pub fn dialect(&self, warehouse: Option<&str>, configured: &Warehouses) -> Option<String> {
        let warehouse = warehouse?;
        let chain = self.chain(warehouse, configured);
        chain
            .iter()
            .find_map(|kind| self.warehouse(Some(kind))?.dialect().map(str::to_owned))
            .or_else(|| {
                chain
                    .iter()
                    .find(|kind| ods_provider_sqlparser::SqlDialect::from_name(kind).is_some())
                    .cloned()
            })
            .or_else(|| Some(warehouse.to_owned()))
    }

    /// Every other warehouse plugin's error catalogue, by warehouse: those not on
    /// `warehouse`'s chain, asked after dbt's, for a run on another warehouse, or
    /// before dbt names the project's.
    pub fn other_errors(
        &self,
        warehouse: Option<&str>,
        configured: &Warehouses,
    ) -> Vec<Arc<dyn ErrorCatalogue>> {
        let chain = warehouse.map_or_else(Vec::new, |w| self.chain(w, configured));
        self.warehouses
            .iter()
            .filter(|(w, _)| !chain.contains(w))
            .filter_map(|(_, (p, _))| p.errors())
            .collect()
    }

    /// The error catalogue for a project on `warehouse` (ADR-0031 §3a, §3b): its chain's
    /// catalogues, nearest first, then dbt's, then every other plugin's.
    pub fn project_catalogue(
        &self,
        warehouse: Option<&str>,
        configured: &Warehouses,
    ) -> ods_provider_dbt::error_catalogue::ProjectCatalogue {
        ods_provider_dbt::error_catalogue::ProjectCatalogue::new(
            self.errors(warehouse, configured),
            self.other_errors(warehouse, configured),
        )
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

/// Refuses configuration whose `[warehouses.<kind>] extends` make a cycle, with the
/// installed plugins' own parents (ADR-0031 §3b): a configuration error (exit 4).
///
/// # Errors
/// The cycle, at the first `extends` on it.
pub(crate) fn validate_warehouses(
    config: &ods_config::Loaded,
) -> Result<(), ods_config::ConfigError> {
    let warehouses = &config.config.warehouses;
    let Some(cycle) = installed().cycle(warehouses) else {
        return Ok(());
    };
    let (key, origin) = cycle
        .split(" → ")
        .find_map(|kind| {
            let key = ["warehouses", kind, "extends"].map(str::to_owned).to_vec();
            config
                .effective(&key)
                .map(|s| (format!("warehouses.{kind}.extends"), s.source.clone()))
        })
        .unwrap_or_else(|| ("warehouses".to_owned(), ods_config::Source::Default));
    Err(ods_config::ConfigError::Schema {
        key,
        origin: Box::new(origin),
        message: format!("the warehouses it extends make a cycle: {cycle}"),
    })
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
    fn the_builtins_serve_databricks_and_duckdb_and_nothing_else() {
        let plugins = Plugins::builtin();
        assert!(plugins.has_changes(Some("databricks")));
        assert!(plugins.changes(Some("databricks"), probe()).is_some());
        assert!(plugins.privileges(Some("databricks"), probe()).is_some());
        assert_eq!(
            plugins.versions_read(Some("databricks")).as_deref(),
            Some("table version from the Delta history")
        );
        // `DuckDB` has no table version or login check; a kind without a plugin, none.
        for other in [None, Some("duckdb"), Some("snowflake"), Some("Databricks")] {
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
                ("error_catalogue", "databricks", true),
                ("error_catalogue", "duckdb", true),
            ]
        );
        assert_eq!(
            plugins
                .dialect(Some("databricks"), &Warehouses::new())
                .as_deref(),
            Some("databricks")
        );
        assert_eq!(
            plugins.errors(Some("databricks"), &Warehouses::new())[0]
                .catalogue()
                .name,
            "databricks"
        );
        let catalogues = |w: Option<&str>, configured: &Warehouses| {
            plugins
                .errors(w, configured)
                .iter()
                .map(|c| c.catalogue().name)
                .collect::<Vec<_>>()
        };
        let others = |w: Option<&str>, configured: &Warehouses| {
            plugins
                .other_errors(w, configured)
                .iter()
                .map(|c| c.catalogue().name)
                .collect::<Vec<_>>()
        };
        let none = Warehouses::new();
        assert_eq!(catalogues(Some("duckdb"), &none), ["duckdb"]);
        assert_eq!(catalogues(Some("snowflake"), &none), Vec::<String>::new());
        // Another warehouse's project, or one not yet named, still has the others' after
        // dbt's.
        assert_eq!(others(Some("duckdb"), &none), ["databricks"]);
        assert_eq!(others(Some("databricks"), &none), ["duckdb"]);
        assert_eq!(others(None, &none), ["databricks", "duckdb"]);
        // A warehouse built on Databricks has its patterns on its chain, not after dbt.
        let on_databricks = extends(&[("acmebricks", &["databricks"])]);
        assert_eq!(
            catalogues(Some("acmebricks"), &on_databricks),
            ["databricks"]
        );
        assert_eq!(others(Some("acmebricks"), &on_databricks), ["duckdb"]);
        assert_eq!(
            plugins.dialect(Some("duckdb"), &none).as_deref(),
            Some("duckdb")
        );
        // Without a plugin, the kind is the dialect's name, for the parser to map.
        assert_eq!(
            plugins.dialect(Some("postgres"), &none).as_deref(),
            Some("postgres")
        );
        assert_eq!(plugins.dialect(None, &Warehouses::new()), None);
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
        assert_eq!(
            plugins.dialect(Some("acme"), &Warehouses::new()).as_deref(),
            Some("snowflake")
        );
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

    fn extends(pairs: &[(&str, &[&str])]) -> Warehouses {
        pairs
            .iter()
            .map(|(kind, parents)| {
                let mut w = ods_config::WarehouseConfig::default();
                w.extends = Some(parents.iter().map(|p| (*p).to_owned()).collect());
                ((*kind).to_owned(), w)
            })
            .collect()
    }

    /// A plugin for `spark`, with error patterns and Spark's dialect.
    struct Spark;

    impl WarehousePlugin for Spark {
        fn origin(&self) -> Origin {
            crate::origin!()
        }
        fn warehouse(&self) -> &'static str {
            "spark"
        }
        fn errors(&self) -> Option<Arc<dyn ErrorCatalogue>> {
            Some(Arc::new(ods_provider_fake::FakeErrorCatalogue::new()))
        }
        fn dialect(&self) -> Option<&str> {
            Some("spark")
        }
    }

    #[test]
    fn a_warehouse_inherits_errors_and_dialect_from_what_it_is_built_on() {
        let mut plugins = Plugins::builtin();
        let none = Warehouses::new();
        assert_eq!(plugins.chain("databricks", &none), ["databricks", "spark"]);
        // Without a Spark plugin, Databricks has only its own catalogue.
        assert_eq!(plugins.errors(Some("databricks"), &none).len(), 1);
        plugins.add_warehouse(Arc::new(Spark)).unwrap();
        let catalogues: Vec<String> = plugins
            .errors(Some("databricks"), &none)
            .iter()
            .map(|c| c.catalogue().name)
            .collect();
        assert_eq!(catalogues.len(), 2);
        assert_eq!(catalogues[0], "databricks", "nearest first: {catalogues:?}");
        // Its own dialect wins over its parent's.
        assert_eq!(
            plugins.dialect(Some("databricks"), &none).as_deref(),
            Some("databricks")
        );
        // A warehouse with no plugin, configured as built on Spark, takes Spark's.
        let configured = extends(&[("acmespark", &["spark"])]);
        assert_eq!(
            plugins.chain("acmespark", &configured),
            ["acmespark", "spark"]
        );
        assert_eq!(plugins.errors(Some("acmespark"), &configured).len(), 1);
        assert_eq!(
            plugins.dialect(Some("acmespark"), &configured).as_deref(),
            Some("spark")
        );
        // With no plugin anywhere, the first kind the parser knows.
        let on_postgres = extends(&[("materialize", &["postgres"])]);
        assert_eq!(
            plugins
                .dialect(Some("materialize"), &on_postgres)
                .as_deref(),
            Some("postgres")
        );
        // A kind the parser doesn't know and that extends nothing stays as named.
        assert_eq!(
            plugins.dialect(Some("acmedb"), &none).as_deref(),
            Some("acmedb")
        );
        // Configuration replaces a plugin's own parents.
        let alone = extends(&[("databricks", &[])]);
        assert_eq!(plugins.chain("databricks", &alone), ["databricks"]);
        assert_eq!(plugins.errors(Some("databricks"), &alone).len(), 1);
    }

    #[test]
    fn only_errors_and_the_dialect_are_inherited() {
        // Source versions, the login check, links and observed lineage come from the
        // project's own warehouse's plugin: a child on Databricks gets none of them.
        let plugins = Plugins::builtin();
        assert!(!plugins.has_changes(Some("acmebricks")));
        assert!(plugins.privileges(Some("acmebricks"), probe()).is_none());
        assert!(matches!(
            plugins.links(Some("acmebricks"), &WarehouseSettings::empty()),
            Err(NoRelationLink::Unsupported { .. })
        ));
        let configured = extends(&[("acmebricks", &["databricks"])]);
        assert_eq!(plugins.errors(Some("acmebricks"), &configured).len(), 1);
    }

    #[test]
    fn a_cycle_of_parents_is_found() {
        let plugins = Plugins::builtin();
        assert_eq!(plugins.cycle(&Warehouses::new()), None);
        let cycle = plugins
            .cycle(&extends(&[("spark", &["databricks"])]))
            .unwrap();
        assert!(cycle.contains("databricks → spark → databricks"), "{cycle}");
        assert!(plugins.cycle(&extends(&[("a", &["a"])])).is_some());
        // A chain still ends when configuration makes one anyway.
        assert_eq!(
            plugins.chain("spark", &extends(&[("spark", &["databricks"])])),
            ["spark", "databricks"]
        );
    }

    #[test]
    fn a_warehouse_is_served_once_unless_replaced_explicitly() {
        let mut plugins = Plugins::builtin();
        let err = plugins
            .add_warehouse(Arc::new(Other("databricks")))
            .unwrap_err();
        assert!(err.to_string().contains("replacing_warehouse"), "{err}");
        plugins.add_warehouse(Arc::new(Other("snowflake"))).unwrap();

        plugins
            .replace_warehouse(Arc::new(Other("databricks")))
            .unwrap();
        // The replacement offers nothing, so Databricks now has no table versions.
        assert!(!plugins.has_changes(Some("databricks")));
        assert!(
            plugins
                .listing()
                .iter()
                .filter(|l| l.name == "databricks")
                .all(|l| !l.builtin)
        );
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
