//! `ods serve`'s Catalog and model pages end to end (#313): start the binary on the
//! jaffle fixture, with and without the state `ods state build` recorded.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde_json::Value;

fn fixtures(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dbt")
        .join(path)
}

/// The server process, killed on drop so a failing test doesn't leak it.
struct Server {
    child: Child,
    url: String,
    _home: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve(target: &Path, extra: &[&str]) -> Server {
    serve_with(target, extra, None)
}

/// `ods serve`, with `ods_toml` as the project's `ods.toml` when given.
fn serve_with(target: &Path, extra: &[&str], ods_toml: Option<&str>) -> Server {
    let home = tempfile::tempdir().unwrap();
    if let Some(toml) = ods_toml {
        std::fs::write(home.path().join("ods.toml"), toml).unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["serve", "--target-dir", target.to_str().unwrap()])
        .args(["--port", "0", "--json", "--no-watch"])
        .args(extra)
        .current_dir(home.path())
        .env_clear()
        .env("XDG_CONFIG_HOME", home.path())
        .envs(std::env::var_os("SystemRoot").map(|root| ("SystemRoot", root)))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ods");
    let stdout = child.stdout.take().unwrap();
    let envelope: Value = serde_json::Deserializer::from_reader(stdout)
        .into_iter()
        .next()
        .expect("ods serve printed nothing")
        .unwrap();
    let url = envelope["result"]["url"]
        .as_str()
        .unwrap_or_else(|| panic!("ods serve didn't start: {envelope}"))
        .to_owned();
    Server {
        child,
        url,
        _home: home,
    }
}

/// GET `path` relative to the server URL: (status, body).
fn get(server: &Server, path: &str) -> (u16, String) {
    let rest = server.url.strip_prefix("http://").unwrap();
    let (host, base) = rest.split_once('/').unwrap();
    let mut stream = TcpStream::connect(host).unwrap();
    write!(
        stream,
        "GET /{base}{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (
        head.split(' ').nth(1).unwrap().parse().unwrap(),
        body.to_owned(),
    )
}

fn json(server: &Server, path: &str) -> Value {
    let (status, body) = get(server, path);
    assert_eq!(status, 200, "{path}: {body}");
    serde_json::from_str(&body).unwrap()
}

fn counts(view: &Value, facet: &str) -> Vec<(String, u64)> {
    view["facets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == facet)
        .unwrap()["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["value"].as_str().unwrap().to_owned(),
                v["count"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn pairs(list: &[(&str, u64)]) -> Vec<(String, u64)> {
    list.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
}

#[test]
fn without_a_state_store_the_catalog_lists_the_manifest_and_never_built() {
    let server = serve(&fixtures("jaffle-ods/artifacts/dbt-1.10"), &[]);
    let view = json(&server, "api/catalog");
    assert_eq!(view["schema_version"], 3);
    assert_eq!(view["total"], 13, "10 models and 3 seeds: {view}");
    assert_eq!(counts(&view, "type"), pairs(&[("model", 10), ("seed", 3)]));
    assert_eq!(
        counts(&view, "layer"),
        pairs(&[("staging", 3), ("marts", 7)]),
        "the folders under models/, upstream first"
    );
    assert_eq!(
        counts(&view, "materialized"),
        pairs(&[("table", 6), ("view", 4), ("seed", 3)])
    );
    assert!(counts(&view, "tag").is_empty(), "the fixture has no tags");
    assert_eq!(
        counts(&view, "decision"),
        pairs(&[
            ("build", 0),
            ("reuse", 0),
            ("never_built", 13),
            ("unknown", 0)
        ])
    );
    for row in view["rows"].as_array().unwrap() {
        assert!(row["last_build"].is_null(), "{row}");
    }
    let (status, page) = get(&server, "catalog");
    assert_eq!(status, 200);
    assert!(page.contains("13 of 13 nodes"), "{page}");

    // A model page: types from catalog.json, tests from the manifest.
    let model = json(&server, "api/catalog/model.jaffle_ods.customers");
    assert_eq!(model["layer"], "marts");
    assert_eq!(model["decision"]["decision"], "never_built");
    assert!(model["last_build"].is_null());
    let id = model["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "customer_id")
        .unwrap()
        .clone();
    assert_eq!(id["type_source"], "warehouse", "{id}");
    assert_eq!(id["description"], "Primary key.");
    let mut tests: Vec<&str> = id["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    tests.sort_unstable();
    assert_eq!(tests, ["not_null", "unique"]);
    assert!(
        model["code"]["raw"]
            .as_str()
            .unwrap()
            .contains("ref('orders')")
    );
    let (status, page) = get(&server, "catalog/model.jaffle_ods.customers?tab=columns");
    assert_eq!(status, 200);
    assert!(page.contains("customer_id"));
    let (status, _) = get(&server, "catalog/model.jaffle_ods.nope");
    assert_eq!(status, 404);

    // A Python model is opaque, and says so.
    let view = json(&server, "api/catalog?lineage=opaque");
    let ids: Vec<&str> = view["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["model.jaffle_ods.customer_segments"]);
    assert_eq!(view["rows"][0]["type_label"], "python model");
}

/// `ods state build` with the fake dbt (a Python script, so Unix only), then the
/// Catalog over the state it recorded: its decisions are the plan's, and each node's
/// last build is the run that built it.
#[cfg(unix)]
#[test]
fn the_catalog_shows_the_plan_and_the_builds_the_state_store_recorded() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    std::fs::create_dir_all(dir.join("base")).unwrap();
    std::fs::copy(
        fixtures("jaffle-ods/artifacts/dbt-1.10-build/manifest.json"),
        dir.join("base/manifest.json"),
    )
    .unwrap();
    let target = dir.join("target");
    let db = dir.join(".ods/state.db");
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["state", "build", "--dbt"])
        .arg(fixtures("fake-dbt/dbt"))
        .args(["--dbt-output", "capture", "--target-dir"])
        .arg(&target)
        .arg("--state-db")
        .arg(&db)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("FAKE_DBT_BASE", dir.join("base"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let before = std::fs::read(&db).unwrap();

    let server = serve(&target, &["--state-db", db.to_str().unwrap()]);
    let home = json(&server, "api/home");
    let view = json(&server, "api/catalog");
    assert_eq!(view["decisions"]["state"], "recorded", "{view}");
    assert_eq!(view["decisions"]["based_on"], 1);
    // The facet counts are the plan's.
    let decisions: std::collections::BTreeMap<String, u64> =
        counts(&view, "decision").into_iter().collect();
    assert_eq!(
        decisions["build"] + decisions["never_built"],
        home["plan"]["build"].as_u64().unwrap(),
        "{view}"
    );
    assert_eq!(decisions["reuse"], home["plan"]["reuse"].as_u64().unwrap());
    assert_eq!(decisions["reuse"], 13, "nothing changed since the build");
    let run = home["runs"][0]["run_id"].as_str().unwrap();
    // The builds shown and the decisions rest on the same snapshot: every build is
    // one the plan's snapshot records, never a later one.
    let based_on = view["decisions"]["based_on"].as_u64().unwrap();
    for row in view["rows"].as_array().unwrap() {
        assert_eq!(row["last_build"]["snapshot"], 1, "{row}");
        assert!(
            row["last_build"]["snapshot"].as_u64().unwrap() <= based_on,
            "{row}"
        );
        assert_eq!(row["last_build"]["run_id"], run, "{row}");
        assert_eq!(row["decision"]["decision"], "reuse", "{row}");
    }
    // Health (#354): everything was built, and the build's record says nothing failed.
    for row in view["rows"].as_array().unwrap() {
        let health = row["health"]["health"].as_str().unwrap();
        assert!(matches!(health, "healthy" | "warning"), "{row}");
    }
    let failing = home["health"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "failing")
        .unwrap();
    assert_eq!(
        failing["count"], 0,
        "measured from the run's record: {failing}"
    );
    // Reuse is offline: the relation isn't claimed to be checked.
    assert_eq!(view["decisions"]["relations_checked"], false);
    assert!(
        view["decisions"]["caveats"][0]
            .as_str()
            .unwrap()
            .contains("isn't checked by this plan"),
        "{view}"
    );

    let model = json(&server, "api/catalog/model.jaffle_ods.orders");
    assert_eq!(model["decision"]["decision"], "reuse");
    assert_eq!(
        model["decision"]["reasons"][0]["code"], "unchanged",
        "{model}"
    );
    let (status, page) = get(&server, "catalog/model.jaffle_ods.orders?tab=state");
    assert_eq!(status, 200);
    assert!(page.contains("REUSE"), "{page}");
    assert!(page.contains("From the plan against snapshot 1"));
    drop(server);
    assert_eq!(
        std::fs::read(&db).unwrap(),
        before,
        "the Catalog only reads"
    );
}

/// Compiled code can hold values resolved from `env_var()` or `var()`, such as
/// credentials: it never reaches the pages or the API, even on loopback (AGENTS rule 9).
#[test]
fn compiled_code_and_its_secrets_are_never_served() {
    const SECRET: &str = "sk_live_0123456789abcdefSECRET";
    let scratch = tempfile::tempdir().unwrap();
    let target = scratch.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let source = fixtures("jaffle-ods/artifacts/dbt-1.10");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(source.join("manifest.json")).unwrap())
            .unwrap();
    let customers = &mut manifest["nodes"]["model.jaffle_ods.customers"];
    customers["compiled_code"] =
        Value::String(format!("select * from orders where api_key = '{SECRET}'"));
    customers["raw_code"] = Value::String(
        "select * from {{ ref('orders') }} where api_key = '{{ env_var(\"API_KEY\") }}'".into(),
    );
    std::fs::write(target.join("manifest.json"), manifest.to_string()).unwrap();
    std::fs::copy(source.join("catalog.json"), target.join("catalog.json")).unwrap();

    let server = serve(&target, &[]);
    let (status, body) = get(&server, "api/catalog/model.jaffle_ods.customers");
    assert_eq!(status, 200, "{body}");
    assert!(!body.contains(SECRET), "{body}");
    assert!(
        body.contains("env_var"),
        "the raw code, unresolved, is shown: {body}"
    );
    for tab in ["overview", "code", "columns", "lineage", "state", "tests"] {
        let (status, page) = get(
            &server,
            &format!("catalog/model.jaffle_ods.customers?tab={tab}"),
        );
        assert_eq!(status, 200);
        assert!(!page.contains(SECRET), "{tab}: {page}");
    }
    let (_, body) = get(&server, "api/catalog");
    assert!(!body.contains(SECRET));
}

/// A `sources.json` measuring `source.jaffle_ods.landing.feed`.
fn sources_json(generated_at: &str, max_loaded_at: &str) -> Value {
    serde_json::json!({
        "metadata": {
            "dbt_schema_version": "https://schemas.getdbt.com/dbt/sources/v3.json",
            "generated_at": generated_at,
            "invocation_id": "freshness"
        },
        "results": [{
            "unique_id": "source.jaffle_ods.landing.feed",
            "status": "pass",
            "max_loaded_at": max_loaded_at,
            "snapshotted_at": generated_at
        }],
        "elapsed_time": 0.1
    })
}

/// `ods` on the scratch project, with `--json`: the result.
fn ods_json(target: &Path, db: &Path, args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(args)
        .arg("--target-dir")
        .arg(target)
        .arg("--state-db")
        .arg(db)
        .arg("--json")
        .current_dir(target.parent().unwrap())
        .env_clear()
        .env("XDG_CONFIG_HOME", target.parent().unwrap())
        .output()
        .unwrap();
    let json: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    assert!(out.status.success(), "{json:#}");
    json["result"].clone()
}

/// The source `stg_orders` reads, as `ods state explain` sees it: its version's value
/// and exactness.
fn explained_version(target: &Path, db: &Path) -> (Value, Value) {
    let explain = ods_json(target, db, &["state", "explain", "stg_orders"]);
    let evidence = explain["explanation"]["entry"]["evidence"]
        .as_array()
        .unwrap_or_else(|| panic!("no evidence: {explain:#}"))
        .iter()
        .find(|e| e["kind"] == "source_data_version")
        .unwrap_or_else(|| panic!("no source version: {explain:#}"))
        .clone();
    (evidence["value"].clone(), evidence["exactness"].clone())
}

fn input<'a>(view: &'a Value, id: &str) -> &'a Value {
    view["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id)
        .unwrap_or_else(|| panic!("no {id}: {view:#}"))
}

/// The Freshness evidence screen (#350) on the demo project with a source added: its
/// seeds, and the source with and without `sources.json`, each source's numbers as
/// `ods state explain` gives them for a model reading it.
#[test]
fn freshness_evidence_matches_what_explain_says() {
    let scratch = tempfile::tempdir().unwrap();
    let target = scratch.path().join("target");
    let db = scratch.path().join(".ods/state.db");
    std::fs::create_dir_all(&target).unwrap();
    for file in ["manifest.json", "run_results.json"] {
        std::fs::copy(
            fixtures("jaffle-ods/artifacts/dbt-1.10-build").join(file),
            target.join(file),
        )
        .unwrap();
    }
    // A source `stg_orders` reads, whose new data is measured with `_loaded_at`.
    let path = target.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["sources"]["source.jaffle_ods.landing.feed"] = serde_json::json!({
        "unique_id": "source.jaffle_ods.landing.feed",
        "resource_type": "source",
        "name": "feed",
        "source_name": "landing",
        "relation_name": "\"landing\".\"feed\"",
        "loaded_at_field": "_loaded_at",
        "config": {"enabled": true}
    });
    manifest["nodes"]["model.jaffle_ods.stg_orders"]["depends_on"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("source.jaffle_ods.landing.feed"));
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    // Measured just before the build, then recorded.
    let sources = target.join("sources.json");
    std::fs::write(
        &sources,
        sources_json("2026-09-25T06:40:00Z", "2026-09-25T06:30:00+00:00").to_string(),
    )
    .unwrap();
    ods_json(&target, &db, &["state", "record"]);

    // Without `sources.json`: the source's version is unknown, and its reader builds.
    std::fs::remove_file(&sources).unwrap();
    let server = serve(&target, &["--state-db", db.to_str().unwrap()]);
    let view = json(&server, "api/catalog/sources");
    assert_eq!(
        (view["sources"].as_u64(), view["seeds"].as_u64()),
        (Some(1), Some(3))
    );
    let feed = input(&view, "source.jaffle_ods.landing.feed");
    assert_eq!(feed["evidence"]["grade"], "unknown", "{feed:#}");
    assert!(feed["evidence"]["value"].is_null());
    assert_eq!(feed["evidence"]["method"], "max(_loaded_at)");
    assert_eq!(feed["relation"], "\"landing\".\"feed\"");
    let (value, exactness) = explained_version(&target, &db);
    assert!(value.is_null(), "explain agrees: {value}");
    assert_eq!(exactness, "none");
    let reader = &feed["readers"][0];
    assert_eq!(reader["node"]["id"], "model.jaffle_ods.stg_orders");
    assert_eq!(reader["decision"]["decision"], "build", "{reader:#}");
    assert_eq!(
        reader["decision"]["reasons"][0]["code"],
        "missing_data_evidence"
    );
    // What it was last built from is still recorded.
    assert_eq!(feed["recorded"][0]["grade"], "semantic", "{feed:#}");
    // The seeds: exact checksums, unchanged since the build, so reused.
    for seed in ["raw_customers", "raw_orders", "raw_payments"] {
        let seed = input(&view, &format!("seed.jaffle_ods.{seed}"));
        assert_eq!(seed["evidence"]["grade"], "exact", "{seed:#}");
        assert_eq!(seed["decision"]["decision"], "reuse", "{seed:#}");
        assert_eq!(seed["evidence"]["value"], seed["recorded"][0]["value"]);
        assert_ne!(seed["downstream"].as_array().unwrap().len(), 0, "{seed:#}");
    }
    let (status, page) = get(&server, "catalog/sources");
    assert_eq!(status, 200);
    assert!(page.contains(r#"data-input="source.jaffle_ods.landing.feed""#));
    drop(server);

    // Measured again after the build, with new data: semantic, and its reader builds.
    std::fs::write(
        &sources,
        sources_json("2026-09-25T11:00:00Z", "2026-09-25T10:45:00+00:00").to_string(),
    )
    .unwrap();
    let server = serve(&target, &["--state-db", db.to_str().unwrap()]);
    let view = json(&server, "api/catalog/sources");
    let feed = input(&view, "source.jaffle_ods.landing.feed");
    let (value, exactness) = explained_version(&target, &db);
    assert_eq!(feed["evidence"]["value"], value, "{feed:#}");
    assert_eq!(feed["evidence"]["grade"], exactness);
    assert_eq!(feed["evidence"]["grade"], "semantic");
    assert_ne!(feed["evidence"]["value"], feed["recorded"][0]["value"]);
    assert_eq!(
        feed["readers"][0]["decision"]["reasons"][0]["code"],
        "new_upstream_data"
    );
    assert_eq!(view["measured_by"], "sources.json");
    let downstream: Vec<&str> = feed["downstream"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect();
    assert!(
        downstream.contains(&"stg_orders") && downstream.contains(&"orders"),
        "{downstream:?}"
    );
}

/// On Databricks, runs read a source's Delta table version before deciding (ADR-0022);
/// the dashboard never connects, so its Freshness evidence screen names that version
/// and says it isn't read there, rather than calling the source unmeasured (#350).
#[test]
fn on_databricks_freshness_evidence_names_the_table_version_runs_read() {
    let scratch = tempfile::tempdir().unwrap();
    let target = scratch.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let mut manifest: Value = serde_json::from_slice(
        &std::fs::read(fixtures(
            "jaffle-ods/artifacts/dbt-1.10-build/manifest.json",
        ))
        .unwrap(),
    )
    .unwrap();
    manifest["metadata"]["adapter_type"] = serde_json::json!("databricks");
    // A source with no `loaded_at_field`: only its table version could say it changed.
    manifest["sources"]["source.jaffle_ods.landing.feed"] = serde_json::json!({
        "unique_id": "source.jaffle_ods.landing.feed",
        "resource_type": "source",
        "name": "feed",
        "source_name": "landing",
        "relation_name": "`main`.`landing`.`feed`",
        "config": {"enabled": true}
    });
    manifest["nodes"]["model.jaffle_ods.stg_orders"]["depends_on"]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!("source.jaffle_ods.landing.feed"));
    std::fs::write(
        target.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let server = serve(&target, &[]);
    let view = json(&server, "api/catalog/sources");
    let feed = input(&view, "source.jaffle_ods.landing.feed");
    assert_eq!(
        feed["evidence"]["method"], "table version from the Delta history (read when a run starts)",
        "{feed:#}"
    );
    assert_eq!(feed["evidence"]["grade"], "unknown", "nothing read here");
    assert!(
        feed["evidence"]["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().starts_with("not read here")),
        "{feed:#}"
    );
}

/// Health and coverage (#354) on the demo project: coverage counted from the manifest
/// (each checked here against the manifest itself), and without a state store, every
/// node's health unknown and failures not measured, never 0.
#[test]
fn health_and_coverage_come_from_the_project_and_say_what_isnt_measured() {
    let target = fixtures("jaffle-ods/artifacts/dbt-1.10");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(target.join("manifest.json")).unwrap()).unwrap();
    let nodes = manifest["nodes"].as_object().unwrap();
    let models: std::collections::BTreeSet<&str> = nodes
        .iter()
        .filter(|(_, n)| n["resource_type"] == "model")
        .map(|(id, _)| id.as_str())
        .collect();
    let mut tested = std::collections::BTreeSet::new();
    for test in nodes
        .values()
        .filter(|n| n["resource_type"] == "test")
        .chain(
            manifest["unit_tests"]
                .as_object()
                .into_iter()
                .flat_map(|u| u.values()),
        )
    {
        for id in test["depends_on"]["nodes"].as_array().unwrap() {
            tested.insert(id.as_str().unwrap());
        }
    }
    let expected_tested = models.iter().filter(|m| tested.contains(*m)).count();

    let server = serve(&target, &[]);
    let home = json(&server, "api/home");
    let coverage = |key: &str| {
        home["coverage"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap()
            .clone()
    };
    let tests = coverage("tests");
    assert_eq!(tests["total"].as_u64(), Some(models.len() as u64));
    assert_eq!(
        tests["count"].as_u64(),
        Some(expected_tested as u64),
        "{tests:#}"
    );
    assert_eq!(
        tests["uncovered"].as_array().unwrap().len(),
        models.len() - expected_tested
    );
    // No sources in the demo project: not measured, never 0 of 0.
    assert!(coverage("source_freshness")["count"].is_null());
    let health = |key: &str| {
        home["health"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == key)
            .unwrap()
            .clone()
    };
    assert_eq!(health("unknown")["count"], 13, "nothing was ever built");
    assert_eq!(health("healthy")["count"], 0);
    assert!(
        health("failing")["count"].is_null(),
        "no run record: not measured"
    );
    let (status, page) = get(&server, "catalog?health=unknown");
    assert_eq!(status, 200);
    assert!(
        page.contains("13 of 13 nodes"),
        "the badge's link lists them"
    );
}

/// `[health]` tunes the badges (#392, ADR-0030): with `tests_required` off, an
/// untested model built by the fake dbt is no longer a warning.
#[cfg(unix)]
#[test]
fn health_settings_in_ods_toml_tune_the_badges() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    std::fs::create_dir_all(dir.join("base")).unwrap();
    std::fs::copy(
        fixtures("jaffle-ods/artifacts/dbt-1.10-build/manifest.json"),
        dir.join("base/manifest.json"),
    )
    .unwrap();
    let target = dir.join("target");
    let db = dir.join(".ods/state.db");
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["state", "build", "--dbt"])
        .arg(fixtures("fake-dbt/dbt"))
        .args(["--dbt-output", "capture", "--target-dir"])
        .arg(&target)
        .arg("--state-db")
        .arg(&db)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("FAKE_DBT_BASE", dir.join("base"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let db_arg = ["--state-db", db.to_str().unwrap()];
    let health_of = |view: &Value, id: &str| -> String {
        view["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap()["health"]["health"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // stg_customers has no tests in the demo project: a warning by default.
    let untested = "model.jaffle_ods.stg_customers";
    let server = serve(&target, &db_arg);
    assert_eq!(
        health_of(&json(&server, "api/catalog"), untested),
        "warning"
    );
    drop(server);

    let server = serve_with(
        &target,
        &db_arg,
        Some("[health.builtin.tests_required]\nseverity = \"off\"\n"),
    );
    let view = json(&server, "api/catalog");
    assert_eq!(health_of(&view, untested), "healthy", "no longer checked");
    assert!(
        view["health_how"]
            .as_str()
            .unwrap()
            .contains("tests_required (off)"),
        "{}",
        view["health_how"]
    );
    let row = view["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == untested)
        .unwrap()
        .clone();
    assert!(
        row["health"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["check"] != "tests_required"),
        "{row:#}"
    );
    drop(server);
}

/// A `[health]` check that doesn't exist is a configuration error when the server
/// starts (exit 4), naming the real ones, never a silently ignored setting.
#[test]
fn a_misspelt_health_check_is_a_configuration_error() {
    let target = fixtures("jaffle-ods/artifacts/dbt-1.10");
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("ods.toml"),
        "[health.builtin.test_required]\nseverity = \"off\"\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["serve", "--target-dir"])
        .arg(&target)
        .args(["--port", "0", "--json", "--no-watch"])
        .current_dir(home.path())
        .env_clear()
        .env("XDG_CONFIG_HOME", home.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    let message = envelope["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("health.builtin.test_required"),
        "{message}"
    );
    assert!(
        message.contains("tests_required"),
        "names the real checks: {message}"
    );
    assert_eq!(envelope["diagnostics"][0]["code"], "ODS-E0102");
}
