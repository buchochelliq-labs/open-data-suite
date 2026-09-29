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
    let home = tempfile::tempdir().unwrap();
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
    assert_eq!(view["schema_version"], 1);
    assert_eq!(view["total"], 13, "10 models and 3 seeds: {view}");
    assert_eq!(counts(&view, "type"), pairs(&[("model", 10), ("seed", 3)]));
    assert_eq!(
        counts(&view, "layer"),
        pairs(&[("marts", 7), ("staging", 3)]),
        "the folders under models/"
    );
    assert_eq!(
        counts(&view, "materialized"),
        pairs(&[("seed", 3), ("table", 6), ("view", 4)])
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
    for row in view["rows"].as_array().unwrap() {
        assert_eq!(row["last_build"]["snapshot"], 1, "{row}");
        assert_eq!(row["last_build"]["run_id"], run, "{row}");
    }

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
