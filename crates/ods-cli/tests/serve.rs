//! `ods serve` end to end: start the binary, talk HTTP to it, watch it reload.

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn fixtures(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dbt")
        .join(path)
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10")
}

/// The server process, killed on drop so a failing test doesn't leak it.
struct Server {
    child: Child,
    /// e.g. `http://127.0.0.1:41234/lineage/`.
    url: String,
    home: tempfile::TempDir,
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
        .args([
            "serve",
            "--target-dir",
            target.to_str().unwrap(),
            "--port",
            "0",
            "--json",
        ])
        .args(extra)
        .current_dir(home.path())
        .env_clear()
        .env("XDG_CONFIG_HOME", home.path())
        // Windows sockets need `SystemRoot`; without it, binding fails.
        .envs(std::env::var_os("SystemRoot").map(|root| ("SystemRoot", root)))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ods");
    // The announcement is printed once the socket is bound.
    let stdout = child.stdout.take().unwrap();
    let envelope: Value = serde_json::Deserializer::from_reader(stdout)
        .into_iter()
        .next()
        .expect("ods serve printed nothing")
        .unwrap();
    assert_eq!(envelope["command"], "serve", "{envelope}");
    let url = envelope["result"]["url"]
        .as_str()
        .unwrap_or_else(|| panic!("ods serve didn't start: {envelope}"))
        .to_owned();
    Server { child, url, home }
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

#[test]
fn serves_the_explorer_and_api_under_a_base_path() {
    let server = serve(&fixture(), &["--base-path", "/lineage", "--no-watch"]);
    assert!(
        server.url.starts_with("http://127.0.0.1:"),
        "loopback by default"
    );
    assert!(server.url.ends_with("/lineage/"), "{}", server.url);

    let (status, page) = get(&server, "");
    assert_eq!(status, 200);
    assert!(page.contains("Project health"), "Home is the first page");
    let (status, page) = get(&server, "lineage");
    assert_eq!(status, 200);
    assert!(page.contains(r#"content="api""#));

    let (_, body) = get(&server, "api/search?q=customers.lifetime");
    let hits: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(hits[0]["node"], "model.jaffle_ods.customers", "{hits}");

    let (status, body) = get(&server, "api/impact?node=stg_payments&column=amount");
    assert_eq!(status, 200);
    assert!(body.contains("model.jaffle_ods.orders"), "{body}");
}

#[test]
fn reloads_when_the_artifacts_change_and_keeps_serving_on_errors() {
    let scratch = tempfile::tempdir().unwrap();
    let target = scratch.path();
    for file in ["manifest.json", "catalog.json"] {
        fs::copy(fixture().join(file), target.join(file)).unwrap();
    }
    let server = serve(target, &[]);
    let generation = |server: &Server| -> (u64, Value) {
        let (_, body) = get(server, "api/version");
        let version: Value = serde_json::from_str(&body).unwrap();
        (
            version["generation"].as_u64().unwrap(),
            version["last_error"].clone(),
        )
    };
    assert_eq!(generation(&server).0, 1);

    let wait_for = |want: &dyn Fn(u64, &Value) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let (g, error) = generation(&server);
            if want(g, &error) {
                return (g, error);
            }
            assert!(
                Instant::now() < deadline,
                "no reload: generation {g}, error {error}"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    };

    // A broken manifest: the last good graph stays up and the error is reported.
    std::thread::sleep(Duration::from_millis(1100));
    fs::write(target.join("manifest.json"), "{").unwrap();
    let (g, _) = wait_for(&|_, e| !e.is_null());
    assert_eq!(g, 1);
    assert_eq!(get(&server, "api/graph").0, 200);

    // Fixed again: a new generation, and the error clears.
    std::thread::sleep(Duration::from_millis(1100));
    fs::copy(
        fixture().join("manifest.json"),
        target.join("manifest.json"),
    )
    .unwrap();
    let (g, _) = wait_for(&|g, e| g == 2 && e.is_null());
    assert_eq!(g, 2);
    drop(server);
}

#[test]
fn a_missing_target_fails_before_listening() {
    let out = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args([
            "serve",
            "--target-dir",
            "/nonexistent/ods",
            "--port",
            "0",
            "--json",
        ])
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        envelope["diagnostics"][0]["code"], "ODS-E0201",
        "{envelope}"
    );
}

#[test]
fn without_a_state_store_home_says_how_to_record_a_first_run() {
    let server = serve(&fixture(), &["--no-watch"]);
    let (status, body) = get(&server, "api/home");
    assert_eq!(status, 200, "{body}");
    let home: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(home["state"], "no_store", "{home}");
    assert_eq!(home["scope"], "jaffle_ods/default");
    assert_eq!(home["empty"]["commands"][0]["command"], "ods state build");
    assert_eq!(home["tiles"][0]["value"], 13, "{home}");
    let (_, body) = get(&server, "api/shell");
    let shell: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(shell["project"], "jaffle_ods");
    assert_eq!(shell["read_only"], true);
    let (status, page) = get(&server, "");
    assert_eq!(status, 200);
    assert!(page.contains("No runs recorded yet"));
    assert!(
        !server.home.path().join(".ods").exists(),
        "the dashboard never creates a state store"
    );
}

/// `ods state build` with the fake dbt (a Python script, so Unix only), then the
/// dashboard over the state it recorded.
#[cfg(unix)]
#[test]
fn home_shows_the_runs_the_state_store_recorded_and_changes_nothing() {
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
    let before = fs::read(&db).unwrap();

    let server = serve(&target, &["--no-watch", "--state-db", db.to_str().unwrap()]);
    let (status, body) = get(&server, "api/home");
    assert_eq!(status, 200, "{body}");
    let home: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(home["state"], "recorded", "{home}");
    assert_eq!(home["runs"].as_array().unwrap().len(), 1, "{home}");
    assert_eq!(home["runs"][0]["snapshot"], 1);
    assert_eq!(home["runs"][0]["built"], 13, "{home}");
    assert_eq!(home["runs"][0]["outcome"], "recorded");
    assert_eq!(home["plan"]["build"], 0, "nothing changed since: {home}");
    let opaque: Vec<&Value> = home["attention"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["kind"] == "opaque")
        .collect();
    assert_eq!(opaque[0]["node"], "customer_segments", "{home}");
    assert_eq!(opaque[0]["why"], "Python model: column lineage unknown");
    let (_, body) = get(&server, "api/shell");
    let shell: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(shell["snapshot"]["id"], 1, "{shell}");
    let (status, page) = get(&server, "");
    assert_eq!(status, 200);
    assert!(page.contains("13 built"), "{page}");
    drop(server);
    assert_eq!(fs::read(&db).unwrap(), before, "the dashboard only reads");
}
