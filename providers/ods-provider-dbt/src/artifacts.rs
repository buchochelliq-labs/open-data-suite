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
}

#[derive(Debug, Deserialize)]
struct RawColumn {
    name: String,
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
    /// Columns declared in YAML (may be incomplete).
    pub declared_columns: Vec<String>,
}

/// The parts of `manifest.json` ODS uses.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Manifest {
    /// Schema version, e.g. 12.
    pub schema_version: u32,
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
}

#[derive(Debug, Deserialize)]
struct RawCatalogColumn {
    name: String,
    #[serde(default)]
    index: i64,
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
    /// Reads `manifest.json` and, if present, `catalog.json` from a dbt target directory.
    ///
    /// # Errors
    /// Returns [`DbtError`] if the manifest is missing or invalid, or the catalog is
    /// present but invalid.
    pub fn load(target_dir: &Path) -> Result<Self, DbtError> {
        let manifest = Manifest::read(&target_dir.join("manifest.json"))?;
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
                let mut declared_columns: Vec<String> =
                    n.columns.into_values().map(|c| c.name).collect();
                declared_columns.sort();
                let node = ManifestNode {
                    unique_id: n.unique_id,
                    resource_type: n.resource_type,
                    relation_name: n.relation_name,
                    compiled_code: n.compiled_code,
                    language: n.language,
                    materialized: n.config.materialized,
                    depends_on: n.depends_on.nodes,
                    declared_columns,
                };
                (node.unique_id.clone(), node)
            })
            .collect::<BTreeMap<_, _>>();
        Ok(Self {
            schema_version,
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
        let columns = raw
            .nodes
            .into_iter()
            .chain(raw.sources)
            .map(|(id, node)| {
                let mut columns: Vec<RawCatalogColumn> = node.columns.into_values().collect();
                columns.sort_by_key(|c| c.index);
                (id, columns.into_iter().map(|c| c.name).collect())
            })
            .collect();
        Ok(Self { columns })
    }
}
