//! What `ods serve`'s Catalog shows (#313): the project's nodes from the dbt artifacts
//! (and `catalog.json`, if generated), and each node's last successful build from the
//! state store, handed to `ods-web` as neutral facts (ADR-0001, ADR-0009).
//!
//! Only what the public artifact schemas record is used: a node's layer is its first
//! folder under the model paths (from dbt's `fqn`), tags come from its resolved
//! `config.tags`, and a column's type is shown only when `catalog.json` or a YAML
//! `data_type` records it (AGENTS rules 3 and 8).

use std::collections::BTreeMap;
use std::path::Path;

use ods_provider_dbt::{Catalog, Manifest, ManifestNode, ResourceType};
use ods_sdk::contracts::state_store::{StateScope, StateStore};
use ods_store_sqlite::SqliteStateStore;
use ods_web::catalog::{
    CatalogColumn, CatalogInput, CatalogNode, CatalogTest, LastBuild, TestKind, TypeSource,
};

use super::state_plan::{block_on, display_name, node_name};

/// How layers are worked out, for people.
const LAYER_SOURCE: &str =
    "From each model's first folder under the model paths (its dbt fqn). Seeds have none.";

/// How many history lines are read to find which snapshot recorded each build.
const HISTORY_READ: usize = 10_000;

/// The Catalog's facts. Never fails: what can't be read is left out, and said so in
/// the server log.
pub(super) fn catalog(
    manifest: &Manifest,
    target_dir: &Path,
    state_db: &Path,
    scope: &StateScope,
) -> CatalogInput {
    // `catalog.json` is optional; without it, only declared types are known.
    let path = target_dir.join("catalog.json");
    let warehouse = if path.is_file() {
        Catalog::read(&path)
            .map_err(|e| tracing::warn!(error = %e, "dashboard: catalog.json can't be read"))
            .ok()
    } else {
        None
    };
    let nodes: Vec<CatalogNode> = manifest
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.resource_type,
                ResourceType::Model | ResourceType::Seed | ResourceType::Snapshot
            )
        })
        .map(|n| node(manifest, n, warehouse.as_ref()))
        .collect();
    let names = manifest
        .nodes
        .iter()
        .filter(|n| n.resource_type == ResourceType::Source)
        .map(|n| (n.unique_id.clone(), display_name(&n.unique_id)))
        .collect();
    let input = CatalogInput::new(nodes)
        .with_names(names)
        .with_last_builds(last_builds(state_db, scope));
    if input.nodes.iter().any(|n| n.layer.is_some()) {
        input.with_layer_source(LAYER_SOURCE)
    } else {
        input
    }
}

fn kind(t: ResourceType) -> &'static str {
    match t {
        ResourceType::Model => "model",
        ResourceType::Seed => "seed",
        ResourceType::Snapshot => "snapshot",
        _ => "other",
    }
}

/// A model's first folder under the model paths: dbt's `fqn` is the package, the
/// folders, the name and, for a versioned model, `v<version>`. `None` for a model at
/// the top of its model path, and for anything else.
fn layer(n: &ManifestNode) -> Option<String> {
    if n.resource_type != ResourceType::Model {
        return None;
    }
    let mut parts: &[String] = n.fqn.get(1..)?;
    if let (Some(version), Some((last, rest))) = (&n.version, parts.split_last())
        && *last == format!("v{version}")
    {
        parts = rest;
    }
    let (_, folders) = parts.split_last()?;
    folders.first().cloned()
}

/// `config.tags`: a string or a list, as the manifest schema allows.
fn tags(n: &ManifestNode) -> Vec<String> {
    let mut tags: Vec<String> = match n.config.raw.get("tags") {
        Some(serde_json::Value::String(tag)) => vec![tag.clone()],
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    };
    tags.sort();
    tags.dedup();
    tags
}

fn node(manifest: &Manifest, n: &ManifestNode, warehouse: Option<&Catalog>) -> CatalogNode {
    let mut node = CatalogNode::new(n.unique_id.clone(), node_name(n), kind(n.resource_type));
    node.language.clone_from(&n.language);
    node.layer = layer(n);
    node.materialization.clone_from(&n.materialized);
    node.tags = tags(n);
    node.description = n.description.clone().filter(|d| !d.trim().is_empty());
    node.relation.clone_from(&n.relation_name);
    node.file.clone_from(&n.original_file_path);
    node.depends_on.clone_from(&n.depends_on);
    node.code.clone_from(&n.raw_code);
    node.compiled_code.clone_from(&n.compiled_code);
    node.columns = columns(n, warehouse);
    node.tests = manifest
        .nodes
        .iter()
        .filter_map(|t| {
            let test = t.test.as_ref()?;
            (test.attached_node.as_deref() == Some(n.unique_id.as_str())).then(|| {
                let name = match &test.namespace {
                    Some(namespace) => format!("{namespace}.{}", test.name),
                    None => test.name.clone(),
                };
                CatalogTest::new(
                    t.unique_id.clone(),
                    name,
                    test.column_name.clone(),
                    TestKind::Data,
                )
            })
        })
        .chain(
            manifest
                .unit_tests
                .iter()
                .filter(|u| u.depends_on.first() == Some(&n.unique_id))
                .map(|u| {
                    let name = u.unique_id.rsplit('.').next().unwrap_or(&u.unique_id);
                    CatalogTest::new(u.unique_id.clone(), name, None, TestKind::Unit)
                }),
        )
        .collect();
    node
}

/// In warehouse order when `catalog.json` has the node, else a seed's file order, else
/// the declared columns; then any declared column the others miss.
fn columns(n: &ManifestNode, warehouse: Option<&Catalog>) -> Vec<CatalogColumn> {
    let from_warehouse = warehouse.and_then(|c| c.columns.get(&n.unique_id));
    let types = warehouse.and_then(|c| c.types.get(&n.unique_id));
    let mut names: Vec<String> = from_warehouse
        .cloned()
        .or_else(|| n.file_columns.clone())
        .unwrap_or_default();
    for declared in &n.declared_columns {
        if !names.iter().any(|c| c.eq_ignore_ascii_case(declared)) {
            names.push(declared.clone());
        }
    }
    // Warehouses may fold case; YAML keeps what was written.
    let lookup = |map: &BTreeMap<String, String>, name: &str| {
        map.get(name).cloned().or_else(|| {
            map.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        })
    };
    names
        .into_iter()
        .map(|name| {
            let mut column = CatalogColumn::new(name.clone());
            column.data_type = types
                .and_then(|t| lookup(t, &name))
                .map(|t| (t, TypeSource::Warehouse))
                .or_else(|| lookup(&n.declared_types, &name).map(|t| (t, TypeSource::Declared)));
            column.description = lookup(&n.column_descriptions, &name);
            column.constraints = n
                .constraints
                .iter()
                .filter(|c| c.columns.iter().any(|col| col.eq_ignore_ascii_case(&name)))
                .map(|c| c.kind.clone())
                .collect();
            column
        })
        .collect()
}

/// Each node's last successful build in the latest snapshot, with the snapshot that
/// first recorded its run. Empty without a store: the store is never created here.
fn last_builds(state_db: &Path, scope: &StateScope) -> BTreeMap<String, LastBuild> {
    if !state_db.is_file() {
        return BTreeMap::new();
    }
    let read = block_on(async {
        let db = SqliteStateStore::open_existing(state_db).await?;
        let latest = db.latest(scope).await?;
        let history = db.history(scope, HISTORY_READ).await?;
        db.close().await;
        Ok::<_, ods_sdk::ProviderError>((latest, history))
    });
    let (latest, history) = match read {
        Ok(Ok(read)) => read,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "dashboard: the state store can't be read");
            return BTreeMap::new();
        }
        Err(e) => {
            tracing::warn!(error = %e.message, "dashboard: the state store can't be read");
            return BTreeMap::new();
        }
    };
    let Some(latest) = latest else {
        return BTreeMap::new();
    };
    // Run → the snapshot that committed it.
    let snapshots: BTreeMap<&str, u64> = history
        .iter()
        .map(|s| (s.run_id.as_str(), s.id.0))
        .collect();
    latest
        .snapshot
        .nodes
        .iter()
        .map(|(id, state)| {
            let mut build = LastBuild::new(
                snapshots.get(state.run_id.as_str()).copied(),
                state.run_id.clone(),
                state.built_at,
            );
            if let Some(tested) = &state.tested {
                build = build.with_tested(tested.run_id.clone(), tested.at);
            }
            (id.clone(), build)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_come_from_the_fqn_folders_only() {
        let manifest = Manifest::read(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10/manifest.json"),
        )
        .unwrap();
        let by_id = manifest.nodes_by_id();
        let layer_of = |id: &str| layer(by_id[id]);
        assert_eq!(
            layer_of("model.jaffle_ods.customers").as_deref(),
            Some("marts")
        );
        assert_eq!(
            layer_of("model.jaffle_ods.stg_orders").as_deref(),
            Some("staging")
        );
        assert_eq!(
            layer_of("seed.jaffle_ods.raw_orders"),
            None,
            "seeds have none"
        );
    }
}
