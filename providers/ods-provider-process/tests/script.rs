//! [`ScriptCheck`] against a real process: this test binary itself, which, started
//! with `--ods-script-fixture <mode>`, plays a script that behaves (or misbehaves) as
//! `mode` says. So the tests need neither a shell nor an interpreter on any platform.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::time::{Duration, Instant};

use ods_provider_process::ScriptCheck;
use ods_sdk::contracts::health_check::{CheckFinding, CheckScope, HealthCheck, NodeFacts, Status};
use ods_sdk::protocol::{CheckRequest, CheckResponse, ProtocolVersion};

const FIXTURE: &str = "--ods-script-fixture";

/// Plays a script, as `mode` says.
fn fixture(mode: &str) {
    let mut input = Vec::new();
    if mode != "no-read" && mode != "sleep" {
        std::io::stdin().read_to_end(&mut input).unwrap();
    }
    let answer = |findings: Vec<CheckFinding>| {
        let json = serde_json::to_vec(&CheckResponse::new(findings)).unwrap();
        std::io::stdout().write_all(&json).unwrap();
    };
    match mode {
        "pass" | "fail-first" => {
            let request: CheckRequest = serde_json::from_slice(&input).unwrap();
            let findings = request
                .scope
                .nodes
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let status = if mode == "fail-first" && i == 0 {
                        Status::Fail
                    } else {
                        Status::Pass
                    };
                    CheckFinding::new(n.id.clone(), status, "looked at it")
                        .with_evidence("check", request.check.id.clone())
                        .with_evidence(
                            "params",
                            serde_json::to_string(&request.check.params).unwrap(),
                        )
                        .with_evidence(
                            "manifest",
                            request
                                .artifacts
                                .get("manifest")
                                .cloned()
                                .unwrap_or_default(),
                        )
                        .with_evidence(
                            "cwd",
                            std::env::current_dir().unwrap().display().to_string(),
                        )
                        .with_evidence(
                            "protocol",
                            format!("{}.{}", request.protocol.major, request.protocol.minor),
                        )
                })
                .collect();
            answer(findings);
        }
        "no-read" => answer(Vec::new()),
        "exit" => {
            eprintln!("starting");
            eprintln!("boom: no catalog.json");
            std::process::exit(3);
        }
        "garbage" => print!("all good!"),
        "major" => print!(r#"{{"protocol": {{"major": 2, "minor": 0}}, "findings": []}}"#),
        "sleep" => std::thread::sleep(Duration::from_secs(30)),
        other => panic!("no fixture mode `{other}`"),
    }
}

fn node(id: &str) -> NodeFacts {
    NodeFacts::new(id, id.rsplit('.').next().unwrap_or(id), "model")
}

fn scope(ids: &[&str]) -> CheckScope {
    CheckScope::new(ids.iter().map(|id| node(id)).collect(), None)
}

fn script(mode: &str, timeout: Duration) -> ScriptCheck {
    let me = std::env::current_exe().unwrap().display().to_string();
    ScriptCheck::new(
        "acme.pii",
        vec![me, FIXTURE.to_owned(), mode.to_owned()],
        std::env::temp_dir(),
        timeout,
    )
}

const LONG: Duration = Duration::from_secs(30);

async fn answers_each_node_with_the_request_it_was_given() {
    let check = script("fail-first", LONG)
        .with_params(BTreeMap::from([("min".to_owned(), 2.into())]))
        .with_artifacts(BTreeMap::from([(
            "manifest".to_owned(),
            "target/manifest.json".to_owned(),
        )]));
    let findings = check
        .check(&scope(&["model.shop.orders", "model.shop.customers"]))
        .await
        .unwrap();
    let statuses: Vec<(&str, Status)> = findings
        .iter()
        .map(|f| (f.node.as_str(), f.status))
        .collect();
    assert_eq!(
        statuses,
        [
            ("model.shop.orders", Status::Fail),
            ("model.shop.customers", Status::Pass)
        ]
    );
    let evidence = &findings[0].evidence;
    assert_eq!(evidence["check"], "acme.pii");
    assert_eq!(evidence["params"], r#"{"min":2}"#);
    assert_eq!(evidence["manifest"], "target/manifest.json");
    assert_eq!(evidence["protocol"], "1.0");
    // It runs in the project's root.
    assert_eq!(
        std::fs::canonicalize(&evidence["cwd"]).unwrap(),
        std::fs::canonicalize(std::env::temp_dir()).unwrap()
    );
    assert_eq!(check.describe().id, "acme.pii");
}

async fn a_large_request_is_written_while_the_answer_is_read() {
    // Far more than a pipe holds either way: neither side may wait on the other.
    let ids: Vec<String> = (0..5000)
        .map(|i| format!("model.shop.a_rather_long_model_name_{i:05}"))
        .collect();
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    let findings = script("pass", LONG).check(&scope(&ids)).await.unwrap();
    assert_eq!(findings.len(), 5000);
}

async fn a_script_that_answers_without_reading_its_request_is_no_error() {
    let findings = script("no-read", LONG)
        .check(&scope(&["model.shop.orders"]))
        .await
        .unwrap();
    assert_eq!(findings, []);
}

async fn a_failing_script_says_how_with_its_last_line_on_stderr() {
    let error = script("exit", LONG)
        .check(&scope(&["model.shop.orders"]))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("exited with"), "{error}");
    assert!(
        error.contains("its last line on stderr: boom: no catalog.json"),
        "{error}"
    );
}

async fn output_that_isnt_the_protocol_is_an_error() {
    let error = script("garbage", LONG)
        .check(&scope(&["model.shop.orders"]))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("didn't answer with the script protocol's JSON"),
        "{error}"
    );
}

async fn an_answer_in_another_major_is_an_error() {
    let error = script("major", LONG)
        .check(&scope(&["model.shop.orders"]))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("answered in protocol 2.0"), "{error}");
    assert_eq!(ods_sdk::protocol::PROTOCOL, ProtocolVersion::new(1, 0));
}

async fn a_script_that_takes_too_long_is_stopped() {
    let started = Instant::now();
    let error = script("sleep", Duration::from_millis(500))
        .check(&scope(&["model.shop.orders"]))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("didn't answer within 0.5s"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "it was stopped"
    );
}

async fn a_command_that_cant_start_is_an_error() {
    let check = ScriptCheck::new(
        "acme.pii",
        vec!["ods-no-such-program-anywhere".to_owned()],
        std::env::temp_dir(),
        LONG,
    );
    let error = check
        .check(&scope(&["model.shop.orders"]))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("couldn't be started"), "{error}");
    let empty = ScriptCheck::new("acme.pii", Vec::new(), std::env::temp_dir(), LONG);
    let error = empty
        .check(&scope(&["model.shop.orders"]))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no command"), "{error}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(FIXTURE) {
        fixture(&args[2]);
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // The test harness's own flags (e.g. `--quiet`, a name filter) are ignored: every
    // test runs.
    macro_rules! tests {
        ($($name:ident),* $(,)?) => {
            $(
                runtime.block_on($name());
                println!("test {} ... ok", stringify!($name));
            )*
        };
    }
    tests!(
        answers_each_node_with_the_request_it_was_given,
        a_large_request_is_written_while_the_answer_is_read,
        a_script_that_answers_without_reading_its_request_is_no_error,
        a_failing_script_says_how_with_its_last_line_on_stderr,
        output_that_isnt_the_protocol_is_an_error,
        an_answer_in_another_major_is_an_error,
        a_script_that_takes_too_long_is_stopped,
        a_command_that_cant_start_is_an_error,
    );
}
