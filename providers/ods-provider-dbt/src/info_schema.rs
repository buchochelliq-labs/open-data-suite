//! Reading dbt v2's Parquet "dbt Information Schema" (`target/info_schema/v1/`).
//!
//! dbt calls the Information Schema a "contracted interface": one Parquet file per table,
//! e.g. `dbt.models.parquet`. Table and column names follow the public reference
//! (`docs/reference/info-schema.md`) and the Apache-2.0 schema spec in `dbt-labs/dbt`
//! (`crates/dbt-index-core/src/info_schema/schema.rs`). Only the tables and columns ODS
//! needs are read; unknown columns are ignored, and missing optional ones read as null.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

use parquet::file::reader::SerializedFileReader;
use parquet::record::Field;

use crate::artifacts::{
    ArtifactSource, Catalog, DbtConfig, DbtConstraint, DbtError, DbtTest, Manifest, ManifestNode,
    RawConstraint, ResourceType,
};

/// Information Schema versions this reader understands.
const VERSIONS: [u32; 1] = [1];

/// Node tables, with the resource type of their rows.
const NODE_TABLES: [(&str, ResourceType); 4] = [
    ("dbt.models", ResourceType::Model),
    ("dbt.seeds", ResourceType::Seed),
    ("dbt.snapshots", ResourceType::Snapshot),
    ("dbt.sources", ResourceType::Source),
];

/// One row, by column name.
type Row = BTreeMap<String, Field>;

fn invalid(path: &Path, message: impl Into<String>) -> DbtError {
    DbtError::Invalid {
        path: path.to_owned(),
        artifact: "info schema",
        message: message.into(),
    }
}

/// Every row of `<dir>/<table>.parquet`, keeping only `columns`.
fn read_table(dir: &Path, table: &str, columns: &[&str]) -> Result<Vec<Row>, DbtError> {
    let path = dir.join(format!("{table}.parquet"));
    let file = File::open(&path).map_err(|source| DbtError::Io {
        path: path.clone(),
        source,
    })?;
    let reader = SerializedFileReader::new(file).map_err(|e| invalid(&path, e.to_string()))?;
    let mut rows = Vec::new();
    for row in reader {
        let row = row.map_err(|e| invalid(&path, e.to_string()))?;
        rows.push(
            row.get_column_iter()
                .filter(|(name, _)| columns.contains(&name.as_str()))
                .map(|(name, field)| (name.clone(), field.clone()))
                .collect(),
        );
    }
    Ok(rows)
}

fn text(row: &Row, column: &str) -> Option<String> {
    match row.get(column) {
        Some(Field::Str(value)) => Some(value.clone()),
        _ => None,
    }
}

/// The node's resolved `config` (a JSON string column), plus the sources' own
/// `loaded_at_*` columns.
fn config(row: &Row, path: &Path) -> Result<DbtConfig, DbtError> {
    let mut config = match text(row, "config").filter(|c| !c.is_empty()) {
        Some(json) => DbtConfig::from_json(
            &serde_json::from_str(&json)
                .map_err(|e| invalid(path, format!("a `config` isn't JSON: {e}")))?,
        ),
        None => DbtConfig::default(),
    };
    config.loaded_at_field = config
        .loaded_at_field
        .or_else(|| text(row, "loaded_at_field"));
    config.loaded_at_query = config
        .loaded_at_query
        .or_else(|| text(row, "loaded_at_query"));
    if config.unique_key.is_empty()
        && let Some(key) = text(row, "unique_key").filter(|k| !k.trim().is_empty())
    {
        // A JSON list (`["a","b"]`) or a plain name.
        let value = serde_json::from_str(&key).unwrap_or(serde_json::Value::String(key));
        config.unique_key = crate::artifacts::unique_key(&value);
    }
    Ok(config)
}

fn required(row: &Row, column: &str, path: &Path) -> Result<String, DbtError> {
    text(row, column).ok_or_else(|| invalid(path, format!("a row has no `{column}`")))
}

/// Finds the Information Schema directory for a dbt target directory: `dir` itself if it
/// holds the tables, else `dir/info_schema/v<N>`. Returns the directory and version.
pub(crate) fn locate(dir: &Path) -> Option<(PathBuf, u32)> {
    if dir.join("dbt.models.parquet").is_file() {
        let version = dir
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix('v'))
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        return Some((dir.to_owned(), version));
    }
    VERSIONS.iter().rev().find_map(|v| {
        let candidate = dir.join("info_schema").join(format!("v{v}"));
        candidate
            .join("dbt.models.parquet")
            .is_file()
            .then_some((candidate, *v))
    })
}

/// The dbt target directory an Information Schema directory belongs to
/// (`<target>/info_schema/v1` → `<target>`).
fn target_of(dir: &Path) -> Option<&Path> {
    let parent = dir.parent()?;
    (parent.file_name()? == "info_schema")
        .then(|| parent.parent())
        .flatten()
}

/// The compiled SQL of a node when the table doesn't carry it: dbt writes it to
/// `<target>/compiled/<package>/<original_file_path>` (or the row's `compiled_path`).
fn compiled_file(dir: &Path, row: &Row) -> Option<String> {
    let target = target_of(dir)?;
    let candidates = [
        text(row, "compiled_path").map(|p| {
            // `compiled_path` is relative to the project root, e.g. `target/compiled/…`.
            let p = PathBuf::from(p);
            match p.strip_prefix("target") {
                Ok(rest) => target.join(rest),
                Err(_) => target.join(p),
            }
        }),
        text(row, "package_name")
            .zip(text(row, "original_file_path"))
            .map(|(package, file)| target.join("compiled").join(package).join(file)),
    ];
    candidates
        .into_iter()
        .flatten()
        .find_map(|path| std::fs::read_to_string(path).ok())
}

/// Reads the manifest-equivalent and, if warehouse columns are present, the catalog.
pub(crate) fn read(dir: &Path, version: u32) -> Result<(Manifest, Option<Catalog>), DbtError> {
    if !VERSIONS.contains(&version) {
        return Err(DbtError::UnsupportedVersion {
            path: dir.to_owned(),
            artifact: "info schema",
            found: version,
            supported: VERSIONS.to_vec(),
        });
    }
    let project = read_table(dir, "dbt.project", &["dbt_version", "adapter_type"])?;
    let (dbt_version, adapter_type) = project
        .first()
        .map(|r| (text(r, "dbt_version"), text(r, "adapter_type")))
        .unwrap_or_default();

    let mut parents = read_parents(dir)?;
    let mut columns = read_columns(dir)?;

    let mut nodes = read_nodes(dir, &mut parents, &mut columns)?;
    nodes.extend(read_tests(dir, &mut parents)?);
    nodes.sort_by(|a, b| a.unique_id.cmp(&b.unique_id));

    let catalog = (!columns.actual.is_empty()).then(|| {
        let mut catalog = Catalog::default();
        for (node, mut ordered) in columns.actual {
            ordered.sort();
            catalog.types.insert(
                node.clone(),
                ordered
                    .iter()
                    .map(|(_, c, t)| (c.clone(), t.clone()))
                    .collect(),
            );
            catalog
                .columns
                .insert(node, ordered.into_iter().map(|(_, c, _)| c).collect());
        }
        catalog
    });
    let manifest = Manifest {
        schema_version: version,
        dbt_version,
        adapter_type,
        source: ArtifactSource::InfoSchema,
        nodes,
    };
    Ok((manifest, catalog))
}

/// Parents of every node, from `dbt.edges`. It also links macros and tests; those are
/// filtered out when the lineage project is built, like manifest `depends_on`.
fn read_parents(dir: &Path) -> Result<BTreeMap<String, Vec<String>>, DbtError> {
    let path = dir.join("dbt.edges.parquet");
    let mut parents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for edge in read_table(dir, "dbt.edges", &["parent_unique_id", "child_unique_id"])? {
        parents
            .entry(required(&edge, "child_unique_id", &path)?)
            .or_default()
            .push(required(&edge, "parent_unique_id", &path)?);
    }
    Ok(parents)
}

type Declared = BTreeMap<String, BTreeSet<String>>;
type Actual = BTreeMap<String, Vec<(i64, String, String)>>;

/// What `dbt.node_columns` says about each node's columns.
#[derive(Default)]
struct Columns {
    /// Every declared or discovered column.
    declared: Declared,
    /// The warehouse's own ordered list (the catalog), with types, where
    /// `data_type_actual` is known.
    actual: Actual,
    /// Types declared in YAML.
    declared_types: BTreeMap<String, BTreeMap<String, String>>,
    /// Column-level constraints.
    constraints: BTreeMap<String, Vec<DbtConstraint>>,
    /// Column descriptions.
    descriptions: BTreeMap<String, BTreeMap<String, String>>,
}

fn read_columns(dir: &Path) -> Result<Columns, DbtError> {
    let path = dir.join("dbt.node_columns.parquet");
    let mut columns = Columns::default();
    for row in read_table(
        dir,
        "dbt.node_columns",
        &[
            "node_unique_id",
            "column_name",
            "column_index",
            "data_type_actual",
            "data_type_declared",
            "constraints",
            "description",
        ],
    )? {
        let node = required(&row, "node_unique_id", &path)?;
        let column = required(&row, "column_name", &path)?;
        columns
            .declared
            .entry(node.clone())
            .or_default()
            .insert(column.clone());
        if let Some(data_type) = text(&row, "data_type_declared").filter(|t| !t.is_empty()) {
            columns
                .declared_types
                .entry(node.clone())
                .or_default()
                .insert(column.clone(), data_type);
        }
        if let Some(description) = text(&row, "description").filter(|d| !d.trim().is_empty()) {
            columns
                .descriptions
                .entry(node.clone())
                .or_default()
                .insert(column.clone(), description);
        }
        let column_constraints = constraints(&row, &path, Some(&column))?;
        if !column_constraints.is_empty() {
            columns
                .constraints
                .entry(node.clone())
                .or_default()
                .extend(column_constraints);
        }
        if let Some(data_type) = text(&row, "data_type_actual") {
            let index = match row.get("column_index") {
                Some(Field::Long(i)) => *i,
                Some(Field::Int(i)) => i64::from(*i),
                _ => i64::MAX,
            };
            columns
                .actual
                .entry(node)
                .or_default()
                .push((index, column, data_type));
        }
    }
    Ok(columns)
}

/// A `constraints` column: a JSON array of `{type, columns, to, to_columns, …}`.
fn constraints(
    row: &Row,
    path: &Path,
    column: Option<&str>,
) -> Result<Vec<DbtConstraint>, DbtError> {
    let Some(json) = text(row, "constraints").filter(|c| !c.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let raw: Vec<RawConstraint> = serde_json::from_str(&json)
        .map_err(|e| invalid(path, format!("a `constraints` isn't valid: {e}")))?;
    Ok(raw.into_iter().map(|c| c.into_constraint(column)).collect())
}

/// Models, seeds, snapshots and sources.
fn read_nodes(
    dir: &Path,
    parents: &mut BTreeMap<String, Vec<String>>,
    columns: &mut Columns,
) -> Result<Vec<ManifestNode>, DbtError> {
    let mut nodes = Vec::new();
    for (table, resource_type) in NODE_TABLES {
        let path = dir.join(format!("{table}.parquet"));
        if !path.is_file() {
            continue;
        }
        for row in read_table(
            dir,
            table,
            &[
                "unique_id",
                "relation_name",
                "compiled_code",
                "compiled_path",
                "package_name",
                "original_file_path",
                "raw_code",
                "node_language",
                "materialized",
                "enabled",
                "config",
                "loaded_at_field",
                "loaded_at_query",
                "constraints",
                "description",
                "unique_key",
            ],
        )? {
            if matches!(row.get("enabled"), Some(Field::Bool(false))) {
                continue;
            }
            let unique_id = required(&row, "unique_id", &path)?;
            // dbt-oss leaves `compiled_code` empty and writes the SQL to a file instead.
            let compiled_code = text(&row, "compiled_code")
                .filter(|c| !c.is_empty())
                .or_else(|| {
                    (resource_type != ResourceType::Source)
                        .then(|| compiled_file(dir, &row))
                        .flatten()
                });
            // The Information Schema has no file checksum: fingerprint the raw code, or
            // the compiled code, where there is one. Seeds have neither, so they never
            // look unchanged (conservative).
            let checksum = text(&row, "raw_code")
                .filter(|code| !code.is_empty())
                .map(|code| format!("raw_code:{}", fingerprint(&code)))
                .or_else(|| {
                    compiled_code
                        .as_deref()
                        .map(|code| format!("compiled_code:{}", fingerprint(code)))
                });
            nodes.push(ManifestNode {
                resource_type,
                relation_name: text(&row, "relation_name"),
                compiled_code,
                language: text(&row, "node_language"),
                materialized: text(&row, "materialized"),
                depends_on: parents.remove(&unique_id).unwrap_or_default(),
                declared_columns: columns
                    .declared
                    .get(&unique_id)
                    .map(|c| c.iter().cloned().collect())
                    .unwrap_or_default(),
                checksum,
                config: config(&row, &path)?,
                test: None,
                constraints: {
                    let mut all = constraints(&row, &path, None)?;
                    all.extend(columns.constraints.remove(&unique_id).unwrap_or_default());
                    all
                },
                declared_types: columns
                    .declared_types
                    .remove(&unique_id)
                    .unwrap_or_default(),
                description: text(&row, "description").filter(|d| !d.trim().is_empty()),
                column_descriptions: columns.descriptions.remove(&unique_id).unwrap_or_default(),
                unique_id,
            });
        }
    }
    Ok(nodes)
}

/// Data tests from `dbt.data_tests`, with what they depend on.
fn read_tests(
    dir: &Path,
    parents: &mut BTreeMap<String, Vec<String>>,
) -> Result<Vec<ManifestNode>, DbtError> {
    let path = dir.join("dbt.data_tests.parquet");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let mut tests = Vec::new();
    for row in read_table(
        dir,
        "dbt.data_tests",
        &[
            "unique_id",
            "test_name",
            "test_definition_package",
            "arguments",
            "column_name",
            "node_unique_id",
            "enabled",
            "where",
        ],
    )? {
        if matches!(row.get("enabled"), Some(Field::Bool(false))) {
            continue;
        }
        let unique_id = required(&row, "unique_id", &path)?;
        let Some(name) = text(&row, "test_name") else {
            // A singular (SQL) test: no generic test to interpret.
            continue;
        };
        let arguments = match text(&row, "arguments").filter(|a| !a.is_empty()) {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| invalid(&path, format!("test `arguments` aren't JSON: {e}")))?,
            None => serde_json::Value::Null,
        };
        tests.push(ManifestNode {
            resource_type: ResourceType::Test,
            relation_name: None,
            compiled_code: None,
            language: None,
            materialized: None,
            depends_on: parents.remove(&unique_id).unwrap_or_default(),
            declared_columns: Vec::new(),
            checksum: None,
            config: DbtConfig::default(),
            test: Some(DbtTest {
                name,
                namespace: text(&row, "test_definition_package").filter(|p| p != "dbt"),
                column_name: text(&row, "column_name"),
                attached_node: text(&row, "node_unique_id"),
                arguments,
                where_clause: text(&row, "where").filter(|w| !w.trim().is_empty()),
            }),
            constraints: Vec::new(),
            declared_types: BTreeMap::new(),
            description: None,
            column_descriptions: BTreeMap::new(),
            unique_id,
        });
    }
    Ok(tests)
}

/// A stable, dependency-free 64-bit FNV-1a fingerprint (only compared for equality).
fn fingerprint(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")
}
