//! What `ods serve`'s Catalog shows (#313): the project's nodes from the dbt artifacts
//! (and `catalog.json`, if generated), and each node's last successful build from the
//! state store, handed to `ods-web` as neutral facts (ADR-0001, ADR-0009).
//!
//! Only what the public artifact schemas record is used: a node's layer is inferred
//! from its first folder under the model paths (dbt's `fqn`), tags come from its
//! resolved `config.tags`, and a column's type is shown only when `catalog.json` or a
//! YAML `data_type` records it (AGENTS rules 3 and 8).

use std::collections::BTreeMap;
use std::path::Path;

use ods_provider_dbt::fingerprint::checks_digest;
use ods_provider_dbt::{Catalog, Manifest, ManifestNode, ResourceType};
use ods_sdk::contracts::state_store::{SnapshotSummary, StoredSnapshot};
use ods_web::catalog::{
    CatalogColumn, CatalogInput, CatalogNode, CatalogTest, ColumnSource, LastBuild, TestKind,
    TypeSource,
};
use ods_web::freshness::{FreshnessInput, SourceInput};

use super::relation_links::Links;
use super::state_plan::{Workspace, display_name, node_name};

/// The Freshness evidence screen's sources (#350): each one's data version as the
/// planner is given it, so the screen shows what `ods state explain` does, and how the
/// project says its new data is measured (`loaded_at_field` or `loaded_at_query`), and
/// whether runs read a table version the screen can't.
pub(super) fn freshness(ws: &Workspace) -> FreshnessInput {
    // Runs read table versions from the warehouse's history first (ADR-0022), which
    // the dashboard, offline, can't: the screen says so rather than show nothing.
    // As the warehouse's plugin describes what it reads (ADR-0031 §3).
    let table_versions =
        crate::plugins::installed().versions_read(ws.manifest.adapter_type.as_deref());
    let declared: BTreeMap<&str, &ManifestNode> = ws
        .manifest
        .nodes
        .iter()
        .filter(|n| n.resource_type == ResourceType::Source)
        .map(|n| (n.unique_id.as_str(), n))
        .collect();
    let sources = ws
        .project
        .sources
        .iter()
        .map(|source| {
            let node = declared.get(source.id.as_str());
            let measured_with = node.and_then(|n| {
                n.config
                    .loaded_at_field
                    .as_ref()
                    .map(|field| format!("max({field})"))
                    .or_else(|| {
                        n.config
                            .loaded_at_query
                            .as_ref()
                            .map(|_| "loaded_at_query".to_owned())
                    })
            });
            SourceInput::new(&source.id, &source.name)
                .with_relation(node.and_then(|n| n.relation_name.clone()))
                .measured_with(measured_with)
                .read_by_runs(table_versions.clone())
                .with_version(
                    source.version.clone(),
                    source.observed_at,
                    source.version_evidence.clone(),
                )
        })
        .collect();
    FreshnessInput::new(sources).measured(
        ws.sources_taken_at,
        ws.sources_file
            .as_ref()
            .and_then(|f| f.file_name())
            .map(|f| f.to_string_lossy().into_owned()),
    )
}

/// How layers are worked out, for people: derived, so it says so.
const LAYER_SOURCE: &str = "Inferred from each model's first folder under the model paths (its dbt fqn), not declared. Seeds have none.";

/// The Catalog's facts, with `last_builds` from the same snapshot the plan is made
/// against. Never fails: what can't be read is left out, and said so in the server log.
pub(super) fn catalog(
    manifest: &Manifest,
    target_dir: &Path,
    last_builds: BTreeMap<String, LastBuild>,
    links: &Links,
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
    let mut tests = tests_by_node(manifest);
    let nodes: Vec<CatalogNode> = manifest
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.resource_type,
                ResourceType::Model | ResourceType::Seed | ResourceType::Snapshot
            )
        })
        .map(|n| {
            let mut node = node(n, warehouse.as_ref());
            node.relation_link = links.fields(n.relation_name.as_deref());
            node.tests = tests.remove(n.unique_id.as_str()).unwrap_or_default();
            node
        })
        .collect();
    let names = manifest
        .nodes
        .iter()
        .filter(|n| n.resource_type == ResourceType::Source)
        .map(|n| (n.unique_id.clone(), display_name(&n.unique_id)))
        .collect();
    let input = CatalogInput::new(nodes)
        .with_names(names)
        .with_last_builds(last_builds)
        .with_warehouse_as_of(warehouse.and_then(|w| w.generated_at));
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

fn node(n: &ManifestNode, warehouse: Option<&Catalog>) -> CatalogNode {
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
    // Never `compiled_code`: it can hold values resolved from `env_var()`, `var()` or
    // macros, such as credentials (AGENTS rule 9). The raw code keeps them unresolved.
    node.columns = columns(n, warehouse);
    node
}

/// Every node's tests, in one pass over the manifest: each data test on the nodes it
/// reads or is attached to, each unit test on the model it names (`model`). A test is
/// covered by the node's checks when it reads the node, as the state's test record
/// counts them (`checks_of`).
fn tests_by_node(manifest: &Manifest) -> BTreeMap<&str, Vec<CatalogTest>> {
    let mut tests: BTreeMap<&str, Vec<CatalogTest>> = BTreeMap::new();
    for t in manifest
        .nodes
        .iter()
        .filter(|n| n.resource_type == ResourceType::Test)
    {
        let attached = t.test.as_ref().and_then(|d| d.attached_node.as_deref());
        let name = match &t.test {
            Some(d) => match &d.namespace {
                Some(namespace) => format!("{namespace}.{}", d.name),
                None => d.name.clone(),
            },
            None => display_name(&t.unique_id),
        };
        let mut targets: Vec<&str> = t.depends_on.iter().map(String::as_str).collect();
        targets.extend(attached);
        targets.sort_unstable();
        targets.dedup();
        for target in targets {
            let column = t
                .test
                .as_ref()
                .filter(|_| attached == Some(target))
                .and_then(|d| d.column_name.clone());
            tests.entry(target).or_default().push(
                CatalogTest::new(t.unique_id.clone(), name.clone(), column, TestKind::Data)
                    .covered(t.depends_on.iter().any(|d| d == target)),
            );
        }
    }
    // A unit test names its model; its package is its id's second part.
    let models: BTreeMap<(&str, &str), Vec<&str>> = manifest
        .nodes
        .iter()
        .filter(|n| n.resource_type == ResourceType::Model)
        .fold(BTreeMap::new(), |mut map, n| {
            let package = n.unique_id.split('.').nth(1).unwrap_or_default();
            if let Some(name) = n.name.as_deref() {
                map.entry((package, name))
                    .or_insert_with(Vec::new)
                    .push(n.unique_id.as_str());
            }
            map
        });
    for u in &manifest.unit_tests {
        let Some(model) = u.definition.get("model").and_then(|m| m.as_str()) else {
            continue;
        };
        let package = u.unique_id.split('.').nth(1).unwrap_or_default();
        let name = u.unique_id.rsplit('.').next().unwrap_or(&u.unique_id);
        for &target in models.get(&(package, model)).into_iter().flatten() {
            // Of a versioned model's versions, the ones it reads.
            if u.depends_on.iter().any(|d| d == target) {
                tests.entry(target).or_default().push(
                    CatalogTest::new(u.unique_id.clone(), name, None, TestKind::Unit).covered(true),
                );
            }
        }
    }
    for list in tests.values_mut() {
        list.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    }
    tests
}

/// In warehouse order when `catalog.json` has the node, else a seed's file order, else
/// the declared columns; then any declared column the others miss. Each says what
/// lists it, so one only the warehouse catalog lists can be told apart: the catalog
/// may be older than the code.
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
    let has = |list: Option<&Vec<String>>, name: &str| {
        list.is_some_and(|l| l.iter().any(|c| c.eq_ignore_ascii_case(name)))
    };
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
            if has(Some(&n.declared_columns), &name) {
                column.listed_by.push(ColumnSource::Declared);
            }
            if has(from_warehouse, &name) {
                column.listed_by.push(ColumnSource::WarehouseCatalog);
            }
            if has(n.file_columns.as_ref(), &name) {
                column.listed_by.push(ColumnSource::File);
            }
            column
        })
        .collect()
}

/// Each node's last successful build in `latest`, with the snapshot that recorded its
/// run (from `history`). The caller reads both once, with the snapshot it plans
/// against, so the Catalog's builds and decisions can't come from different
/// snapshots. A build's passing checks count only if they are the node's checks in
/// `manifest` now: the digest is the planner's (`checks_digest`), compared as the
/// state does (`NodeState::is_tested_with`), so a test added or edited since doesn't
/// read as passed.
pub(super) fn last_builds(
    latest: Option<&StoredSnapshot>,
    history: &[SnapshotSummary],
    manifest: &Manifest,
) -> BTreeMap<String, LastBuild> {
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
                let now = checks_digest(manifest, id);
                build = build.with_tested(
                    tested.run_id.clone(),
                    tested.at,
                    tested.checks.clone(),
                    state.is_tested_with(now.as_deref()),
                );
            }
            (id.clone(), build)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ods_core::state::{
        Fingerprint, NodeState, SnapshotId, StateSnapshot, TestRecord, Timestamp,
    };

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10")
            .join(name)
    }

    /// The fixture's manifest, changed by `edit` first.
    fn manifest(edit: impl FnOnce(&mut serde_json::Value)) -> Manifest {
        let path = fixture("manifest.json");
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        edit(&mut json);
        Manifest::parse(&path, &json.to_string()).unwrap()
    }

    #[test]
    fn layers_come_from_the_fqn_folders_only() {
        let manifest = manifest(|_| {});
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

    #[test]
    fn tags_are_a_string_or_a_list() {
        let manifest = manifest(|m| {
            let nodes = &mut m["nodes"];
            nodes["model.jaffle_ods.orders"]["config"]["tags"] = serde_json::json!("core");
            nodes["model.jaffle_ods.customers"]["config"]["tags"] =
                serde_json::json!(["pii", "core", "pii"]);
        });
        let by_id = manifest.nodes_by_id();
        assert_eq!(tags(by_id["model.jaffle_ods.orders"]), ["core"]);
        assert_eq!(
            tags(by_id["model.jaffle_ods.customers"]),
            ["core", "pii"],
            "sorted, once each"
        );
        assert!(
            tags(by_id["model.jaffle_ods.stg_orders"]).is_empty(),
            "{:?}",
            tags(by_id["model.jaffle_ods.stg_orders"])
        );
    }

    #[test]
    fn columns_say_where_their_type_and_existence_come_from() {
        let manifest = manifest(|m| {
            let columns = &mut m["nodes"]["model.jaffle_ods.customers"]["columns"];
            columns["customer_id"]["data_type"] = serde_json::json!("bigint");
            columns["note"] = serde_json::json!({"name": "note", "data_type": "string"});
        });
        let customers = manifest.nodes_by_id()["model.jaffle_ods.customers"];
        let mut warehouse = Catalog::default();
        warehouse.columns.insert(
            customers.unique_id.clone(),
            vec!["CUSTOMER_ID".into(), "old_column".into()],
        );
        warehouse.types.insert(
            customers.unique_id.clone(),
            BTreeMap::from([
                ("CUSTOMER_ID".to_owned(), "INTEGER".to_owned()),
                ("OLD_COLUMN".to_owned(), "TEXT".to_owned()),
            ]),
        );
        let columns = columns(customers, Some(&warehouse));
        let by_name = |name: &str| columns.iter().find(|c| c.name == name).unwrap();

        // The warehouse's type wins, matched whatever the case.
        let id = by_name("CUSTOMER_ID");
        assert_eq!(
            id.data_type,
            Some(("INTEGER".to_owned(), TypeSource::Warehouse))
        );
        assert_eq!(
            id.listed_by,
            [ColumnSource::Declared, ColumnSource::WarehouseCatalog]
        );
        assert_eq!(id.description.as_deref(), Some("Primary key."));
        // Only declared: its declared type, marked so.
        let note = by_name("note");
        assert_eq!(
            note.data_type,
            Some(("string".to_owned(), TypeSource::Declared))
        );
        assert_eq!(note.listed_by, [ColumnSource::Declared]);
        // Only the warehouse catalog lists it: the page can mark it possibly dropped.
        assert_eq!(
            by_name("old_column").listed_by,
            [ColumnSource::WarehouseCatalog]
        );
        // Without a warehouse catalog, no type is invented.
        let bare = super::columns(customers, None);
        assert!(bare.iter().all(|c| {
            c.data_type
                .as_ref()
                .is_none_or(|(_, s)| *s == TypeSource::Declared)
        }));
    }

    #[test]
    fn tests_are_listed_on_the_nodes_they_read_and_marked_covered() {
        let manifest = manifest(|_| {});
        let tests = tests_by_node(&manifest);
        let customers = &tests["model.jaffle_ods.customers"];
        let names: Vec<(&str, Option<&str>, bool)> = customers
            .iter()
            .map(|t| (t.name.as_str(), t.column.as_deref(), t.covered_by_checks))
            .collect();
        // Its own column tests, and the relationships test from orders that reads it
        // (no column: the column is orders').
        assert!(
            names.contains(&("not_null", Some("customer_id"), true)),
            "{names:?}"
        );
        assert!(
            names.contains(&("unique", Some("customer_id"), true)),
            "{names:?}"
        );
        assert!(names.contains(&("relationships", None, true)), "{names:?}");
        // Every one of the node's checks is listed.
        let checks =
            ods_provider_dbt::fingerprint::checks_of(&manifest, "model.jaffle_ods.customers");
        for check in checks {
            assert!(customers.iter().any(|t| t.id == check), "{check}");
        }
    }

    #[test]
    fn last_builds_come_from_the_snapshot_given() {
        let at = Timestamp::parse("2026-09-28T09:00:00Z").unwrap();
        let mut node = NodeState::new(
            Fingerprint::from_content([("sql", "a")]),
            at,
            "run-2",
            BTreeMap::new(),
        );
        node.tested = Some(TestRecord::new("run-2", at, "digest"));
        let mut snapshot = StateSnapshot::new(None, at, "run-2", BTreeMap::new());
        snapshot
            .nodes
            .insert("model.x.orders".to_owned(), node.clone());
        let mut old = node;
        old.run_id = "run-1".to_owned();
        snapshot.nodes.insert("seed.x.raw".to_owned(), old);
        let latest = StoredSnapshot::new(SnapshotId(2), snapshot);
        let history = vec![
            SnapshotSummary::new(SnapshotId(2), Some(SnapshotId(1)), at, "run-2", 2),
            SnapshotSummary::new(SnapshotId(1), None, at, "run-1", 1),
        ];
        let fixture = manifest(|_| {});
        let builds = last_builds(Some(&latest), &history, &fixture);
        assert_eq!(builds["model.x.orders"].snapshot, Some(2));
        assert_eq!(builds["seed.x.raw"].snapshot, Some(1), "kept from run 1");
        assert!(builds["model.x.orders"].tested.is_some());
        assert!(
            last_builds(None, &history, &fixture).is_empty(),
            "no snapshot, no builds"
        );
        // A run the history doesn't reach: the snapshot is unknown, not guessed.
        assert_eq!(
            last_builds(Some(&latest), &history[..1], &fixture)["seed.x.raw"].snapshot,
            None
        );
    }

    /// A test added after the checks last passed isn't vouched for: the recorded
    /// digest no longer matches the node's checks.
    #[test]
    fn checks_passed_count_only_while_the_checks_are_the_same() {
        let id = "model.jaffle_ods.customers";
        let before = manifest(|_| {});
        let digest = checks_digest(&before, id).expect("customers has checks");
        let at = Timestamp::parse("2026-09-28T09:00:00Z").unwrap();
        let mut node = NodeState::new(
            Fingerprint::from_content([("sql", "a")]),
            at,
            "run-1",
            BTreeMap::new(),
        );
        node.tested = Some(TestRecord::new("run-1", at, digest.clone()));
        let mut snapshot = StateSnapshot::new(None, at, "run-1", BTreeMap::new());
        snapshot.nodes.insert(id.to_owned(), node);
        let latest = StoredSnapshot::new(SnapshotId(1), snapshot);
        let history = vec![SnapshotSummary::new(SnapshotId(1), None, at, "run-1", 1)];

        let same = last_builds(Some(&latest), &history, &before);
        assert!(same[id].checks_current, "nothing changed since they passed");
        assert_eq!(same[id].tested_checks.as_deref(), Some(digest.as_str()));

        // A new test on customers, not run since.
        let after = manifest(|m| {
            let nodes = m["nodes"].as_object_mut().unwrap();
            let (_, template) = nodes
                .iter()
                .find(|(k, _)| k.starts_with("test.jaffle_ods.unique_customers_customer_id"))
                .unwrap();
            let mut added = template.clone();
            let new_id = "test.jaffle_ods.accepted_values_customers_value_tier.0000000001";
            added["unique_id"] = serde_json::json!(new_id);
            added["name"] = serde_json::json!("accepted_values_customers_value_tier");
            added["column_name"] = serde_json::json!("value_tier");
            added["test_metadata"]["name"] = serde_json::json!("accepted_values");
            nodes.insert(new_id.to_owned(), added);
        });
        let changed = last_builds(Some(&latest), &history, &after);
        assert!(
            !changed[id].checks_current,
            "a test added since hasn't run: the record no longer vouches"
        );
        assert!(
            changed[id].tested.is_some(),
            "the record itself is still shown"
        );
    }
}
