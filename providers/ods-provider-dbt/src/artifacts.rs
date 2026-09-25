//! Lean, tolerant models of `manifest.json` and `catalog.json`.

use std::collections::BTreeMap;
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
    adapter_type: Option<String>,
    #[serde(default)]
    dbt_version: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawDependsOn {
    #[serde(default)]
    nodes: Vec<String>,
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
    #[serde(default)]
    checksum: Option<String>,
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
    resource_type: ResourceType,
    #[serde(default)]
    relation_name: Option<String>,
    #[serde(default)]
    compiled_code: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    depends_on: RawDependsOn,
    #[serde(default)]
    config: RawConfig,
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
}

/// A node from the manifest (model, seed, snapshot, source, test, …).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ManifestNode {
    /// dbt's `unique_id`, e.g. `model.jaffle.orders`.
    pub unique_id: String,
    /// What it is.
    pub resource_type: ResourceType,
    /// The fully qualified relation as dbt renders it, e.g. `"db"."main"."orders"`.
    pub relation_name: Option<String>,
    /// Rendered SQL, present after `dbt compile`/`run`/`build` for SQL models.
    pub compiled_code: Option<String>,
    /// `sql` or `python`.
    pub language: Option<String>,
    /// The configured materialization.
    pub materialized: Option<String>,
    /// Upstream node ids.
    pub depends_on: Vec<String>,
    /// Columns declared in YAML: often incomplete, and not in table order.
    pub declared_columns: Vec<String>,
    /// dbt's checksum of the node's source file (e.g. a model's SQL or a seed's CSV).
    pub checksum: Option<String>,
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
        }
    }
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
    /// Enabled nodes and sources, sorted by id.
    pub nodes: Vec<ManifestNode>,
}

/// Column lists from `catalog.json`, in warehouse order, by node id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Catalog {
    /// Columns by `unique_id`.
    pub columns: BTreeMap<String, Vec<String>>,
    /// Warehouse data types by `unique_id`, then column name.
    pub types: BTreeMap<String, BTreeMap<String, String>>,
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
        let manifest = Manifest::read(&json)?;
        let catalog_path = target_dir.join("catalog.json");
        let catalog = if catalog_path.is_file() {
            Some(Catalog::read(&catalog_path)?)
        } else {
            None
        };
        Ok(Self { manifest, catalog })
    }
}

fn read(path: &Path) -> Result<String, DbtError> {
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

fn check_version(
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
        let nodes = raw
            .nodes
            .into_values()
            .chain(raw.sources.into_values())
            .filter(|n| n.config.enabled != Some(false))
            .map(|n| {
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
                let where_clause = n
                    .config
                    .where_clause
                    .clone()
                    .filter(|w| !w.trim().is_empty());
                let test = n.test_metadata.map(|t| DbtTest {
                    name: t.name,
                    namespace: t.namespace,
                    column_name: n.column_name,
                    attached_node: n.attached_node,
                    arguments: t.kwargs,
                    where_clause,
                });
                let node = ManifestNode {
                    unique_id: n.unique_id,
                    resource_type: n.resource_type,
                    relation_name: n.relation_name,
                    compiled_code: n.compiled_code,
                    language: n.language,
                    materialized: n.config.materialized,
                    depends_on: n.depends_on.nodes,
                    declared_columns,
                    checksum: n.checksum.checksum,
                    config: DbtConfig {
                        state: n.config.state.filter(|v| !v.is_null()),
                        freshness: n.config.freshness.filter(|v| !v.is_null()),
                        loaded_at_field: n.config.loaded_at_field.or(n.loaded_at_field),
                        loaded_at_query: n.config.loaded_at_query.or(n.loaded_at_query),
                        unique_key: n
                            .config
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
                };
                (node.unique_id.clone(), node)
            })
            .collect::<BTreeMap<_, _>>();
        Ok(Self {
            schema_version,
            source: ArtifactSource::ManifestJson,
            dbt_version: raw.metadata.dbt_version,
            adapter_type: raw.metadata.adapter_type,
            nodes: nodes.into_values().collect(),
        })
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
        Ok(Self { columns, types })
    }
}
