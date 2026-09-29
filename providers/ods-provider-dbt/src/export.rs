//! A dbt state directory for deferral (#296, ADR-0020 §3): the upstream `manifest.json`
//! with chosen nodes pointing at this target's relations.
//!
//! The upstream document is read as generic JSON, with its key order kept, and only
//! the `database`, `schema`, `alias` and `relation_name` of the chosen nodes are
//! replaced. Nothing is added or removed: dbt's schema is dbt's, and fields ODS doesn't
//! model survive untouched. Which nodes to point where is decided elsewhere; this
//! module only reads and writes dbt's format.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::artifacts::{check_version, read};
use crate::{DbtError, ManifestNode};

/// Manifest schema versions the export writes. The export keeps the upstream's
/// version, so only versions a test has shown dbt to read back are allowed.
pub const EXPORT_VERSIONS: [u32; 1] = [12];

/// The fields dbt builds a deferred relation from (ADR-0020, "How dbt resolves refs").
pub const RELATION_FIELDS: [&str; 4] = ["database", "schema", "alias", "relation_name"];

/// Why an upstream state can't be exported from.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExportError {
    /// The upstream manifest can't be read, isn't one, or has a version the export
    /// doesn't write.
    #[error(transparent)]
    Artifact(#[from] DbtError),
    /// The upstream manifest is another project's.
    #[error("the upstream manifest is for {upstream}, not {current}")]
    OtherProject {
        /// The upstream project, as its manifest names it.
        upstream: String,
        /// The current project.
        current: String,
    },
    /// A node to point at this target lacks a relation field in the upstream manifest,
    /// so it can't be rewritten consistently.
    #[error("`{node}` in the upstream manifest has no `{field}`")]
    MissingField {
        /// The node.
        node: String,
        /// The field.
        field: &'static str,
    },
    /// A node to point at this target isn't in the upstream manifest.
    #[error("`{0}` isn't in the upstream manifest")]
    UnknownNode(String),
    /// The rewritten manifest couldn't be serialized.
    #[error("couldn't write the manifest: {0}")]
    Serialize(String),
}

/// A node of the upstream manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UpstreamRef {
    /// Its `unique_id`.
    pub id: String,
    /// Whether dbt defers references to it: a model, seed or snapshot that isn't
    /// ephemeral.
    pub deferrable: bool,
}

/// An upstream state's `manifest.json`, as a document to rewrite.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Upstream {
    /// The file it was read from.
    pub path: PathBuf,
    /// The whole document, in its key order.
    pub document: Value,
    /// Its manifest schema version.
    pub schema_version: u32,
    /// `metadata.project_name`.
    pub project_name: Option<String>,
    /// `metadata.project_id`.
    pub project_id: Option<String>,
    /// `metadata.invocation_id`.
    pub invocation_id: Option<String>,
    /// Its `nodes`, sorted by id.
    pub nodes: Vec<UpstreamRef>,
}

fn text(value: &Value, pointer: &str) -> Option<String> {
    value.pointer(pointer)?.as_str().map(str::to_owned)
}

/// Reads `<dir>/manifest.json` as a document to export from.
///
/// # Errors
/// [`ExportError::Artifact`] when the file can't be read, isn't a manifest, or has a
/// version other than [`EXPORT_VERSIONS`].
pub fn read_upstream(dir: &Path) -> Result<Upstream, ExportError> {
    let path = dir.join("manifest.json");
    let invalid = |message: String| DbtError::Invalid {
        path: path.clone(),
        artifact: "manifest",
        message,
    };
    let document: Value =
        serde_json::from_str(&read(&path)?).map_err(|e| invalid(e.to_string()))?;
    let url = text(&document, "/metadata/dbt_schema_version")
        .ok_or_else(|| invalid("it has no `metadata.dbt_schema_version`".to_owned()))?;
    let schema_version = check_version(&path, "manifest", &url, &EXPORT_VERSIONS)?;
    let nodes = document
        .get("nodes")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("it has no `nodes`".to_owned()))?;
    let mut refs: Vec<UpstreamRef> = nodes
        .iter()
        .map(|(id, node)| {
            let refable = matches!(
                node.get("resource_type").and_then(Value::as_str),
                Some("model" | "seed" | "snapshot")
            );
            let ephemeral =
                node.pointer("/config/materialized").and_then(Value::as_str) == Some("ephemeral");
            UpstreamRef {
                id: id.clone(),
                deferrable: refable && !ephemeral,
            }
        })
        .collect();
    refs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Upstream {
        project_name: text(&document, "/metadata/project_name"),
        project_id: text(&document, "/metadata/project_id"),
        invocation_id: text(&document, "/metadata/invocation_id"),
        path,
        schema_version,
        nodes: refs,
        document,
    })
}

/// Whether `upstream` is the same project as the current manifest's. The names must
/// match; so must the ids when both manifests have one. Returns a warning when either
/// has no id, so only the names could be compared.
///
/// # Errors
/// [`ExportError::OtherProject`] when they differ, or either has no name.
pub fn same_project(
    upstream: &Upstream,
    current_name: Option<&str>,
    current_id: Option<&str>,
) -> Result<Option<String>, ExportError> {
    let named = |name: Option<&str>, id: Option<&str>| match (name, id) {
        (Some(name), Some(id)) => format!("project `{name}` (id {id})"),
        (Some(name), None) => format!("project `{name}`"),
        (None, _) => "a manifest that names no project".to_owned(),
    };
    let other = || ExportError::OtherProject {
        upstream: named(
            upstream.project_name.as_deref(),
            upstream.project_id.as_deref(),
        ),
        current: named(current_name, current_id),
    };
    match (upstream.project_name.as_deref(), current_name) {
        (Some(a), Some(b)) if a == b => {}
        _ => return Err(other()),
    }
    match (upstream.project_id.as_deref(), current_id) {
        (Some(a), Some(b)) if a == b => Ok(None),
        (Some(_), Some(_)) => Err(other()),
        _ => Ok(Some(format!(
            "{} doesn't record a project id, so only the project names were compared",
            if upstream.project_id.is_none() {
                format!("the upstream manifest `{}`", upstream.path.display())
            } else {
                "the current manifest".to_owned()
            }
        ))),
    }
}

/// Where a node builds in this target, from the current manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RelationFields {
    /// `database`: `None` for adapters without databases, written as `null`.
    pub database: Option<String>,
    /// `schema`.
    pub schema: String,
    /// `alias`.
    pub alias: String,
    /// `relation_name`.
    pub relation_name: String,
}

impl RelationFields {
    /// A relation's fields.
    pub fn new(
        database: Option<String>,
        schema: impl Into<String>,
        alias: impl Into<String>,
        relation_name: impl Into<String>,
    ) -> Self {
        Self {
            database,
            schema: schema.into(),
            alias: alias.into(),
            relation_name: relation_name.into(),
        }
    }

    /// The relation `node` builds, or `None` when the manifest doesn't name it in
    /// full (e.g. an ephemeral model, or artifacts without these fields).
    pub fn of(node: &ManifestNode) -> Option<Self> {
        Some(Self {
            database: node.database.clone(),
            schema: node.schema.clone()?,
            alias: node.alias.clone()?,
            relation_name: node.relation_name.clone()?,
        })
    }
}

/// The upstream document with each node in `relations` pointing at its relation
/// there: only those nodes' four [relation fields](RELATION_FIELDS) change. The
/// output keeps the upstream's key order and its `dbt_schema_version`.
///
/// # Errors
/// When a node isn't in the upstream manifest, lacks one of the fields, or the output
/// can't be serialized.
pub fn rewrite_manifest(
    upstream: &Value,
    relations: &BTreeMap<String, RelationFields>,
) -> Result<Vec<u8>, ExportError> {
    let mut document = upstream.clone();
    for (id, fields) in relations {
        let node = document
            .get_mut("nodes")
            .and_then(|n| n.get_mut(id.as_str()))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| ExportError::UnknownNode(id.clone()))?;
        let values = [
            fields.database.clone().map_or(Value::Null, Value::String),
            Value::String(fields.schema.clone()),
            Value::String(fields.alias.clone()),
            Value::String(fields.relation_name.clone()),
        ];
        for (field, value) in RELATION_FIELDS.into_iter().zip(values) {
            // Replaced in place, so the key keeps its position.
            let slot = node
                .get_mut(field)
                .ok_or_else(|| ExportError::MissingField {
                    node: id.clone(),
                    field,
                })?;
            *slot = value;
        }
    }
    serde_json::to_vec(&document).map_err(|e| ExportError::Serialize(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(project: &str, version: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/dbt")
            .join(project)
            .join("artifacts")
            .join(version)
    }

    fn dir_with(document: &Value) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            serde_json::to_vec(document).unwrap(),
        )
        .unwrap();
        dir
    }

    /// Every path at which `a` and `b` differ, including keys only one has.
    fn diff(before: &Value, after: &Value, at: &str, out: &mut Vec<String>) {
        match (before, after) {
            (Value::Object(old), Value::Object(new)) => {
                for (key, value) in old {
                    match new.get(key) {
                        Some(changed) => diff(value, changed, &format!("{at}/{key}"), out),
                        None => out.push(format!("{at}/{key} removed")),
                    }
                }
                for key in new.keys().filter(|k| !old.contains_key(*k)) {
                    out.push(format!("{at}/{key} added"));
                }
            }
            (Value::Array(old), Value::Array(new)) if old.len() == new.len() => {
                for (i, (value, changed)) in old.iter().zip(new).enumerate() {
                    diff(value, changed, &format!("{at}/{i}"), out);
                }
            }
            _ if before != after => out.push(at.to_owned()),
            _ => {}
        }
    }

    fn keys(value: &Value) -> Vec<String> {
        value
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn only_the_four_relation_fields_of_chosen_nodes_change() {
        for (project, version) in [
            ("jaffle-ods", "dbt-1.10"),
            ("jaffle-ods", "dbt-2.0"),
            ("jaffle-ods-state", "dbt-1.10"),
            ("jaffle-ods-state", "dbt-2.0"),
        ] {
            let upstream = read_upstream(&fixture(project, version)).unwrap();
            let chosen: Vec<&UpstreamRef> = upstream
                .nodes
                .iter()
                .filter(|n| n.deferrable)
                .take(2)
                .collect();
            assert_eq!(chosen.len(), 2);
            let relations: BTreeMap<String, RelationFields> = chosen
                .iter()
                .map(|n| {
                    (
                        n.id.clone(),
                        RelationFields::new(
                            Some("dev_db".into()),
                            "dev",
                            format!("{}_dev", n.id),
                            format!("\"dev_db\".\"dev\".\"{}\"", n.id),
                        ),
                    )
                })
                .collect();
            let written = rewrite_manifest(&upstream.document, &relations).unwrap();
            let after: Value = serde_json::from_slice(&written).unwrap();
            let mut changed = Vec::new();
            diff(&upstream.document, &after, "", &mut changed);
            let mut expected: Vec<String> = chosen
                .iter()
                .flat_map(|n| RELATION_FIELDS.map(|f| format!("/nodes/{}/{f}", n.id)))
                .collect();
            expected.sort();
            changed.sort();
            assert_eq!(changed, expected, "{project} {version}");
            // Nothing is reordered either, and the version is the upstream's.
            assert_eq!(keys(&after), keys(&upstream.document));
            for n in &chosen {
                assert_eq!(
                    keys(&after["nodes"][&n.id]),
                    keys(&upstream.document["nodes"][&n.id])
                );
            }
            assert_eq!(
                after["metadata"]["dbt_schema_version"],
                upstream.document["metadata"]["dbt_schema_version"]
            );
        }
    }

    #[test]
    fn nothing_chosen_writes_the_same_document() {
        let upstream = read_upstream(&fixture("jaffle-ods", "dbt-1.10")).unwrap();
        let written = rewrite_manifest(&upstream.document, &BTreeMap::new()).unwrap();
        let after: Value = serde_json::from_slice(&written).unwrap();
        assert_eq!(after, upstream.document);
    }

    #[test]
    fn a_database_less_relation_is_written_as_null() {
        let upstream = read_upstream(&fixture("jaffle-ods", "dbt-1.10")).unwrap();
        let id = upstream
            .nodes
            .iter()
            .find(|n| n.deferrable)
            .unwrap()
            .id
            .clone();
        let relations = BTreeMap::from([(
            id.clone(),
            RelationFields::new(None, "dev", "a", "`dev`.`a`"),
        )]);
        let after: Value =
            serde_json::from_slice(&rewrite_manifest(&upstream.document, &relations).unwrap())
                .unwrap();
        assert_eq!(after["nodes"][&id]["database"], Value::Null);
    }

    #[test]
    fn unknown_nodes_and_missing_fields_are_refused() {
        let upstream = read_upstream(&fixture("jaffle-ods", "dbt-1.10")).unwrap();
        let fields = RelationFields::new(None, "dev", "a", "a");
        let err = rewrite_manifest(
            &upstream.document,
            &BTreeMap::from([("model.x.nope".to_owned(), fields.clone())]),
        )
        .unwrap_err();
        assert!(matches!(err, ExportError::UnknownNode(_)), "{err}");
        let id = upstream
            .nodes
            .iter()
            .find(|n| n.deferrable)
            .unwrap()
            .id
            .clone();
        let mut document = upstream.document.clone();
        document["nodes"][&id]
            .as_object_mut()
            .unwrap()
            .remove("alias");
        let err = rewrite_manifest(&document, &BTreeMap::from([(id, fields)])).unwrap_err();
        assert!(
            matches!(err, ExportError::MissingField { field: "alias", .. }),
            "{err}"
        );
    }

    #[test]
    fn other_versions_are_refused_naming_the_version() {
        let mut document = read_upstream(&fixture("jaffle-ods", "dbt-1.10"))
            .unwrap()
            .document;
        document["metadata"]["dbt_schema_version"] =
            Value::from("https://schemas.getdbt.com/dbt/manifest/v11.json");
        let dir = dir_with(&document);
        let err = read_upstream(dir.path()).unwrap_err();
        let text = err.to_string();
        assert!(
            matches!(
                err,
                ExportError::Artifact(DbtError::UnsupportedVersion { found: 11, .. })
            ),
            "{text}"
        );
        assert!(text.contains("v11") && text.contains("v12"), "{text}");
    }

    #[test]
    fn a_missing_or_invalid_upstream_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            read_upstream(dir.path()),
            Err(ExportError::Artifact(DbtError::Io { .. }))
        ));
        std::fs::write(dir.path().join("manifest.json"), "{").unwrap();
        assert!(matches!(
            read_upstream(dir.path()),
            Err(ExportError::Artifact(DbtError::Invalid { .. }))
        ));
    }

    #[test]
    fn another_project_is_refused() {
        let upstream = read_upstream(&fixture("jaffle-ods", "dbt-1.10")).unwrap();
        let id = upstream.project_id.clone().unwrap();
        assert_eq!(
            same_project(&upstream, Some("jaffle_ods"), Some(&id)).unwrap(),
            None
        );
        assert!(matches!(
            same_project(&upstream, Some("shop"), Some(&id)),
            Err(ExportError::OtherProject { .. })
        ));
        assert!(matches!(
            same_project(&upstream, Some("jaffle_ods"), Some("0000")),
            Err(ExportError::OtherProject { .. })
        ));
        assert!(same_project(&upstream, None, None).is_err());
        // Without an id on one side, the names decide, with a warning.
        let warning = same_project(&upstream, Some("jaffle_ods"), None).unwrap();
        assert!(warning.is_some_and(|w| w.contains("project id")));
    }

    #[test]
    fn ephemeral_models_and_tests_are_not_deferrable() {
        let mut document = read_upstream(&fixture("jaffle-ods", "dbt-1.10"))
            .unwrap()
            .document;
        let model = document["nodes"]
            .as_object()
            .unwrap()
            .keys()
            .find(|k| k.starts_with("model."))
            .unwrap()
            .clone();
        document["nodes"][&model]["config"]["materialized"] = Value::from("ephemeral");
        let dir = dir_with(&document);
        let upstream = read_upstream(dir.path()).unwrap();
        let deferrable = |id: &str| {
            upstream
                .nodes
                .iter()
                .find(|n| n.id == id)
                .unwrap()
                .deferrable
        };
        assert!(!deferrable(&model));
        for n in &upstream.nodes {
            let kind = n.id.split('.').next().unwrap();
            if kind == "test" {
                assert!(!n.deferrable, "{}", n.id);
            }
            if matches!(kind, "seed" | "snapshot") {
                assert!(n.deferrable, "{}", n.id);
            }
        }
        assert!(upstream.nodes.iter().any(|n| n.id.starts_with("test.")));
    }

    #[test]
    fn relation_fields_come_from_the_current_manifest() {
        let manifest =
            crate::Manifest::read(&fixture("jaffle-ods", "dbt-1.10").join("manifest.json"))
                .unwrap();
        let orders = manifest
            .nodes
            .iter()
            .find(|n| n.unique_id == "model.jaffle_ods.orders")
            .unwrap();
        assert_eq!(
            RelationFields::of(orders),
            Some(RelationFields::new(
                Some("jaffle_ods".into()),
                "main",
                "orders",
                "\"jaffle_ods\".\"main\".\"orders\""
            ))
        );
        assert_eq!(
            manifest.project_id.as_deref(),
            Some("15a2ce11fa796d8c6724f7e1c35a38ad")
        );
    }
}
