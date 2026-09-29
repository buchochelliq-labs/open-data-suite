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
    let (status, page) = get(&server, "lineage");
    assert_eq!(status, 200);
    assert!(page.contains(r#"aria-current="page" data-section="lineage""#));
    assert_eq!(request(&server, "POST", "api/lineage/overlay").0, 405);
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
