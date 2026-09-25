//! `ods lineage` end to end on the committed `jaffle-ods` dbt fixture (#74, #100).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10")
}

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("ods-cli-lineage-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ods(args: &[&str]) -> Output {
    let home = Temp::new();
    Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(args)
        .current_dir(&home.0)
        .env_clear()
        .env("XDG_CONFIG_HOME", &home.0)
        .output()
        .expect("failed to spawn ods")
}

fn json(args: &[&str]) -> Value {
    let mut all = args.to_vec();
    all.push("--json");
    let out = ods(&all);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    envelope["result"].clone()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn columns_trace_every_model_column_to_its_inputs() {
    let target = fixture();
    let result = json(&[
        "lineage",
        "columns",
        "--target-dir",
        target.to_str().unwrap(),
        "--model",
        "customers",
    ]);
    assert_eq!(result["summary"]["dialect"], "duckdb");
    assert_eq!(
        result["summary"]["models_opaque"], 1,
        "the Python model customer_segments"
    );
    let model = &result["models"][0];
    assert_eq!(model["unique_id"], "model.jaffle_ods.customers");
    let lifetime = model["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "lifetime_value")
        .unwrap();
    assert_eq!(lifetime["inputs"][0]["column"], "orders.amount");
    assert_eq!(
        lifetime["inputs"][0]["edge"],
        serde_json::json!({"type": "direct", "subtype": "aggregation"})
    );
}

#[test]
fn impact_prunes_readers_that_do_not_use_the_changed_column() {
    let target = fixture();
    let result = json(&[
        "lineage",
        "impact",
        "--target-dir",
        target.to_str().unwrap(),
        "--column",
        "stg_payments.payment_method",
    ]);
    assert_eq!(strings(&result["run"]), ["model.jaffle_ods.orders"]);
    let pruned: Vec<&str> = result["impact"]["pruned"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["node"].as_str().unwrap())
        .collect();
    assert_eq!(
        pruned,
        [
            "model.jaffle_ods.customer_order_rank",
            "model.jaffle_ods.customers",
            "model.jaffle_ods.order_events"
        ]
    );
}

#[test]
fn impact_against_a_base_build_finds_the_real_change_and_its_consumers() {
    let head = Temp::new();
    for file in ["manifest.json", "catalog.json"] {
        fs::copy(fixture().join(file), head.0.join(file)).unwrap();
    }
    let manifest_path = head.0.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let code = &mut manifest["nodes"]["model.jaffle_ods.orders"]["compiled_code"];
    let changed = code.as_str().unwrap().replace(
        "coalesce(p.total_amount, 0) as amount",
        "coalesce(p.total_amount, 0) * 1.2 as amount",
    );
    *code = Value::String(changed);
    fs::write(&manifest_path, manifest.to_string()).unwrap();

    let base = fixture();
    let result = json(&[
        "lineage",
        "impact",
        "--target-dir",
        head.0.to_str().unwrap(),
        "--base",
        base.to_str().unwrap(),
    ]);
    assert_eq!(
        strings(&result["changed_models"]),
        ["model.jaffle_ods.orders"]
    );
    assert_eq!(
        result["changes"].as_array().unwrap().len(),
        1,
        "only `amount` changed"
    );
    assert_eq!(
        strings(&result["run"]),
        [
            "model.jaffle_ods.customer_order_rank",
            // A Python model is opaque: it may use any column of `customers`, so it runs,
            // and so does everything reading it.
            "model.jaffle_ods.customer_segments",
            "model.jaffle_ods.customers",
            "model.jaffle_ods.customers_snapshot_view",
            "model.jaffle_ods.orders",
            "model.jaffle_ods.segment_summary"
        ]
    );
    // `order_events` doesn't read `orders` at all, so it's neither run nor pruned.
    assert!(!result.to_string().contains("order_events"));
}

#[test]
fn export_writes_openlineage_job_events_with_column_lineage() {
    let out_dir = Temp::new();
    let file = out_dir.0.join("events.ndjson");
    let target = fixture();
    let result = json(&[
        "lineage",
        "export",
        "--target-dir",
        target.to_str().unwrap(),
        "--namespace",
        "duckdb://local",
        "--event-time",
        "2026-09-25T00:00:00Z",
        "--output-file",
        file.to_str().unwrap(),
    ]);
    assert_eq!(result["events"], 10, "every model, the Python one included");
    assert_eq!(result["with_column_lineage"], 9, "all but the Python model");
    let events: Vec<Value> = fs::read_to_string(&file)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    // The Python model: table-level lineage from what it declares, no column facet.
    let python = events
        .iter()
        .find(|e| e["job"]["name"] == "model.jaffle_ods.customer_segments")
        .expect("an event for the Python model");
    assert_eq!(python["inputs"][0]["name"], "jaffle_ods.main.customers");
    assert!(python["outputs"][0].get("facets").is_none(), "{python:#}");
    let orders = events
        .iter()
        .find(|e| e["job"]["name"] == "model.jaffle_ods.orders")
        .unwrap();
    assert!(
        orders["schemaURL"]
            .as_str()
            .unwrap()
            .ends_with("#/$defs/JobEvent")
    );
    let facet = &orders["outputs"][0]["facets"]["columnLineage"];
    assert!(
        facet["_schemaURL"]
            .as_str()
            .unwrap()
            .contains("ColumnLineageDatasetFacet")
    );
    assert_eq!(
        facet["fields"]["amount"]["inputFields"][0]["name"],
        "jaffle_ods.main.stg_payments"
    );
    assert_eq!(
        facet["fields"]["amount"]["inputFields"][0]["transformations"][0]["subtype"],
        "AGGREGATION"
    );
    // Deterministic: the same inputs give byte-identical events.
    let again = out_dir.0.join("again.ndjson");
    json(&[
        "lineage",
        "export",
        "--target-dir",
        target.to_str().unwrap(),
        "--namespace",
        "duckdb://local",
        "--event-time",
        "2026-09-25T00:00:00Z",
        "--output-file",
        again.to_str().unwrap(),
    ]);
    assert_eq!(fs::read(&file).unwrap(), fs::read(&again).unwrap());
}

#[test]
fn errors_have_stable_codes_and_exit_statuses() {
    let missing = ods(&[
        "lineage",
        "columns",
        "--target-dir",
        "no/such/dir",
        "--json",
    ]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stdout).contains("ODS-E0201"));

    let target = fixture();
    let unknown = ods(&[
        "lineage",
        "impact",
        "--target-dir",
        target.to_str().unwrap(),
        "--column",
        "nope.x",
        "--json",
    ]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown.stdout).contains("ODS-E0203"));
}

#[test]
fn graph_exports_every_format_and_focus_narrows_it() {
    let out_dir = Temp::new();
    let target = fixture();
    let target = target.to_str().unwrap();
    let write = |format: &str, extra: &[&str]| {
        let file = out_dir.0.join(format!("graph.{format}"));
        let mut args = vec![
            "lineage",
            "graph",
            "--target-dir",
            target,
            "--format",
            format,
            "--output-file",
            file.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        let result = json(&args);
        (result, fs::read_to_string(&file).unwrap())
    };

    let (result, text) = write("json", &[]);
    assert_eq!(result["nodes"], 13);
    let document: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(document["schema_version"], 1);
    let orders = document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "model.jaffle_ods.orders")
        .unwrap();
    assert_eq!(orders["name"], "orders");
    assert_eq!(orders["layer"], 2, "seeds, staging, then orders");

    assert!(write("dot", &[]).1.starts_with("digraph lineage {"));
    assert!(write("dot-columns", &[]).1.contains(":c0"), "column ports");
    assert!(write("mermaid", &[]).1.starts_with("flowchart LR\n"));
    assert!(write("graphml", &[]).1.contains("<graphml"));

    // Focusing on one column keeps only what feeds it and what it feeds.
    let (focused, _) = write("json", &["--focus", "customers.lifetime_value"]);
    let focused_nodes = focused["nodes"].as_u64().unwrap();
    assert!((5..11).contains(&focused_nodes), "{focused}");
    let (limited, _) = write(
        "json",
        &[
            "--focus",
            "customers",
            "--upstream",
            "1",
            "--downstream",
            "0",
        ],
    );
    assert_eq!(limited["nodes"], 3, "customers plus its direct upstreams");
}

#[test]
fn view_writes_a_self_contained_offline_page() {
    let out_dir = Temp::new();
    let file = out_dir.0.join("lineage.html");
    let target = fixture();
    let result = json(&[
        "lineage",
        "view",
        "--target-dir",
        target.to_str().unwrap(),
        "--output-file",
        file.to_str().unwrap(),
    ]);
    assert_eq!(result["format"], "html");
    let page = fs::read_to_string(&file).unwrap();
    assert!(!page.contains("/*__ODS_GRAPH__*/"), "graph embedded");
    assert!(page.contains("\"schema_version\":1"));
    assert!(
        page.contains(r#"<meta name="ods-source" content="embedded">"#),
        "the page uses its embedded graph and never fetches"
    );
    // Offline: no external scripts, styles, fonts or requests.
    for external in [
        "<script src",
        "<link",
        "@import",
        "XMLHttpRequest",
        "url(http",
    ] {
        assert!(!page.contains(external), "page references `{external}`");
    }
    let urls: Vec<&str> = page
        .match_indices("http")
        .map(|(i, _)| &page[i..(i + 30).min(page.len())])
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .collect();
    assert!(
        urls.iter()
            .all(|u| u.starts_with("http://www.w3.org/2000/svg")),
        "only the SVG namespace, no remote URLs: {urls:?}"
    );
}

#[test]
fn view_can_write_a_static_site_to_host() {
    let out_dir = Temp::new();
    let site = out_dir.0.join("site");
    let target = fixture();
    let result = json(&[
        "lineage",
        "view",
        "--target-dir",
        target.to_str().unwrap(),
        "--site",
        site.to_str().unwrap(),
    ]);
    assert_eq!(result["format"], "site");
    let index = fs::read_to_string(site.join("index.html")).unwrap();
    assert!(index.contains(r#"<meta name="ods-source" content="graph.json">"#));
    let graph: Value = serde_json::from_slice(&fs::read(site.join("graph.json")).unwrap()).unwrap();
    assert_eq!(graph["schema_version"], 1);
    assert_eq!(graph["nodes"].as_array().unwrap().len(), result["nodes"]);
}

#[test]
fn unknown_focus_is_a_usage_error() {
    let target = fixture();
    let out = ods(&[
        "lineage",
        "graph",
        "--target-dir",
        target.to_str().unwrap(),
        "--output-file",
        "x.json",
        "--focus",
        "customers.no_such_column",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("has no column"));
}

#[test]
fn impact_against_a_base_detects_changes_to_nodes_without_sql_lineage() {
    // A seed's CSV changed: dbt's checksum differs, so everything downstream may change.
    let head = Temp::new();
    for file in ["manifest.json", "catalog.json"] {
        fs::copy(fixture().join(file), head.0.join(file)).unwrap();
    }
    let manifest_path = head.0.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["nodes"]["seed.jaffle_ods.raw_payments"]["checksum"]["checksum"] =
        Value::String("changed".into());
    fs::write(&manifest_path, manifest.to_string()).unwrap();

    let base = fixture();
    let result = json(&[
        "lineage",
        "impact",
        "--target-dir",
        head.0.to_str().unwrap(),
        "--base",
        base.to_str().unwrap(),
    ]);
    assert_eq!(
        strings(&result["changed_models"]),
        ["seed.jaffle_ods.raw_payments"]
    );
    let run = strings(&result["run"]);
    for expected in [
        "model.jaffle_ods.stg_payments",
        "model.jaffle_ods.orders",
        "model.jaffle_ods.customers",
        "model.jaffle_ods.order_events",
    ] {
        assert!(
            run.iter().any(|r| r == expected),
            "{expected} must run: {run:?}"
        );
    }
}

fn v2_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-2.0")
}

#[test]
fn dbt_1x_json_v2_json_and_v2_parquet_give_the_same_lineage() {
    let out = Temp::new();
    let graph = |target: &Path, artifacts: &str| {
        let file = out.0.join(format!(
            "{artifacts}-{}.json",
            target.file_name().unwrap().to_str().unwrap()
        ));
        json(&[
            "lineage",
            "graph",
            "--target-dir",
            target.to_str().unwrap(),
            "--artifacts",
            artifacts,
            "--output-file",
            file.to_str().unwrap(),
        ]);
        let document: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
        (
            document["column_edges"].clone(),
            document["node_edges"].clone(),
        )
    };
    let v1 = graph(&fixture(), "json");
    let v2_json = graph(&v2_fixture(), "json");
    let v2_parquet = graph(&v2_fixture(), "info-schema");
    assert!(v1.0.as_array().unwrap().len() > 50);
    assert_eq!(v1, v2_json, "dbt v2's manifest.json matches dbt 1.x");
    assert_eq!(
        v2_json, v2_parquet,
        "the Parquet Information Schema matches the JSON"
    );
}

#[test]
fn a_v2_target_without_json_is_read_from_the_information_schema() {
    // `dbt compile --generate-info-schema --no-write-json`: Parquet plus compiled SQL only.
    let target = Temp::new();
    copy_dir(
        &v2_fixture().join("info_schema"),
        &target.0.join("info_schema"),
    );
    copy_dir(&v2_fixture().join("compiled"), &target.0.join("compiled"));
    assert!(!target.0.join("manifest.json").exists());
    let result = json(&[
        "lineage",
        "columns",
        "--target-dir",
        target.0.to_str().unwrap(),
    ]);
    assert_eq!(result["summary"]["models_analyzed"], 9);
    assert_eq!(result["summary"]["models_opaque"], 1);
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn impact_on_a_column_that_does_not_exist_is_an_error_not_nothing() {
    let target = fixture();
    let target = target.to_str().unwrap();
    let out = ods(&[
        "lineage",
        "impact",
        "--target-dir",
        target,
        "--column",
        "orders.nope",
        "--json",
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a typo must not read as `nothing affected`"
    );
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["diagnostics"][0]["code"], "ODS-E0203");
    // New columns don't exist yet, so `added` accepts any name.
    let added = json(&[
        "lineage",
        "impact",
        "--target-dir",
        target,
        "--column",
        "orders.nope=added",
    ]);
    assert!(added["run"].as_array().unwrap().is_empty());
}

#[test]
fn seeds_get_column_lineage_without_a_catalog() {
    // dbt 2.0's manifest.json, with no catalog: seed columns come from their CSV header.
    let target = fixture().with_file_name("dbt-2.0");
    let target = target.to_str().unwrap();
    let impact = json(&[
        "lineage",
        "impact",
        "--target-dir",
        target,
        "--artifacts",
        "json",
        "--column",
        "raw_orders.status",
    ]);
    let run = strings(&impact["run"]);
    assert!(run.contains(&"model.jaffle_ods.stg_orders".to_owned()));
    assert!(
        !run.contains(&"model.jaffle_ods.order_events".to_owned()),
        "order_events doesn't read status, so a seed column change skips it: {run:?}"
    );

    let columns = json(&[
        "lineage",
        "columns",
        "--target-dir",
        target,
        "--artifacts",
        "json",
        "--model",
        "raw_orders",
    ]);
    let seed = &columns["upstreams"][0];
    assert_eq!(seed["kind"], "seed");
    let status = seed["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "status")
        .unwrap();
    assert_eq!(status["used_by"][0]["column"], "stg_orders.status");
    assert_eq!(
        status["used_by"][0]["used_by"][0]["column"],
        "orders.status"
    );
    let reaches = strings(&status["reaches"]);
    assert!(reaches.contains(&"orders.status".to_owned()));
    assert!(
        reaches.contains(&"customers_snapshot_view.lifetime_value".to_owned()),
        "a filter on status shapes customers' rows, so everything downstream of them: {reaches:?}"
    );
}
