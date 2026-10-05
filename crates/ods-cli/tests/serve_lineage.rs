//! `ods serve`'s Lineage page and State overlay (#312), end to end: the binary, the fake
//! dbt, a recorded run and HTTP.

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
    /// e.g. `http://127.0.0.1:41234/`.
    url: String,
    _home: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve(target: &Path, cwd: Option<&Path>, extra: &[&str]) -> Server {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["serve", "--target-dir"])
        .arg(target)
        .args(["--port", "0", "--json", "--no-watch"])
        .args(extra)
        .current_dir(cwd.unwrap_or(home.path()))
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

/// `method path` relative to the server URL: (status, body).
fn request(server: &Server, method: &str, path: &str) -> (u16, String) {
    let rest = server.url.strip_prefix("http://").unwrap();
    let (host, base) = rest.split_once('/').unwrap();
    let mut stream = TcpStream::connect(host).unwrap();
    write!(
        stream,
        "{method} /{base}{path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
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

fn get(server: &Server, path: &str) -> (u16, String) {
    request(server, "GET", path)
}

fn overlay(server: &Server) -> Value {
    let (status, body) = get(server, "api/lineage/overlay");
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[test]
fn without_a_state_store_every_node_shows_as_never_built() {
    let target = fixtures("jaffle-ods/artifacts/dbt-1.10");
    let server = serve(&target, None, &[]);
    let overlay = overlay(&server);
    assert_eq!(overlay["state"], "no_store", "{overlay}");
    let nodes = overlay["nodes"].as_object().unwrap();
    assert_eq!(nodes.len(), 13, "every model and seed: {overlay}");
    assert!(nodes.values().all(|n| n["decision"] == "never_built"));
    // No plan, so no decision to explain on the State plan page.
    assert!(nodes.values().all(|n| n["why_href"].is_null()));
    let (status, page) = get(&server, "lineage");
    assert_eq!(status, 200);
    assert!(page.contains(r#"aria-current="page" data-section="lineage""#));
    assert_eq!(request(&server, "POST", "api/lineage/overlay").0, 405);
    follow_links(&server, &overlay);
}

/// Every node's Model and Why links, as the side panel gives them, resolve through the
/// server to a page about that node (#313's `/catalog/<id>`, #311's
/// `/state/plan?node=`).
fn follow_links(server: &Server, overlay: &Value) {
    let nodes = overlay["nodes"].as_object().unwrap();
    assert!(!nodes.is_empty());
    for (id, node) in nodes {
        let name = id.rsplit('.').next().unwrap();
        for link in ["model_href", "why_href"] {
            // No plan entry, nothing to explain: no Why link.
            let Some(href) = node[link].as_str() else {
                assert_eq!(link, "why_href", "{id}: every node has a Model page");
                assert!(overlay["based_on"].is_null(), "{id}: planned, but no Why");
                continue;
            };
            let (status, page) = get(server, href);
            assert_eq!(status, 200, "{id}: {link} {href}");
            assert!(
                page.contains(name),
                "{id}: {link} {href} is about another node"
            );
        }
    }
}

/// `ods state build` with the fake dbt (a Python script, so Unix only), then a change
/// to `customers`: the overlay shows what `ods state plan` would do.
#[cfg(unix)]
#[test]
fn the_overlay_is_the_plan_against_the_recorded_state() {
    use std::fs;

    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    fs::create_dir_all(dir.join("base")).unwrap();
    fs::copy(
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

    // Edit `customers` after the run, as a developer would (a comment alone wouldn't
    // count: the fingerprint ignores formatting).
    let manifest_path = target.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let customers = &mut manifest["nodes"]["model.jaffle_ods.customers"];
    for field in ["raw_code", "compiled_code"] {
        let code = customers[field].as_str().unwrap().to_owned();
        customers[field] = Value::String(code.replacen("select", "select distinct", 1));
    }
    customers["checksum"]["checksum"] = Value::String("0".repeat(64));
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let before = fs::read(&db).unwrap();

    let server = serve(&target, Some(dir), &["--state-db", db.to_str().unwrap()]);
    let overlay = overlay(&server);
    assert_eq!(overlay["state"], "recorded", "{overlay}");
    assert_eq!(overlay["based_on"], 1);
    let node = |id: &str| overlay["nodes"][id].clone();
    let customers = node("model.jaffle_ods.customers");
    assert_eq!(customers["decision"], "build", "{overlay}");
    assert_eq!(customers["summary"], "code changed");
    assert_eq!(customers["last_built"]["snapshot"], 1);
    assert_eq!(node("model.jaffle_ods.orders")["decision"], "reuse");
    // The Python model reads customers: the DAG says so even though its code can't be
    // analyzed, and it builds too.
    let segments = node("model.jaffle_ods.customer_segments");
    assert_eq!(segments["decision"], "build");
    assert_eq!(segments["opaque"], "Python model: column lineage unknown");
    let (_, graph) = get(&server, "api/graph");
    let graph: Value = serde_json::from_str(&graph).unwrap();
    assert!(
        graph["node_edges"].as_array().unwrap().iter().any(|e| {
            e["from"] == "model.jaffle_ods.customers"
                && e["to"] == "model.jaffle_ods.customer_segments"
        }),
        "{}",
        graph["node_edges"]
    );

    follow_links(&server, &overlay);
    // The deep link selects the node.
    let (status, page) = get(&server, "lineage?node=model.jaffle_ods.customers");
    assert_eq!(status, 200);
    assert!(page.contains(r#"id="ods-selected">"model.jaffle_ods.customers"</script>"#));
    drop(server);
    assert_eq!(fs::read(&db).unwrap(), before, "the page only reads");

    assert_agrees_with_plan(&overlay, &target, &db, dir);
}

/// The overlay says what `ods state plan` says: the same nodes, decisions and main
/// reasons, and reuse taken on trust.
#[cfg(unix)]
fn assert_agrees_with_plan(overlay: &Value, target: &Path, db: &Path, dir: &Path) {
    // The same decisions as `ods state plan`.
    let plan = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["state", "plan", "--json", "--target-dir"])
        .arg(target)
        .arg("--state-db")
        .arg(db)
        .current_dir(dir)
        .env_clear()
        .output()
        .unwrap();
    let plan: Value = serde_json::from_slice(&plan.stdout).unwrap();
    let entries = plan["result"]["plan"]["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("{plan}"));
    assert_eq!(entries.len(), 13, "{plan}");
    // The same nodes both ways…
    let planned: std::collections::BTreeSet<&str> = entries
        .iter()
        .map(|e| e["node"].as_str().unwrap())
        .collect();
    let shown: std::collections::BTreeSet<&str> = overlay["nodes"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(planned, shown);
    // …the same decision, and the same main reason.
    for entry in entries {
        let node = &overlay["nodes"][entry["node"].as_str().unwrap()];
        let action = entry["action"].as_str().unwrap();
        assert!(
            (action == "reuse") == (node["decision"] == "reuse"),
            "{}: plan says {action}, overlay {}",
            entry["node"],
            node["decision"]
        );
        assert_eq!(
            node["reasons"][0]["code"], entry["reasons"][0]["code"],
            "{}",
            entry["node"]
        );
        if action == "reuse" {
            assert_eq!(
                node["relation"],
                "not checked: this page doesn't query the warehouse; `ods state build --dry-run` checks",
                "the page's plan doesn't check the warehouse, and says so"
            );
        }
    }
}

/// A state database that can't be read: every node is unknown (it would build), never
/// reused, and the page still loads.
#[test]
fn an_unreadable_store_shows_every_node_as_unknown() {
    let scratch = tempfile::tempdir().unwrap();
    let db = scratch.path().join("state.db");
    std::fs::write(&db, "not a database").unwrap();
    let target = fixtures("jaffle-ods/artifacts/dbt-1.10");
    let server = serve(&target, None, &["--state-db", db.to_str().unwrap()]);
    let overlay = overlay(&server);
    assert_eq!(overlay["state"], "unreadable", "{overlay}");
    assert!(overlay["error"].is_string(), "said on loopback: {overlay}");
    let nodes = overlay["nodes"].as_object().unwrap();
    assert_eq!(nodes.len(), 13);
    assert!(
        nodes.values().all(|n| n["decision"] == "unknown"),
        "{overlay}"
    );
    assert_eq!(get(&server, "lineage").0, 200);
}

/// The plan can't be made when the project's own inputs are broken (here, the source
/// freshness results): every node is unknown, not reused.
#[test]
fn a_plan_that_cant_be_made_shows_every_node_as_unknown() {
    let scratch = tempfile::tempdir().unwrap();
    let sources = scratch.path().join("sources.json");
    std::fs::write(&sources, "{").unwrap();
    let target = fixtures("jaffle-ods/artifacts/dbt-1.10");
    let server = serve(&target, None, &["--sources", sources.to_str().unwrap()]);
    let overlay = overlay(&server);
    assert_eq!(overlay["state"], "project_unreadable", "{overlay}");
    assert!(
        overlay["nodes"]
            .as_object()
            .unwrap()
            .values()
            .all(|n| n["decision"] == "unknown"),
        "{overlay}"
    );
    assert_eq!(get(&server, "lineage").0, 200);
}

/// Compiled SQL can hold values resolved from `env_var()`, `var()` or macros,
/// credentials included. None of it reaches the lineage graph, the Lineage page, its
/// overlay, Home or the offline export: not even where the analyzer gives up on it.
#[test]
fn a_secret_in_compiled_sql_never_reaches_the_lineage_outputs() {
    const SECRET: &str = "sk_live_ods_TEST_SECRET_42";
    let scratch = tempfile::tempdir().unwrap();
    let target = scratch.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let fixture = fixtures("jaffle-ods/artifacts/dbt-1.10");
    std::fs::copy(fixture.join("catalog.json"), target.join("catalog.json")).unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(fixture.join("manifest.json")).unwrap()).unwrap();
    // One model the analyzer can't read (it quotes what it can't read), one it can.
    manifest["nodes"]["model.jaffle_ods.order_events"]["compiled_code"] =
        Value::String(format!("select * from table(generator('{SECRET}'))"));
    manifest["nodes"]["model.jaffle_ods.orders"]["compiled_code"] = Value::String(format!(
        "select id from orders where status = '{SECRET}' '{SECRET}'"
    ));
    std::fs::write(
        target.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();

    let server = serve(&target, None, &[]);
    for path in [
        "api/graph",
        "api/node?id=model.jaffle_ods.order_events",
        "api/node?id=model.jaffle_ods.orders",
        "api/lineage/overlay",
        "lineage",
        "lineage?node=model.jaffle_ods.order_events",
        "",
        "api/home",
    ] {
        let (status, body) = get(&server, path);
        assert_eq!(status, 200, "{path}");
        assert!(!body.contains(SECRET), "{path} shows the compiled SQL");
    }
    let (_, graph) = get(&server, "api/graph");
    assert!(
        graph.contains("did not parse") || graph.contains("unsupported"),
        "the analyzer still says why"
    );

    for args in [
        vec!["lineage", "view", "--output-file"],
        vec!["lineage", "graph", "--format", "json", "--output-file"],
    ] {
        let out_file = scratch.path().join("out");
        let out = Command::new(env!("CARGO_BIN_EXE_ods"))
            .args(&args)
            .arg(&out_file)
            .arg("--target-dir")
            .arg(&target)
            .current_dir(scratch.path())
            .env_clear()
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let written = std::fs::read_to_string(&out_file).unwrap();
        assert!(
            !written.contains(SECRET),
            "{args:?} writes the compiled SQL"
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains(SECRET));
        assert!(!String::from_utf8_lossy(&out.stderr).contains(SECRET));
    }
}

/// `ods lineage impact --output json`'s `run` for these `--column` specs.
fn cli_impact(target: &Path, specs: &[&str]) -> Vec<String> {
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_ods"));
    command
        .args(["lineage", "impact", "--target-dir"])
        .arg(target)
        .args(["--output", "json"])
        .current_dir(home.path())
        .env_clear()
        .env("XDG_CONFIG_HOME", home.path())
        .envs(std::env::var_os("SystemRoot").map(|root| ("SystemRoot", root)));
    for spec in specs {
        command.args(["--column", spec]);
    }
    let output = command.output().unwrap();
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    envelope["result"]["run"]
        .as_array()
        .unwrap_or_else(|| panic!("{envelope}"))
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect()
}

/// The Impact simulator (#347) reaches exactly what `ods lineage impact` does, for
/// each kind of change, on the demo project.
#[test]
fn the_impact_simulator_reaches_what_ods_lineage_impact_says() {
    let target = fixtures("jaffle-ods/artifacts/dbt-1.10");
    let server = serve(&target, None, &[]);
    for (query, specs) in [
        (
            "column=orders.amount&change=drop",
            vec!["orders.amount=removed"],
        ),
        (
            "column=orders.amount&change=retype&to=decimal",
            vec!["orders.amount=modified"],
        ),
        (
            "column=orders.amount&change=rename&to=total_amount",
            vec!["orders.amount=removed", "orders.total_amount=added"],
        ),
        (
            "column=orders.status&change=drop",
            vec!["orders.status=removed"],
        ),
        (
            "column=customers.lifetime_value&change=drop",
            vec!["customers.lifetime_value=removed"],
        ),
    ] {
        let (status, body) = get(&server, &format!("api/lineage/impact?{query}"));
        assert_eq!(status, 200, "{body}");
        let view: Value = serde_json::from_str(&body).unwrap();
        let reached: Vec<String> = view["result"]["reached"]
            .as_array()
            .unwrap_or_else(|| panic!("{query}: {view}"))
            .iter()
            .map(|id| id.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(reached, cli_impact(&target, &specs), "{query}");
        assert!(!reached.is_empty(), "{query}: the demo has readers");
        // The Python model's lineage is unknown: it is never "not affected".
        let python = view["result"]["must_run"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == "customer_segments");
        if let Some(python) = python {
            assert_eq!(python["lineage"], "opaque", "{query}");
            assert!(python["columns"].is_null(), "{query}");
        }
    }
    // The page, opened from a column's link on the Model page.
    let (_, model) = get(&server, "catalog/model.jaffle_ods.orders?tab=columns");
    assert!(
        model.contains(r#"href="../lineage/impact?column=model.jaffle_ods.orders.amount""#),
        "the Columns tab links each column to the simulator"
    );
    let (status, page) = get(&server, "lineage/impact?column=orders.amount&change-0=drop");
    assert_eq!(status, 200);
    assert!(page.contains(r#"<tr data-node="model.jaffle_ods.customers" data-verdict="breaks">"#));
}

/// The ERD page (#64) draws the relationships `ods erd generate --infer` finds, on the
/// demo project, and says how to test each untested one in dbt's own YAML.
#[test]
fn the_erd_page_shows_what_ods_erd_generate_finds() {
    let target = fixtures("jaffle-ods/artifacts/dbt-1.10");
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["erd", "generate", "--target-dir"])
        .arg(&target)
        .args(["--format", "json", "--infer"])
        .current_dir(home.path())
        .env_clear()
        .env("XDG_CONFIG_HOME", home.path())
        .envs(std::env::var_os("SystemRoot").map(|root| ("SystemRoot", root)))
        .output()
        .unwrap();
    let cli: Value = serde_json::from_slice(&output.stdout).unwrap();
    let server = serve(&target, None, &[]);
    let (status, body) = get(&server, "api/erd");
    assert_eq!(status, 200, "{body}");
    let page: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(page["erd"]["relationships"], cli["relationships"]);
    assert_eq!(page["erd"]["entities"], cli["entities"]);
    assert_ne!(
        page["missing"].as_array().map(Vec::len),
        Some(0),
        "untested relationships are listed"
    );
    let snippets: Vec<&str> = page["missing"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["suggestion"]["snippet"].as_str())
        .collect();
    assert!(
        snippets
            .iter()
            .any(|s| s.contains("relationships:") && s.contains("to: ref('customers')")),
        "{snippets:?}"
    );
    let (status, html) = get(&server, "erd");
    assert_eq!(status, 200);
    assert!(html.contains(r#"aria-current="page" data-section="erd""#));
}
