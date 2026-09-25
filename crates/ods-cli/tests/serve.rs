//! `ods serve` end to end: start the binary, talk HTTP to it, watch it reload.

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10")
}

/// The server process, killed on drop so a failing test doesn't leak it.
struct Server {
    child: Child,
    /// e.g. `http://127.0.0.1:41234/lineage/`.
    url: String,
    home: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn serve(target: &Path, extra: &[&str]) -> Server {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let home = std::env::temp_dir().join(format!(
        "ods-cli-serve-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&home).unwrap();
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
        .current_dir(&home)
        .env_clear()
        .env("XDG_CONFIG_HOME", &home)
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
    let url = envelope["result"]["url"].as_str().unwrap().to_owned();
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
    let target = std::env::temp_dir().join(format!("ods-cli-serve-target-{}", std::process::id()));
    let _ = fs::remove_dir_all(&target);
    fs::create_dir_all(&target).unwrap();
    for file in ["manifest.json", "catalog.json"] {
        fs::copy(fixture().join(file), target.join(file)).unwrap();
    }
    let server = serve(&target, &[]);
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
    let _ = fs::remove_dir_all(&target);
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
