//! `ods serve` end to end: start the binary, talk HTTP to it, watch it reload.

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Only the Unix-only tests (the fake dbt is a Python script) use it.
#[cfg(unix)]
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

/// The server's snapshot generation and last reload error.
fn generation(server: &Server) -> (u64, Value) {
    let (_, body) = get(server, "api/version");
    let version: Value = serde_json::from_str(&body).unwrap();
    (
        version["generation"].as_u64().unwrap(),
        version["last_error"].clone(),
    )
}

fn wait_for_generation(server: &Server, want: u64) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (g, error) = generation(server);
        if g >= want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no reload: generation {g}, error {error}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn a_source_freshness_file_written_later_reloads_the_dashboard() {
    let scratch = tempfile::tempdir().unwrap();
    let target = scratch.path();
    for file in ["manifest.json", "catalog.json"] {
        fs::copy(fixture().join(file), target.join(file)).unwrap();
    }
    // No sources.json yet: it is watched all the same, and creating it is a change.
    let server = serve(target, &[]);
    assert_eq!(generation(&server).0, 1);
    std::thread::sleep(Duration::from_millis(1100));
    fs::write(target.join("sources.json"), "{").unwrap();
    wait_for_generation(&server, 2);
    let (status, body) = get(&server, "api/home");
    assert_eq!(status, 200, "{body}");
    let home: Value = serde_json::from_str(&body).unwrap();
    // A bad freshness file is the project's problem, not the store's.
    assert_eq!(home["state"], "project_unreadable", "{home}");
    let message = home["empty"]["message"].as_str().unwrap();
    assert!(message.contains("sources.json"), "{message}");
    assert!(!message.contains("doctor"), "{message}");
}

/// Runs `ods state build` with the fake dbt in `dir`, with `envs` for the fake dbt.
#[cfg(unix)]
fn fake_build(dir: &Path, envs: &[(&str, &str)], extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["state", "build", "--dbt"])
        .arg(fixtures("fake-dbt/dbt"))
        .args(["--dbt-output", "capture", "--target-dir"])
        .arg(dir.join("target"))
        .arg("--state-db")
        .arg(dir.join(".ods/state.db"))
        .args(extra)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("FAKE_DBT_BASE", dir.join("base"))
        .envs(envs.iter().copied())
        .output()
        .unwrap()
}

/// A first build with the fake dbt in `dir`, then a change to `orders` whose build
/// fails; returns the state database.
#[cfg(unix)]
fn build_then_fail_orders(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir.join("base")).unwrap();
    let base = dir.join("base/manifest.json");
    fs::copy(
        fixtures("jaffle-ods/artifacts/dbt-1.10-build/manifest.json"),
        &base,
    )
    .unwrap();
    let out = fake_build(dir, &[], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Change what `orders` computes, then fail to build it.
    let mut manifest: Value = serde_json::from_slice(&fs::read(&base).unwrap()).unwrap();
    for field in ["raw_code", "compiled_code"] {
        let code = &mut manifest["nodes"]["model.jaffle_ods.orders"][field];
        *code = Value::String(format!("{} union all select 1", code.as_str().unwrap()));
    }
    fs::write(&base, serde_json::to_vec(&manifest).unwrap()).unwrap();
    // Times are kept to the second: a run started in the second a snapshot was
    // recorded can't be told apart from one before it.
    std::thread::sleep(Duration::from_millis(1100));
    // With values that may be secret: the dashboard must never show them (rule 9).
    let out = fake_build(
        dir,
        &[("FAKE_DBT_FAIL", "orders")],
        &[
            "--vars",
            r#"{"password":"hunter2"}"#,
            "--",
            "--log-path",
            "sekrit",
        ],
    );
    assert!(!out.status.success(), "the build of orders fails");
    dir.join(".ods/state.db")
}

#[cfg(unix)]
/// Both runs are in the run ledger (#210, ADR-0029): the savings panel counts them, as
/// estimates, with no cost unless `[state.cost]` sets a rate.
fn savings_are_shown(server: &Server, runs: &Value) {
    let savings = &runs["savings"];
    assert_eq!(savings["run_count"], 2, "{savings}");
    assert_eq!(savings["estimate"], true);
    assert!(savings["cost"].is_null(), "{savings}");
    let (_, page) = get(server, "state/runs");
    assert!(page.contains(r#"data-state="savings""#), "{page}");
}

/// The State pages (#311) over what `ods state build` recorded: a first build, then a
/// change to `orders` whose build fails. The Why panel says what `ods state explain`
/// says, the failed run is shown with the state it kept and the retry, and nothing is
/// written.
#[cfg(unix)]
#[test]
fn the_state_pages_show_the_plan_the_runs_and_a_failed_run() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    let db = build_then_fail_orders(dir);
    // What `ods state explain` says, for the Why panel to match. It runs first: it
    // opens the database for writing, and closing it may checkpoint what a run left in
    // the write-ahead log into the file, changing its bytes but not its data. After it,
    // only the dashboard, which opens it read-only, touches the file.
    let home = tempfile::tempdir().unwrap();
    let explain = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args([
            "state",
            "explain",
            "orders",
            "--output",
            "json",
            "--target-dir",
        ])
        .arg(dir.join("target"))
        .arg("--state-db")
        .arg(&db)
        .current_dir(home.path())
        .env_clear()
        .env("XDG_CONFIG_HOME", home.path())
        .output()
        .unwrap();
    let explained: Value = serde_json::from_slice(&explain.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&explain.stderr)));
    let before = fs::read(&db).unwrap();

    let server = serve(
        &dir.join("target"),
        &["--no-watch", "--state-db", db.to_str().unwrap()],
    );
    let (status, body) = get(&server, "api/state/plan");
    assert_eq!(status, 200, "{body}");
    let plan: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(plan["based_on"], 1, "{plan}");
    let orders = plan["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "orders")
        .unwrap();
    assert_eq!(orders["action"], "build", "{orders}");
    assert_eq!(orders["code"], "code_changed", "{orders}");

    // The Why panel says what `ods state explain` says.
    let (status, body) = get(&server, "api/state/plan/model.jaffle_ods.orders");
    assert_eq!(status, 200, "{body}");
    let why: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(why["explanation"], explained["result"]["explanation"]);
    assert_eq!(why["verdict"], explained["result"]["verdict"]);
    assert_eq!(why["fingerprint"]["components"][0]["name"], "sql", "{why}");
    assert_eq!(why["fingerprint"]["components"][0]["changed"], true);
    let (status, page) = get(&server, "state/plan?node=orders");
    assert_eq!(status, 200);
    assert!(page.contains(r#"aria-label="Why orders builds""#), "{page}");

    // One run recorded; the failed one recorded nothing, and says so. Both ran dbt, so
    // their journals give their outcomes (#322): the failed one is listed from its
    // journal alone.
    let (status, body) = get(&server, "api/state/runs");
    assert_eq!(status, 200, "{body}");
    let runs: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(runs["runs"].as_array().unwrap().len(), 2, "{runs}");
    savings_are_shown(&server, &runs);
    let failed = journal_rows(&runs);
    orders_failed_in(&server, failed["run_id"].as_str().unwrap());
    let last = &runs["last_run"];
    assert_eq!(
        last["failed"][0]["node"], "model.jaffle_ods.orders",
        "{last}"
    );
    assert_eq!(last["recorded_nothing_inferred"], true, "{last}");
    assert_eq!(last["scope"], "jaffle_ods/default");
    // Option names stay; their values and dbt's own options don't.
    assert!(
        !body.contains("hunter2") && !body.contains("sekrit"),
        "{body}"
    );
    assert!(body.contains("--vars '<redacted>'"), "{body}");
    assert_eq!(last["last_good"], 1);
    assert_eq!(runs["failed"], 1);
    // The retry it suggests is a command this ODS has, with what the run withheld to
    // give again as placeholders, never the values (#321).
    assert_eq!(
        last["next"][0]["command"],
        "ods state retry --failed --vars '<value>' -- '<dbt arguments>'"
    );
    let help = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["state", "retry", "--help"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("--failed"));
    let (_, page) = get(&server, "state/runs");
    assert!(page.contains("kept 1"), "{page}");
    assert!(
        page.contains("Last good state when it ran: snapshot 1"),
        "{page}"
    );
    assert!(
        !page.contains("hunter2") && !page.contains("sekrit"),
        "{page}"
    );
    let run_id = runs["runs"][1]["run_id"].as_str().unwrap();
    let (status, page) = get(&server, &format!("state/runs/{run_id}"));
    assert_eq!(status, 200);
    assert!(page.contains("first recorded build"), "{page}");
    assert!(!page.contains("[duration]") && !page.contains("[wall clock]"));
    let (status, _) = get(&server, &format!("api/state/runs/{run_id}"));
    assert_eq!(status, 200);
    drop(server);
    assert_eq!(fs::read(&db).unwrap(), before, "the dashboard only reads");
}

/// The runs listed from their journals (#322): the failed one first, by itself, then
/// the recorded one, with their outcomes. Returns the failed run's row.
#[cfg(unix)]
fn journal_rows(runs: &Value) -> &Value {
    let failed = &runs["runs"][0];
    assert_eq!(failed["snapshot"], Value::Null, "{failed}");
    assert_eq!(failed["outcome"], "failed", "{failed}");
    assert_eq!(failed["from_last_run"], true, "{failed}");
    assert_eq!(failed["kept_state"], 1, "{failed}");
    assert_eq!(runs["runs"][1]["built"], 13);
    assert_eq!(runs["runs"][1]["outcome"], "succeeded");
    assert_eq!(runs["runs"][1]["outcome_from"], "journal");
    assert!(runs["runs"][1]["duration"].is_string(), "{runs}");
    assert_eq!(runs["last_run_listed"], false, "its journal lists it");
    failed
}

/// The page of `run_id`, a run that recorded nothing, lists `orders` as failed, from
/// its journal (#322).
#[cfg(unix)]
fn orders_failed_in(server: &Server, run_id: &str) {
    let (status, run) = get(server, &format!("api/state/runs/{run_id}"));
    assert_eq!(status, 200, "{run}");
    let run: Value = serde_json::from_str(&run).unwrap();
    let orders = run["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == "model.jaffle_ods.orders")
        .unwrap();
    assert_eq!(orders["status"], "error", "{orders}");
    assert!(orders["error"]["message"].is_string(), "{orders}");
}

/// `ods state <command>` with the fake dbt in `dir`, as `fake_build` runs `build`.
#[cfg(unix)]
fn fake_state(dir: &Path, command: &str, envs: &[(&str, &str)]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["state", command, "--dbt"])
        .arg(fixtures("fake-dbt/dbt"))
        .args(["--dbt-output", "capture", "--target-dir"])
        .arg(dir.join("target"))
        .arg("--state-db")
        .arg(dir.join(".ods/state.db"))
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("FAKE_DBT_BASE", dir.join("base"))
        .envs(envs.iter().copied())
        .output()
        .unwrap()
}

/// `ods state test` keeps how it ended, like `run` and `build` (#311 review): its
/// run shows on the Runs page, for its scope, tied to the snapshot it recorded. A test
/// run isn't offered `retry --failed`, which only retries builds.
#[cfg(unix)]
#[test]
fn a_test_run_is_the_last_run_tied_to_its_snapshot() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    fs::create_dir_all(dir.join("base")).unwrap();
    fs::copy(
        fixtures("jaffle-ods/artifacts/dbt-1.10-build/manifest.json"),
        dir.join("base/manifest.json"),
    )
    .unwrap();
    // Built without tests, so `test` has something to run.
    let out = fake_state(dir, "run", &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::thread::sleep(Duration::from_millis(1100));
    let out = fake_state(
        dir,
        "test",
        &[("FAKE_DBT_FAIL_TEST", "unique_orders_order_id")],
    );
    assert!(!out.status.success(), "a test fails");
    let kept: Value =
        serde_json::from_slice(&fs::read(dir.join(".ods/state.db.last-run.json")).unwrap())
            .unwrap();
    assert_eq!(kept["command"], "test", "{kept:#}");
    assert_eq!(kept["scope"], "jaffle_ods/default", "{kept:#}");
    assert!(kept["run_id"].is_string(), "{kept:#}");

    let db = dir.join(".ods/state.db");
    let server = serve(
        &dir.join("target"),
        &["--no-watch", "--state-db", db.to_str().unwrap()],
    );
    let (status, body) = get(&server, "api/state/runs");
    assert_eq!(status, 200, "{body}");
    let runs: Value = serde_json::from_str(&body).unwrap();
    let newest = &runs["runs"][0];
    assert_eq!(newest["run_id"], kept["run_id"], "{runs}");
    assert_eq!(newest["from_last_run"], true, "{runs}");
    // Its journal says the run failed, and that other nodes' tests passed.
    assert_eq!(newest["outcome"], "partial", "{runs}");
    assert_eq!(
        newest["command"]
            .as_str()
            .unwrap()
            .split(' ')
            .take(3)
            .collect::<Vec<_>>(),
        ["ods", "state", "test"]
    );
    let last = &runs["last_run"];
    assert_eq!(last["snapshot"], newest["snapshot"], "{last}");
    assert_eq!(last["recorded_nothing_inferred"], false);
    let next: Vec<&str> = last["next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["command"].as_str().unwrap())
        .collect();
    assert_eq!(next, ["ods state retry"], "no --failed after a test run");
}

/// The live run view (#322): while `ods state build` runs, `ods serve` lists it as
/// probably running and streams its journal as it is written, then ends the stream.
#[cfg(unix)]
#[test]
fn a_build_is_streamed_live_while_it_runs() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path();
    fs::create_dir_all(dir.join("base")).unwrap();
    let base = dir.join("base/manifest.json");
    fs::copy(
        fixtures("jaffle-ods/artifacts/dbt-1.10-build/manifest.json"),
        &base,
    )
    .unwrap();
    let out = fake_build(dir, &[], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let db = dir.join(".ods/state.db");
    let journals = dir.join(".ods/state.db.runs");
    let first: Vec<_> = fs::read_dir(&journals)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let server = serve(
        &dir.join("target"),
        &["--no-watch", "--state-db", db.to_str().unwrap()],
    );

    // Everything builds again, slowly, and orders fails.
    let mut build = Command::new(env!("CARGO_BIN_EXE_ods"))
        .args(["state", "build", "--full-refresh", "--dbt"])
        .arg(fixtures("fake-dbt/dbt"))
        .args(["--dbt-output", "capture", "--target-dir"])
        .arg(dir.join("target"))
        .arg("--state-db")
        .arg(&db)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("FAKE_DBT_BASE", dir.join("base"))
        .env("FAKE_DBT_FAIL", "orders")
        .env("FAKE_DBT_NODE_DELAY", "0.15")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // The dashboard finds it: a journal that changed recently and doesn't say it ended.
    let deadline = Instant::now() + Duration::from_secs(30);
    let run_id = loop {
        let (status, body) = get(&server, "api/runs/live");
        assert_eq!(status, 200, "{body}");
        let live: Value = serde_json::from_str(&body).unwrap();
        if let Some(run) = live["runs"].as_array().unwrap().first() {
            assert_eq!(run["status"], "probably_running", "{run}");
            assert!(!first.iter().any(|p| {
                p.to_string_lossy()
                    .contains(run["run_id"].as_str().unwrap())
            }));
            break run["run_id"].as_str().unwrap().to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "the build never showed as running"
        );
        assert!(
            build.try_wait().unwrap().is_none(),
            "the build ended before it was seen"
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    let (events, end, while_running) = stream_run(&server, &run_id, &mut build);
    let status = build.wait().unwrap();
    assert!(!status.success(), "orders fails");
    assert!(while_running > 0, "no event arrived while the build ran");
    let end = end.expect("the stream ends with `end`");
    assert_eq!(end["reason"], "finished", "{end}");
    assert_eq!(end["outcome"], "failed", "{end}");
    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds.first(), Some(&"run_started"), "{kinds:?}");
    assert_eq!(kinds.last(), Some(&"run_finished"), "{kinds:?}");
    // Each node starts before it finishes, and orders failed.
    for (i, e) in events.iter().enumerate() {
        if e["kind"] == "node_finished" && e["stats"]["status"] != "skipped" {
            let node = &e["node"];
            assert!(
                events[..i]
                    .iter()
                    .any(|s| s["kind"] == "node_started" && &s["node"] == node),
                "{node} finished before it started"
            );
        }
    }
    assert!(
        events
            .iter()
            .any(|e| e["node"] == "model.jaffle_ods.orders" && e["stats"]["status"] == "error")
    );
    // Once it ended, it is no longer listed as running (after the list's short reuse).
    wait_not_listed(&server);
}

/// Reads the run's event stream to its end: the run events, the `end` event's data, and
/// how many events arrived while `build` was still running.
#[cfg(unix)]
fn stream_run(
    server: &Server,
    run_id: &str,
    build: &mut Child,
) -> (Vec<Value>, Option<Value>, usize) {
    use std::io::BufRead as _;

    let rest = server.url.strip_prefix("http://").unwrap();
    let (host, base_path) = rest.split_once('/').unwrap();
    let stream = TcpStream::connect(host).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(
        &stream,
        "GET /{base_path}api/runs/{run_id}/events HTTP/1.0\r\nHost: {host}\r\n\r\n"
    )
    .unwrap();
    let mut reader = std::io::BufReader::new(stream);
    let (mut events, mut end, mut while_running) = (Vec::<Value>::new(), None, 0);
    let mut name = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        let line = line.trim_end();
        if let Some(value) = line.strip_prefix("event: ") {
            value.clone_into(&mut name);
        } else if let Some(data) = line.strip_prefix("data: ") {
            let data: Value = serde_json::from_str(data).unwrap();
            if name == "end" {
                end = Some(data);
            } else if name == "run_event" {
                if build.try_wait().unwrap().is_none() {
                    while_running += 1;
                }
                events.push(data);
            }
        }
    }
    (events, end, while_running)
}

/// Waits until `/api/runs/live` lists nothing: it is reused for a moment after a run ends.
#[cfg(unix)]
fn wait_not_listed(server: &Server) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, body) = get(server, "api/runs/live");
        let live: Value = serde_json::from_str(&body).unwrap();
        if live["runs"].as_array().unwrap().is_empty() {
            return;
        }
        assert!(Instant::now() < deadline, "still listed as running: {live}");
        std::thread::sleep(Duration::from_millis(200));
    }
}
