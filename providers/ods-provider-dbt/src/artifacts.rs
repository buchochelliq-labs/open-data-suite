//! Lean, tolerant models of `manifest.json` and `catalog.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Manifest schema versions this reader understands.
const MANIFEST_VERSIONS: [u32; 2] = [11, 12];
/// Catalog schema versions this reader understands.
const CATALOG_VERSIONS: [u32; 1] = [1];

/// Why artifacts could not be read.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DbtError {
    /// The file could not be read.
    #[error("cannot read `{}`: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// The file is not a valid artifact.
    #[error("`{}` is not a valid dbt {artifact}: {message}", path.display())]
    Invalid {
        /// The file.
        path: PathBuf,
        /// Which artifact.
        artifact: &'static str,
        /// What is wrong.
        message: String,
    },
    /// The artifact's schema version is not supported.
    #[error(
        "`{}` is dbt {artifact} schema v{found}; supported versions: {}",
        path.display(),
        supported.iter().map(|v| format!("v{v}")).collect::<Vec<_>>().join(", ")
    )]
    UnsupportedVersion {
        /// The file.
        path: PathBuf,
        /// Which artifact.
        artifact: &'static str,
        /// The version found.
        found: u32,
        /// The versions supported.
        supported: Vec<u32>,
    },
}

/// What a manifest node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResourceType {
    /// A model.
    Model,
    /// A seed.
    Seed,
    /// A snapshot.
    Snapshot,
    /// A source.
    Source,
    /// A data test.
    Test,
    /// Anything else (analysis, operation, unit test, …).
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawMetadata {
    dbt_schema_version: String,
    #[serde(default)]
    invocation_id: Option<String>,
    #[serde(default)]
    project_name: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    adapter_type: Option<String>,
    #[serde(default)]
    dbt_version: Option<String>,
    #[serde(default)]
    generated_at: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawDependsOn {
    #[serde(default)]
    nodes: Vec<String>,
    #[serde(default)]
    macros: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawMacro {
    #[serde(default)]
    macro_sql: String,
    #[serde(default)]
    depends_on: RawDependsOn,
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    materialized: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    state: Option<serde_json::Value>,
    #[serde(default)]
    freshness: Option<serde_json::Value>,
    #[serde(default)]
    loaded_at_field: Option<String>,
    #[serde(default)]
    loaded_at_query: Option<String>,
    #[serde(default)]
    unique_key: Option<serde_json::Value>,
    /// Tests only: a filter limiting the rows they check.
    #[serde(default, rename = "where")]
    where_clause: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawChecksum {
    /// `sha256`, or `path` for seeds too large to hash (then `checksum` is the path).
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
}

impl RawChecksum {
    /// The checksum, if it is a hash of the content.
    fn of_content(self) -> Option<String> {
        match self.name.as_deref() {
            Some("path") => None,
            _ => self.checksum,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawColumn {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    data_type: Option<String>,
    #[serde(default)]
    constraints: Vec<RawConstraint>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawConstraint {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    columns: Option<Vec<String>>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    to_columns: Option<Vec<String>>,
    #[serde(default)]
    expression: Option<String>,
}

impl RawConstraint {
    /// `column` is set for a column-level constraint, which applies to that column.
    pub(crate) fn into_constraint(self, column: Option<&str>) -> DbtConstraint {
        DbtConstraint {
            kind: self.kind,
            columns: self
                .columns
                .filter(|c| !c.is_empty())
                .or_else(|| column.map(|c| vec![c.to_owned()]))
                .unwrap_or_default(),
            to: self.to,
            to_columns: self.to_columns.unwrap_or_default(),
            expression: self.expression,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawTestMetadata {
    name: String,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    kwargs: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct RawNode {
    unique_id: String,
    #[serde(default)]
    original_file_path: Option<String>,
    #[serde(default)]
    root_path: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// Model versions are numbers or strings.
    #[serde(default)]
    version: Option<serde_json::Value>,
    resource_type: ResourceType,
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    relation_name: Option<String>,
    #[serde(default)]
    compiled_code: Option<String>,
    #[serde(default)]
    raw_code: Option<String>,
    #[serde(default)]
    fqn: Vec<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    depends_on: RawDependsOn,
    /// Kept whole for fingerprints; the fields ODS reads are parsed into [`RawConfig`].
    #[serde(default)]
    config: serde_json::Value,
    #[serde(default)]
    columns: BTreeMap<String, RawColumn>,
    #[serde(default)]
    checksum: RawChecksum,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    test_metadata: Option<RawTestMetadata>,
    #[serde(default)]
    column_name: Option<String>,
    #[serde(default)]
    attached_node: Option<String>,
    #[serde(default)]
    constraints: Vec<RawConstraint>,
    // dbt 1.x sources also carry these at the top level.
    #[serde(default)]
    loaded_at_field: Option<String>,
    #[serde(default)]
    loaded_at_query: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    metadata: RawMetadata,
    #[serde(default)]
    nodes: BTreeMap<String, RawNode>,
    #[serde(default)]
    sources: BTreeMap<String, RawNode>,
    #[serde(default)]
    macros: BTreeMap<String, RawMacro>,
    /// dbt 1.8+. Kept whole: their fixtures are their definition.
    #[serde(default)]
    unit_tests: BTreeMap<String, serde_json::Value>,
    /// dbt 1.6+: the semantic layer's models and metrics.
    #[serde(default)]
    semantic_models: BTreeMap<String, RawSemanticModel>,
    #[serde(default)]
    metrics: BTreeMap<String, RawMetric>,
}

/// `config.enabled`, the only config the semantic layer's entries are read for.
#[derive(Debug, Default, Deserialize)]
struct RawEnabled {
    #[serde(default)]
    enabled: Option<bool>,
}

/// An entity, measure or dimension of a semantic model: its kind is the entity's or
/// dimension's `type`, or the measure's `agg`.
#[derive(Debug, Deserialize)]
struct RawSemanticField {
    name: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    agg: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSemanticModel {
    unique_id: String,
    name: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    depends_on: RawDependsOn,
    #[serde(default)]
    entities: Vec<RawSemanticField>,
    #[serde(default)]
    measures: Vec<RawSemanticField>,
    #[serde(default)]
    dimensions: Vec<RawSemanticField>,
    #[serde(default)]
    config: Option<RawEnabled>,
}

/// A measure or metric a metric reads, by name.
#[derive(Debug, Deserialize)]
struct RawInput {
    name: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawMetricParams {
    #[serde(default)]
    input_measures: Vec<RawInput>,
    #[serde(default)]
    numerator: Option<RawInput>,
    #[serde(default)]
    denominator: Option<RawInput>,
    #[serde(default)]
    expr: Option<String>,
    #[serde(default)]
    metrics: Vec<RawInput>,
}

#[derive(Debug, Deserialize)]
struct RawMetric {
    unique_id: String,
    name: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    type_params: RawMetricParams,
    #[serde(default)]
    depends_on: RawDependsOn,
    #[serde(default)]
    config: Option<RawEnabled>,
}

/// A node from the manifest (model, seed, snapshot, source, test, …).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ManifestNode {
    /// dbt's `unique_id`, e.g. `model.jaffle.orders`.
    pub unique_id: String,
    /// What it is.
    pub resource_type: ResourceType,
    /// The database (or catalog) of the relation it builds, as compiled for the target.
    pub database: Option<String>,
    /// The schema of the relation it builds, as compiled for the target.
    pub schema: Option<String>,
    /// The name of the relation it builds, as compiled for the target.
    pub alias: Option<String>,
    /// The fully qualified relation as dbt renders it, e.g. `"db"."main"."orders"`.
    pub relation_name: Option<String>,
    /// Rendered SQL, present after `dbt compile`/`run`/`build` for SQL models.
    pub compiled_code: Option<String>,
    /// The code as written, with its Jinja.
    pub raw_code: Option<String>,
    /// dbt's fully qualified name: package, folders and name (and version), e.g.
    /// `["jaffle", "marts", "orders"]`. Empty when the artifacts don't record it.
    pub fqn: Vec<String>,
    /// `sql` or `python`.
    pub language: Option<String>,
    /// The configured materialization.
    pub materialized: Option<String>,
    /// Upstream node ids.
    pub depends_on: Vec<String>,
    /// Macros it calls directly, by id (e.g. `macro.shop.cents_to_dollars`).
    pub depends_on_macros: Vec<String>,
    /// Columns declared in YAML: often incomplete, and not in table order.
    pub declared_columns: Vec<String>,
    /// dbt's checksum of the node's source file (e.g. a model's SQL or a seed's CSV);
    /// `None` when dbt didn't hash the content (seeds over 1 MiB are identified by path).
    pub checksum: Option<String>,
    /// Its name, e.g. `orders`.
    pub name: Option<String>,
    /// Its source file, relative to the project root, e.g. `seeds/raw_orders.csv`.
    pub original_file_path: Option<String>,
    /// The project root dbt recorded, if any (older manifests).
    pub root_path: Option<String>,
    /// A seed's columns, in order, from the header of its CSV file. Only set when the
    /// file is found and its SHA-256 matches dbt's checksum, i.e. it is the file dbt
    /// loaded.
    pub file_columns: Option<Vec<String>>,
    /// Its model version, e.g. `2`, for versioned models.
    pub version: Option<String>,
    /// Scheduling configuration, as resolved by dbt.
    pub config: DbtConfig,
    /// For data tests: which test, on what.
    pub test: Option<DbtTest>,
    /// Contract constraints, model- and column-level (column-level ones name their
    /// column in [`DbtConstraint::columns`]).
    pub constraints: Vec<DbtConstraint>,
    /// Column data types declared in YAML, by column name.
    pub declared_types: BTreeMap<String, String>,
    /// The node's documented description, if any.
    pub description: Option<String>,
    /// Documented column descriptions, by column name.
    pub column_descriptions: BTreeMap<String, String>,
}

/// `unique_key: id` or `unique_key: [a, b]`. Comma-separated strings (`"a, b"`) are an
/// older spelling of a list.
pub(crate) fn unique_key(value: &serde_json::Value) -> Vec<String> {
    let parts: Vec<String> = match value {
        serde_json::Value::String(text) => text.split(',').map(str::to_owned).collect(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    parts
        .into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect()
}

/// A data test, as declared: `unique`, `not_null`, `relationships`, a package test, …
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DbtTest {
    /// The generic test's name, e.g. `relationships`.
    pub name: String,
    /// The package defining it, e.g. `dbt_utils`; `None` for dbt's own tests.
    pub namespace: Option<String>,
    /// The column it tests, if it is a column test.
    pub column_name: Option<String>,
    /// The node it tests.
    pub attached_node: Option<String>,
    /// Its arguments, e.g. `{"to": "ref('customers')", "field": "id"}`.
    pub arguments: serde_json::Value,
    /// A `where` filter: the test only checks the rows it keeps.
    pub where_clause: Option<String>,
}

/// A model contract constraint (`primary_key`, `foreign_key`, `unique`, `not_null`,
/// `check`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DbtConstraint {
    /// The constraint type, as written.
    pub kind: String,
    /// The constrained columns.
    pub columns: Vec<String>,
    /// For foreign keys: the referenced relation, e.g. `ref('customers')`.
    pub to: Option<String>,
    /// For foreign keys: the referenced columns.
    pub to_columns: Vec<String>,
    /// Free-form expression (older foreign-key syntax, checks).
    pub expression: Option<String>,
}

/// A node's scheduling configuration as dbt resolved it: project, folder, YAML and SQL
/// `config()` already merged. dbt v2 merges the `state` block key by key; dbt 1.x lets
/// a more specific block replace a less specific one. Either way it is used as given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DbtConfig {
    /// The dbt State `state:` block, e.g. `{"lag_tolerance": "4h"}`.
    pub state: Option<serde_json::Value>,
    /// The `freshness:` block, e.g. `{"build_after": {"count": 1, "period": "day"}}`.
    pub freshness: Option<serde_json::Value>,
    /// Column or expression whose maximum says when a source last received data.
    pub loaded_at_field: Option<String>,
    /// Query returning when a source last received data.
    pub loaded_at_query: Option<String>,
    /// The `unique_key` of an incremental model or snapshot: the columns dbt merges on,
    /// one or several.
    pub unique_key: Vec<String>,
    /// The whole resolved config, as dbt wrote it (a JSON object; `null` if absent).
    pub raw: serde_json::Value,
}

impl DbtConfig {
    /// Builds it from a parsed config object, e.g. the Information Schema's `config`
    /// column. Null values count as unset.
    pub(crate) fn from_json(config: &serde_json::Value) -> Self {
        let value = |key: &str| config.get(key).filter(|v| !v.is_null()).cloned();
        let text = |key: &str| config.get(key).and_then(|v| v.as_str()).map(str::to_owned);
        Self {
            state: value("state"),
            freshness: value("freshness"),
            loaded_at_field: text("loaded_at_field"),
            loaded_at_query: text("loaded_at_query"),
            unique_key: config.get("unique_key").map(unique_key).unwrap_or_default(),
            raw: config.clone(),
        }
    }
}

/// A macro: its source and the macros it calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DbtMacro {
    /// The macro's Jinja source.
    pub sql: String,
    /// Macros it calls, by id.
    pub depends_on: Vec<String>,
}

/// Which dbt artifact format the project was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArtifactSource {
    /// `manifest.json` (dbt 1.7+ and v2's JSON output).
    ManifestJson,
    /// dbt v2's Parquet "dbt Information Schema".
    InfoSchema,
}

/// Which artifacts to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ArtifactPreference {
    /// `manifest.json` if present, else the Information Schema.
    #[default]
    Auto,
    /// Only `manifest.json` (+ `catalog.json`).
    Json,
    /// Only the Parquet Information Schema.
    InfoSchema,
}

/// The parts of the project manifest ODS uses, from either artifact format.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Manifest {
    /// Schema version of the source format: manifest v11/v12, or Information Schema v1.
    pub schema_version: u32,
    /// Which format it was read from.
    pub source: ArtifactSource,
    /// The dbt version that wrote it.
    pub dbt_version: Option<String>,
    /// The adapter, e.g. `databricks`; names the SQL dialect.
    pub adapter_type: Option<String>,
    /// The dbt project's name.
    pub project_name: Option<String>,
    /// dbt's id of the project (a hash of its name), when the artifacts record one.
    pub project_id: Option<String>,
    /// The dbt invocation that wrote it.
    pub invocation_id: Option<String>,
    /// Macros, by id: their source and the macros they call.
    pub macros: BTreeMap<String, DbtMacro>,
    /// Enabled nodes and sources, sorted by id.
    pub nodes: Vec<ManifestNode>,
    /// Enabled unit tests (dbt 1.8+), sorted by id.
    pub unit_tests: Vec<DbtUnitTest>,
    /// Enabled semantic models (dbt 1.6+), sorted by id.
    pub semantic_models: Vec<DbtSemanticModel>,
    /// Enabled metrics (dbt 1.6+), sorted by id.
    pub metrics: Vec<DbtMetric>,
}

/// A semantic model: the entities, measures and dimensions declared on a model.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DbtSemanticModel {
    /// dbt's `unique_id`, e.g. `semantic_model.jaffle.orders`.
    pub unique_id: String,
    /// Its name.
    pub name: String,
    /// Its label, if declared.
    pub label: Option<String>,
    /// Its description, if declared.
    pub description: Option<String>,
    /// The nodes it is defined on, from `depends_on.nodes`.
    pub depends_on: Vec<String>,
    /// Its entities; `kind` is the entity's type (`primary`, `foreign`, …).
    pub entities: Vec<SemanticField>,
    /// Its measures; `kind` is the aggregation (`sum`, `count`, …).
    pub measures: Vec<SemanticField>,
    /// Its dimensions; `kind` is the dimension's type (`time`, `categorical`).
    pub dimensions: Vec<SemanticField>,
}

/// An entity, measure or dimension of a semantic model.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SemanticField {
    /// Its name.
    pub name: String,
    /// What kind it is, as the manifest says; `None` when it doesn't.
    pub kind: Option<String>,
    /// Its description, if declared.
    pub description: Option<String>,
}

/// A metric, as declared: what it is computed from, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DbtMetric {
    /// dbt's `unique_id`, e.g. `metric.jaffle.revenue`.
    pub unique_id: String,
    /// Its name.
    pub name: String,
    /// Its label, if declared.
    pub label: Option<String>,
    /// Its description, if declared.
    pub description: Option<String>,
    /// Its type: `simple`, `ratio`, `derived`, `cumulative`, `conversion`.
    pub kind: Option<String>,
    /// The measures it reads, by name (`type_params.input_measures`).
    pub input_measures: Vec<String>,
    /// A ratio's numerator and denominator metrics, by name.
    pub ratio: Option<(String, String)>,
    /// A derived metric's expression.
    pub expr: Option<String>,
    /// The metrics a derived metric reads, by name.
    pub input_metrics: Vec<String>,
    /// The semantic models and metrics it reads, from `depends_on.nodes`.
    pub depends_on: Vec<String>,
}

/// A unit test: fixed inputs and the rows a model must produce from them.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DbtUnitTest {
    /// dbt's `unique_id`, e.g. `unit_test.jaffle.orders.test_totals`.
    pub unique_id: String,
    /// The nodes it reads, from `depends_on.nodes`: the model under test first.
    pub depends_on: Vec<String>,
    /// Its whole manifest entry, for fingerprints.
    pub definition: serde_json::Value,
}

/// Column lists from `catalog.json`, in warehouse order, by node id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Catalog {
    /// Columns by `unique_id`.
    pub columns: BTreeMap<String, Vec<String>>,
    /// Warehouse data types by `unique_id`, then column name.
    pub types: BTreeMap<String, BTreeMap<String, String>>,
    /// When dbt wrote it (`metadata.generated_at`), as written: the columns and types
    /// are the warehouse's as of then.
    pub generated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawCatalogColumn {
    name: String,
    #[serde(default)]
    index: i64,
    #[serde(default, rename = "type")]
    data_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawCatalogNode {
    #[serde(default)]
    columns: BTreeMap<String, RawCatalogColumn>,
}

#[derive(Debug, Deserialize)]
struct RawCatalog {
    metadata: RawMetadata,
    #[serde(default)]
    nodes: BTreeMap<String, RawCatalogNode>,
    #[serde(default)]
    sources: BTreeMap<String, RawCatalogNode>,
}

/// A manifest and, if present, a catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Artifacts {
    /// The manifest.
    pub manifest: Manifest,
    /// The catalog, if one was found.
    pub catalog: Option<Catalog>,
}

impl Artifacts {
    /// Reads a dbt target directory: `manifest.json` (plus `catalog.json` if present)
    /// or, for dbt v2, the Parquet Information Schema under `info_schema/v1/` (the
    /// directory may also be the `v1/` directory itself).
    ///
    /// # Errors
    /// Returns [`DbtError`] if no supported artifacts are found or they are invalid.
    pub fn load(target_dir: &Path) -> Result<Self, DbtError> {
        Self::load_with(target_dir, ArtifactPreference::Auto)
    }

    /// Like [`Artifacts::load`], choosing the format explicitly.
    ///
    /// # Errors
    /// Returns [`DbtError`] if the chosen artifacts are missing or invalid.
    pub fn load_with(target_dir: &Path, preference: ArtifactPreference) -> Result<Self, DbtError> {
        let json = target_dir.join("manifest.json");
        let use_json = match preference {
            ArtifactPreference::Json => true,
            ArtifactPreference::InfoSchema => false,
            ArtifactPreference::Auto => json.is_file(),
        };
        if !use_json {
            let Some((dir, version)) = crate::info_schema::locate(target_dir) else {
                return Err(DbtError::Io {
                    path: target_dir.join("info_schema/v1/dbt.models.parquet"),
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "no manifest.json and no dbt Information Schema",
                    ),
                });
            };
            let (manifest, catalog) = crate::info_schema::read(&dir, version)?;
            return Ok(Self { manifest, catalog });
        }
        let mut manifest = Manifest::read(&json)?;
        crate::seeds::attach_columns(&mut manifest, target_dir);
        let catalog_path = target_dir.join("catalog.json");
        let catalog = if catalog_path.is_file() {
            Some(Catalog::read(&catalog_path)?)
        } else {
            None
        };
        Ok(Self { manifest, catalog })
    }
}

pub(crate) fn read(path: &Path) -> Result<String, DbtError> {
    fs::read_to_string(path).map_err(|source| DbtError::Io {
        path: path.to_owned(),
        source,
    })
}

/// The number in `https://schemas.getdbt.com/dbt/manifest/v12.json`.
fn schema_version(url: &str) -> Option<u32> {
    url.rsplit('/')
        .next()?
        .strip_prefix('v')?
        .strip_suffix(".json")?
        .parse()
        .ok()
}

pub(crate) fn check_version(
    path: &Path,
    artifact: &'static str,
    url: &str,
    supported: &[u32],
) -> Result<u32, DbtError> {
    let found = schema_version(url).ok_or_else(|| DbtError::Invalid {
        path: path.to_owned(),
        artifact,
        message: format!("unrecognised dbt_schema_version `{url}`"),
    })?;
    if supported.contains(&found) {
        Ok(found)
    } else {
        Err(DbtError::UnsupportedVersion {
            path: path.to_owned(),
            artifact,
            found,
            supported: supported.to_vec(),
        })
    }
}

impl Manifest {
    /// The nodes by id, for lookups.
    pub fn nodes_by_id(&self) -> BTreeMap<&str, &ManifestNode> {
        self.nodes
            .iter()
            .map(|n| (n.unique_id.as_str(), n))
            .collect()
    }

    /// `depends_on`, with every node that `pass_through` accepts replaced by that node's
    /// own dependencies, transitively (e.g. ephemeral models, whose SQL their readers
    /// inline). Sorted. Ids that name no node in `by_id` are kept.
    pub fn dependencies_through<'a>(
        by_id: &BTreeMap<&'a str, &'a ManifestNode>,
        depends_on: &'a [String],
        pass_through: impl Fn(&ManifestNode) -> bool,
    ) -> BTreeSet<&'a str> {
        let mut out = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut pending: Vec<&str> = depends_on.iter().map(String::as_str).collect();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            match by_id.get(id) {
                Some(node) if pass_through(node) => {
                    pending.extend(node.depends_on.iter().map(String::as_str));
                }
                _ => {
                    out.insert(id);
                }
            }
        }
        out
    }

    /// The macros reachable from `roots` through the macros they call, sorted. Macros
    /// `skip` accepts are left out and not followed; ids of macros the manifest
    /// doesn't include are kept, so callers can report them.
    pub fn macros_reached<'a>(
        &'a self,
        roots: impl IntoIterator<Item = &'a str>,
        skip: impl Fn(&str) -> bool,
    ) -> BTreeSet<&'a str> {
        let mut seen = BTreeSet::new();
        let mut pending: Vec<&str> = roots.into_iter().collect();
        while let Some(id) = pending.pop() {
            if skip(id) || !seen.insert(id) {
                continue;
            }
            if let Some(m) = self.macros.get(id) {
                pending.extend(m.depends_on.iter().map(String::as_str));
            }
        }
        seen
    }

    /// Reads and validates a manifest.
    ///
    /// # Errors
    /// Returns [`DbtError`] if the file can't be read, isn't a manifest, or has an
    /// unsupported schema version.
    pub fn read(path: &Path) -> Result<Self, DbtError> {
        Self::parse(path, &read(path)?)
    }

    /// Parses manifest JSON; `path` is only used in errors.
    ///
    /// # Errors
    /// Returns [`DbtError`] if the text isn't a supported manifest.
    pub fn parse(path: &Path, json: &str) -> Result<Self, DbtError> {
        let raw: RawManifest = serde_json::from_str(json).map_err(|e| DbtError::Invalid {
            path: path.to_owned(),
            artifact: "manifest",
            message: e.to_string(),
        })?;
        let schema_version = check_version(
            path,
            "manifest",
            &raw.metadata.dbt_schema_version,
            &MANIFEST_VERSIONS,
        )?;
        let invalid = |message: String| DbtError::Invalid {
            path: path.to_owned(),
            artifact: "manifest",
            message,
        };
        let mut parsed = Vec::new();
        for n in raw.nodes.into_values().chain(raw.sources.into_values()) {
            let config: RawConfig = if n.config.is_null() {
                RawConfig::default()
            } else {
                serde_json::from_value(n.config.clone())
                    .map_err(|e| invalid(format!("`{}` has an invalid config: {e}", n.unique_id)))?
            };
            if config.enabled != Some(false) {
                parsed.push((n, config));
            }
        }
        let nodes = parsed
            .into_iter()
            .map(|(n, config)| {
                let node = manifest_node(n, config);
                (node.unique_id.clone(), node)
            })
            .collect::<BTreeMap<_, _>>();
        Ok(Self {
            schema_version,
            source: ArtifactSource::ManifestJson,
            dbt_version: raw.metadata.dbt_version,
            adapter_type: raw.metadata.adapter_type,
            project_name: raw.metadata.project_name,
            project_id: raw.metadata.project_id,
            invocation_id: raw.metadata.invocation_id,
            macros: raw
                .macros
                .into_iter()
                .map(|(id, m)| {
                    (
                        id,
                        DbtMacro {
                            sql: m.macro_sql,
                            depends_on: m.depends_on.macros,
                        },
                    )
                })
                .collect(),
            nodes: nodes.into_values().collect(),
            unit_tests: raw
                .unit_tests
                .into_iter()
                .filter(|(_, t)| {
                    t.pointer("/config/enabled") != Some(&serde_json::Value::Bool(false))
                })
                .map(|(id, definition)| DbtUnitTest {
                    depends_on: definition
                        .pointer("/depends_on/nodes")
                        .and_then(serde_json::Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|n| n.as_str().map(str::to_owned))
                                .collect()
                        })
                        .unwrap_or_default(),
                    unique_id: id,
                    definition,
                })
                .collect(),
            semantic_models: raw
                .semantic_models
                .into_values()
                .filter(|m| enabled(m.config.as_ref()))
                .map(semantic_model)
                .collect(),
            metrics: raw
                .metrics
                .into_values()
                .filter(|m| enabled(m.config.as_ref()))
                .map(metric)
                .collect(),
        })
    }
}

/// Whether a semantic-layer entry is enabled: unless its config says otherwise.
fn enabled(config: Option<&RawEnabled>) -> bool {
    config.and_then(|c| c.enabled) != Some(false)
}

/// Blank text is no text.
fn declared(text: Option<String>) -> Option<String> {
    text.filter(|t| !t.trim().is_empty())
}

fn semantic_fields(fields: Vec<RawSemanticField>) -> Vec<SemanticField> {
    fields
        .into_iter()
        .map(|f| SemanticField {
            name: f.name,
            kind: f.kind.or(f.agg),
            description: declared(f.description),
        })
        .collect()
}

fn semantic_model(m: RawSemanticModel) -> DbtSemanticModel {
    DbtSemanticModel {
        unique_id: m.unique_id,
        name: m.name,
        label: declared(m.label),
        description: declared(m.description),
        depends_on: m.depends_on.nodes,
        entities: semantic_fields(m.entities),
        measures: semantic_fields(m.measures),
        dimensions: semantic_fields(m.dimensions),
    }
}

fn metric(m: RawMetric) -> DbtMetric {
    let params = m.type_params;
    DbtMetric {
        unique_id: m.unique_id,
        name: m.name,
        label: declared(m.label),
        description: declared(m.description),
        kind: m.kind,
        input_measures: params.input_measures.into_iter().map(|i| i.name).collect(),
        ratio: params
            .numerator
            .zip(params.denominator)
            .map(|(n, d)| (n.name, d.name)),
        expr: declared(params.expr),
        input_metrics: params.metrics.into_iter().map(|i| i.name).collect(),
        depends_on: m.depends_on.nodes,
    }
}

/// A manifest node from its raw form and parsed config.
fn manifest_node(n: RawNode, config: RawConfig) -> ManifestNode {
    let mut constraints: Vec<DbtConstraint> = n
        .constraints
        .into_iter()
        .map(|c| c.into_constraint(None))
        .collect();
    let mut declared_types = BTreeMap::new();
    let mut column_descriptions = BTreeMap::new();
    let mut declared_columns = Vec::new();
    for column in n.columns.into_values() {
        if let Some(description) = column.description.filter(|d| !d.trim().is_empty()) {
            column_descriptions.insert(column.name.clone(), description);
        }
        constraints.extend(
            column
                .constraints
                .into_iter()
                .map(|c| c.into_constraint(Some(&column.name))),
        );
        if let Some(data_type) = column.data_type.filter(|t| !t.is_empty()) {
            declared_types.insert(column.name.clone(), data_type);
        }
        declared_columns.push(column.name);
    }
    declared_columns.sort();
    let where_clause = config.where_clause.clone().filter(|w| !w.trim().is_empty());
    let test = n.test_metadata.map(|t| DbtTest {
        name: t.name,
        namespace: t.namespace,
        column_name: n.column_name,
        attached_node: n.attached_node,
        arguments: t.kwargs,
        where_clause,
    });
    ManifestNode {
        unique_id: n.unique_id,
        resource_type: n.resource_type,
        database: n.database,
        schema: n.schema,
        alias: n.alias,
        relation_name: n.relation_name,
        compiled_code: n.compiled_code,
        raw_code: n.raw_code,
        fqn: n.fqn,
        language: n.language,
        materialized: config.materialized,
        depends_on: n.depends_on.nodes,
        depends_on_macros: n.depends_on.macros,
        declared_columns,
        checksum: n.checksum.of_content(),
        name: n.name,
        original_file_path: n.original_file_path,
        root_path: n.root_path,
        file_columns: None,
        version: n.version.and_then(|v| match v {
            serde_json::Value::String(s) => Some(s),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }),
        config: DbtConfig {
            state: config.state.filter(|v| !v.is_null()),
            freshness: config.freshness.filter(|v| !v.is_null()),
            loaded_at_field: config.loaded_at_field.or(n.loaded_at_field),
            loaded_at_query: config.loaded_at_query.or(n.loaded_at_query),
            raw: n.config,
            unique_key: config
                .unique_key
                .as_ref()
                .map(unique_key)
                .unwrap_or_default(),
        },
        test,
        constraints,
        declared_types,
        description: n.description.filter(|d| !d.trim().is_empty()),
        column_descriptions,
    }
}

impl Catalog {
    /// Reads and validates a catalog.
    ///
    /// # Errors
    /// Returns [`DbtError`] if the file can't be read or isn't a supported catalog.
    pub fn read(path: &Path) -> Result<Self, DbtError> {
        let raw: RawCatalog =
            serde_json::from_str(&read(path)?).map_err(|e| DbtError::Invalid {
                path: path.to_owned(),
                artifact: "catalog",
                message: e.to_string(),
            })?;
        check_version(
            path,
            "catalog",
            &raw.metadata.dbt_schema_version,
            &CATALOG_VERSIONS,
        )?;
        let mut columns = BTreeMap::new();
        let mut types = BTreeMap::new();
        for (id, node) in raw.nodes.into_iter().chain(raw.sources) {
            let mut ordered: Vec<RawCatalogColumn> = node.columns.into_values().collect();
            ordered.sort_by_key(|c| c.index);
            let node_types: BTreeMap<String, String> = ordered
                .iter()
                .filter_map(|c| Some((c.name.clone(), c.data_type.clone()?)))
                .collect();
            if !node_types.is_empty() {
                types.insert(id.clone(), node_types);
            }
            columns.insert(id, ordered.into_iter().map(|c| c.name).collect());
        }
        Ok(Self {
            columns,
            types,
            generated_at: raw.metadata.generated_at,
        })
    }
}
