//! An out-of-process warehouse plugin end to end (#392, ADR-0031 §3d): `ods health
//! check` asks it for links and runs probes over its connection, and records the links;
//! the dashboard shows them from the record without ever starting it; `ods plugin test`
//! runs the SDK's link, catalogue and lineage suites against it.
//!
//! The plugin is this test binary itself: started with `--ods-warehouse-fixture`, it
//! serves the warehouse kind `acme_db`, which no built-in plugin serves. The project is
//! the dbt fixture's, its manifest rewritten to name that warehouse.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ods_core::{ColumnRef, RelationName};
use ods_sdk::contracts::error_catalogue::{CatalogueInfo, Classification};
use ods_sdk::contracts::observed_lineage::ObservedLineage;
use ods_sdk::contracts::privileges::Access;
use ods_sdk::contracts::probe::ProbeAnswer;
use ods_sdk::contracts::relation_link::{
    NoRelationLink, RelationLink, path_segment, split_relation,
};
use ods_sdk::protocol::{
    AccessAnswer, ClassifyRequest, ClassifyResponse, DescribeResponse, LinkAnswer, LinkRequest,
    LinkResponse, ObservedLineageRequest, ObservedLineageResponse, PrivilegesRequest,
    PrivilegesResponse, ProbeCall, ProbeResponse, ProbeTargetAnswer, Provided,
};
use serde_json::Value;

const FIXTURE: &str = "--ods-warehouse-fixture";

fn link_for(relation: &str, configured: bool) -> Result<RelationLink, NoRelationLink> {
    if !configured {
        return Err(NoRelationLink::NotConfigured {
            setting: "account".to_owned(),
        });
    }
    let invalid = |why: String| NoRelationLink::InvalidName {
        relation: relation.to_owned(),
        why,
    };
    let parts = split_relation(relation, '"').map_err(invalid)?;
    if parts.len() != 3 {
        return Err(NoRelationLink::NotQualified {
            relation: relation.to_owned(),
            parts: parts.len(),
            needed: 3,
        });
    }
    let segments = parts
        .iter()
        .map(|p| path_segment(p))
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    Ok(RelationLink::new(
        format!("https://acme.example/explore/{}", segments.join("/")),
        "Open in Acme",
    ))
}

/// Plays the warehouse plugin. Every call appends its request to `ran` in the directory
/// it runs in.
#[allow(
    clippy::too_many_lines,
    reason = "one pretend plugin: an arm per request, read in one place"
)]
fn fixture() {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    let request: Value = serde_json::from_slice(&input).unwrap();
    let asked = request["request"].as_str().unwrap().to_owned();
    let mut ran = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("ran")
        .unwrap();
    writeln!(ran, "{asked}").unwrap();
    let out = |json: Vec<u8>| std::io::stdout().write_all(&json).unwrap();
    match asked.as_str() {
        "describe" => {
            let mut answer = DescribeResponse::new("acme", "1.2.0");
            for contract in [
                ods_sdk::contracts::relation_link::RELATION_LINKER,
                ods_sdk::contracts::error_catalogue::ERROR_CATALOGUE,
                ods_sdk::contracts::observed_lineage::OBSERVED_LINEAGE_SOURCE,
                ods_sdk::contracts::probe::RELATION_PROBE,
                ods_sdk::contracts::privileges::RELATION_PRIVILEGES,
            ] {
                answer
                    .provides
                    .push(Provided::new(contract.name, contract.version));
            }
            answer.warehouse = Some("acme_db".to_owned());
            answer.dialect = Some("duckdb".to_owned());
            answer.catalogue = Some(CatalogueInfo::new("acme", "1", "Acme"));
            out(serde_json::to_vec(&answer).unwrap());
        }
        "link" => {
            let request: LinkRequest = serde_json::from_value(request).unwrap();
            let configured = request.settings.values().any(|s| s["account"] == "acme");
            let links = request
                .relations
                .iter()
                .map(|r| LinkAnswer::new(r.clone(), link_for(r, configured)))
                .collect();
            out(serde_json::to_vec(&LinkResponse::new(links)).unwrap());
        }
        "classify" => {
            let request: ClassifyRequest = serde_json::from_value(request).unwrap();
            let classifications: Vec<Classification> = request
                .errors
                .iter()
                .map(|e| {
                    let json = if e.message().contains("no such table") {
                        serde_json::json!({"kind": "recognised", "id": "acme-missing-table",
                            "symptom": "missing_relation", "category": "database",
                            "suggestions": []})
                    } else {
                        serde_json::json!({"kind": "not_recognised", "category": "unknown"})
                    };
                    serde_json::from_value(json).unwrap()
                })
                .collect();
            out(serde_json::to_vec(&ClassifyResponse::new(classifications)).unwrap());
        }
        "observed_lineage" => {
            let request: ObservedLineageRequest = serde_json::from_value(request).unwrap();
            let text = std::fs::read_to_string(&request.export).unwrap();
            let mut lineage = ObservedLineage::default();
            for line in text.lines() {
                let Some((from, to)) = line.split_once(" -> ") else {
                    eprintln!("not an export: {line}");
                    std::process::exit(2);
                };
                let col = |s: &str| {
                    let (relation, column) = s.rsplit_once('.').unwrap();
                    ColumnRef::new(RelationName::new(relation.split('.')).unwrap(), column)
                };
                lineage.add_column_edge(col(from), col(to));
            }
            out(serde_json::to_vec(&ObservedLineageResponse::new(lineage)).unwrap());
        }
        "probe" => {
            let call: ProbeCall = serde_json::from_value(request).unwrap();
            let targets = call
                .targets
                .iter()
                .map(|t| {
                    let rows = call
                        .probe
                        .statements()
                        .iter()
                        .map(|s| {
                            s.columns()
                                .iter()
                                .map(|c| (c.clone(), "3".to_owned()))
                                .collect()
                        })
                        .collect();
                    ProbeTargetAnswer::new(t.id.clone(), ProbeAnswer::Rows(rows))
                })
                .collect();
            out(serde_json::to_vec(&ProbeResponse::new(targets)).unwrap());
        }
        "privileges" => {
            let request: PrivilegesRequest = serde_json::from_value(request).unwrap();
            let targets = request
                .targets
                .iter()
                .map(|t| AccessAnswer::new(t.id.clone(), Access::ReadOnly))
                .collect();
            out(serde_json::to_vec(&PrivilegesResponse::new(
                Some("health_ro".to_owned()),
                targets,
            ))
            .unwrap());
        }
        other => panic!("no request `{other}`"),
    }
}

/// The plugin's declaration, with its warehouse's settings and probes through it.
fn ods_toml() -> String {
    let me = std::env::current_exe().unwrap().display().to_string();
    format!(
        "[plugins.acme]\ncommand = ['{me}', '{FIXTURE}']\n\n\
         [providers.warehouse]\nkind = \"acme_db\"\nsettings = {{ account = \"acme\" }}\n\n\
         [health.probes]\nplugin = \"acme\"\n\n\
         [[health.checks]]\nid = \"acme.rows\"\nkind = \"probe\"\n\
         sql = \"select count(*) as n from {{relation}}\"\npass = \"n > 0\"\n\
         select = {{ name = [\"orders\"] }}\nseverity = \"error\"\n"
    )
}

/// A project whose manifest names the warehouse `acme_db`, and a home whose
/// configuration directory holds the trust store.
struct Project {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let project = Self {
            dir: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10");
        let target = project.target();
        std::fs::create_dir_all(&target).unwrap();
        for file in ["manifest.json", "catalog.json", "run_results.json"] {
            let text = std::fs::read_to_string(fixture.join(file)).unwrap();
            let text = text.replace(
                "\"adapter_type\": \"duckdb\"",
                "\"adapter_type\": \"acme_db\"",
            );
            std::fs::write(target.join(file), text).unwrap();
        }
        std::fs::write(project.dir.path().join("ods.toml"), ods_toml()).unwrap();
        project
    }

    fn target(&self) -> PathBuf {
        self.dir.path().join("target")
    }

    fn state_db(&self) -> PathBuf {
        self.dir.path().join("state.db")
    }

    /// The requests the plugin was sent, and forgets them.
    fn ran(&self) -> Vec<String> {
        let path = self.dir.path().join("ran");
        let ran = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(path);
        ran.lines().map(str::to_owned).collect()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ods"));
        command
            .args(args)
            .current_dir(self.dir.path())
            .env_clear()
            .env("XDG_CONFIG_HOME", self.home.path())
            .env("APPDATA", self.home.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .envs(std::env::var_os("SystemRoot").map(|root| ("SystemRoot", root)));
        command
    }

    /// `ods` with `args`, `--json`: exit code and envelope.
    fn ods(&self, args: &[&str]) -> (i32, Value) {
        let out = self.command(args).arg("--json").output().unwrap();
        let json: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{e}: {}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().unwrap(), json)
    }
}

fn health_check_asks_for_links_and_probes_over_the_plugins_connection() {
    let project = Project::new();
    let target = project.target();
    let db = project.state_db();
    let (code, envelope) = project.ods(&[
        "health",
        "check",
        "--allow-scripts",
        "--target-dir",
        target.to_str().unwrap(),
        "--state-db",
        db.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{envelope:#}");
    let ran = project.ran();
    for request in ["describe", "link", "privileges", "probe"] {
        assert!(ran.iter().any(|r| r == request), "{request} in {ran:?}");
    }
    // The probe ran through the plugin, whose login is read-only, and passed.
    let orders = envelope["result"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "model.jaffle_ods.orders")
        .unwrap_or_else(|| panic!("{envelope:#}"))
        .clone();
    let finding = orders["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["check"] == "acme.rows")
        .unwrap_or_else(|| panic!("{orders:#}"))
        .clone();
    assert_eq!(finding["status"], "pass", "{finding:#}");
    // The links are kept beside the state store, for the dashboard.
    let dir = PathBuf::from(format!("{}.plugins", db.display()));
    let record: Value = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(Result::ok)
        .find(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| serde_json::from_slice(&std::fs::read(e.path()).unwrap()).unwrap())
        .unwrap();
    assert_eq!(record["plugin"], "acme");
    assert_eq!(record["warehouse"], "acme_db");
    assert_eq!(
        record["links"]["\"jaffle_ods\".\"main\".\"orders\""]["link"]["url"],
        "https://acme.example/explore/jaffle_ods/main/orders",
        "{record:#}"
    );
}

/// GET `path` relative to `url`: (status, body).
fn get(url: &str, path: &str) -> (u16, String) {
    let rest = url.strip_prefix("http://").unwrap();
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

fn the_dashboard_shows_recorded_links_and_never_starts_the_plugin() {
    let project = Project::new();
    let target = project.target();
    let db = project.state_db();
    let (code, envelope) = project.ods(&[
        "health",
        "check",
        "--allow-scripts",
        "--target-dir",
        target.to_str().unwrap(),
        "--state-db",
        db.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{envelope:#}");
    project.ran();
    let mut child = project
        .command(&[
            "serve",
            "--target-dir",
            target.to_str().unwrap(),
            "--state-db",
            db.to_str().unwrap(),
            "--port",
            "0",
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let announced: Value = serde_json::Deserializer::from_reader(stdout)
        .into_iter()
        .next()
        .unwrap()
        .unwrap();
    let url = announced["result"]["url"]
        .as_str()
        .unwrap_or_else(|| panic!("{announced}"))
        .to_owned();
    let (status, body) = get(&url, "api/graph");
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(status, 200, "{body}");
    let graph: Value = serde_json::from_str(&body).unwrap();
    let orders = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "model.jaffle_ods.orders")
        .unwrap_or_else(|| panic!("{graph:#}"))
        .clone();
    assert_eq!(
        orders["relation_url"], "https://acme.example/explore/jaffle_ods/main/orders",
        "{orders:#}"
    );
    assert_eq!(orders["relation_url_label"], "Open in Acme");
    assert_eq!(
        project.ran(),
        Vec::<String>::new(),
        "the dashboard started the plugin"
    );
}

fn plugin_test_runs_the_warehouse_suites() {
    let project = Project::new();
    let samples = project.dir.path().join("samples.json");
    std::fs::write(
        &samples,
        r#"[{"message": "Acme Error: no such table", "expected": "missing_relation"},
            {"message": "Acme Error: something else entirely", "expected": null}]"#,
    )
    .unwrap();
    let export = project.dir.path().join("lineage.txt");
    std::fs::write(&export, "shop.raw.orders.id -> shop.main.orders.order_id\n").unwrap();
    let malformed = project.dir.path().join("malformed.txt");
    std::fs::write(&malformed, "not an export\n").unwrap();
    let (code, envelope) = project.ods(&[
        "plugin",
        "test",
        "acme",
        "--allow-scripts",
        "--errors",
        samples.to_str().unwrap(),
        "--export",
        export.to_str().unwrap(),
        "--malformed",
        malformed.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{envelope:#}");
    let suites = envelope["result"]["suites"].as_array().unwrap();
    let suite = |contract: &str| {
        suites
            .iter()
            .find(|s| s["contract"] == contract)
            .unwrap_or_else(|| panic!("no {contract}: {envelope:#}"))
            .clone()
    };
    for contract in [
        "relation_linker",
        "error_catalogue",
        "observed_lineage_source",
    ] {
        let suite = suite(contract);
        assert!(suite.get("failed").is_none(), "{suite:#}");
        assert!(!suite["passed"].as_array().unwrap().is_empty(), "{suite:#}");
    }
    // Probes need a live warehouse: `ods health check` runs them.
    assert!(
        suite("relation_probe")["skipped"][0]["why"]
            .as_str()
            .unwrap()
            .contains("ods health check")
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(FIXTURE) {
        fixture();
        return;
    }
    // The test harness's own flags (e.g. `--quiet`, a name filter) are ignored: every
    // test runs.
    macro_rules! tests {
        ($($name:ident),* $(,)?) => {
            $(
                $name();
                println!("test {} ... ok", stringify!($name));
            )*
        };
    }
    tests!(
        health_check_asks_for_links_and_probes_over_the_plugins_connection,
        the_dashboard_shows_recorded_links_and_never_starts_the_plugin,
        plugin_test_runs_the_warehouse_suites,
    );
}
