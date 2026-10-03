//! dbt's structured log, as dbt 1.10 printed it for a real `dbt build` with `DuckDB`
//! (`fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-events`), turned into run events (#322).
//! `customers` fails on a literal (`'sk_live_SENTINEL_42'`), which skips what reads it,
//! and the build ran with `--vars '{secret_var: VARS_SENTINEL_42}'`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ods_core::state::Timestamp;
use ods_provider_dbt::events::{Bridge, LogLevel, UNREADABLE_LINE, coverage};
use ods_provider_dbt::{Manifest, RunResults, RunStatus};
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus, NodeExecution, RequestedNode,
};
use ods_sdk::contracts::run_events::{
    CollectedEvents, NodeRunStatus, RunEventKind, RunOutcome, RunSummary,
};

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dbt/jaffle-ods/artifacts")
        .join(path)
}

fn is_check(id: &str) -> bool {
    id.starts_with("test.") || id.starts_with("unit_test.")
}

/// The request and the report the executor would make of these results.
fn request_and_report(run: &RunResults) -> (ExecutionRequest, ExecutionReport) {
    let nodes: Vec<RequestedNode> = run
        .results
        .iter()
        .filter(|r| !is_check(&r.unique_id))
        .map(|r| RequestedNode::new(r.unique_id.clone(), r.unique_id.rsplit('.').next().unwrap()))
        .collect();
    let request = ExecutionRequest::new(nodes, ExecutionMode::Build).with_scope("jaffle_ods/dev");
    let outcomes = run
        .results
        .iter()
        .filter(|r| !is_check(&r.unique_id))
        .map(|r| {
            let status = match r.status {
                RunStatus::Success => ExecutionStatus::Success,
                RunStatus::Skipped => ExecutionStatus::Skipped,
                _ => ExecutionStatus::Failed,
            };
            NodeExecution::new(
                r.unique_id.clone(),
                status,
                None,
                Some(r.raw_status.clone()),
            )
        })
        .collect();
    let report = ExecutionReport::new(
        run.invocation_id.clone().unwrap(),
        run.started_at
            .as_deref()
            .and_then(|t| Timestamp::parse(t).ok()),
        Timestamp::parse(run.generated_at.as_deref().unwrap()).unwrap(),
        outcomes,
        Vec::new(),
    )
    .failed();
    (request, report)
}

#[test]
fn a_real_dbt_build_log_becomes_run_events_with_stats() {
    let run = RunResults::read(&fixture("dbt-1.10-events/run_results.json")).unwrap();
    let manifest = Manifest::read(&fixture("dbt-1.10-build/manifest.json")).unwrap();
    let (request, report) = request_and_report(&run);
    let sink = CollectedEvents::new();
    let mut bridge = Bridge::new(&sink, &request, coverage(&manifest), LogLevel::Info);
    let log = std::fs::read_to_string(fixture("dbt-1.10-events/dbt-stdout.jsonl")).unwrap();
    let shown: Vec<String> = log
        .lines()
        .filter_map(|l| bridge.line(l))
        .map(|l| l.text)
        .collect();
    let live = sink.events().len();
    bridge.finish(&report, &run);
    let events = sink.events();

    // Every node finished from the log itself: `finish` only ends the run.
    assert_eq!(events.len(), live + 1);
    assert!(events.iter().all(|e| e.run_id == report.run_id));
    assert!(
        events
            .iter()
            .all(|e| e.scope.as_deref() == Some("jaffle_ods/dev"))
    );
    assert!(events.windows(2).all(|w| w[0].at <= w[1].at));

    let run = RunSummary::from_events(&events);
    assert!(run.live);
    assert_eq!(run.outcome, Some(RunOutcome::Failed));
    let stats = |name: &str| {
        &run.nodes
            .iter()
            .find(|n| n.node.ends_with(&format!(".{name}")))
            .unwrap_or_else(|| panic!("{name}"))
            .stats
    };

    // A seed: the adapter reported rows, and more.
    let seed = stats("raw_orders");
    assert_eq!(seed.status, NodeRunStatus::Success);
    assert_eq!(seed.rows_affected, Some(6));
    assert_eq!(seed.adapter.get("code").map(String::as_str), Some("INSERT"));
    assert!(!seed.adapter.contains_key("_message"));
    assert!(seed.compile_ms.is_some() && seed.execute_ms.is_some());
    assert!(seed.duration_ms.is_some());
    assert!(seed.started_at.is_some() && seed.finished_at.is_some());
    assert!(
        seed.thread
            .as_deref()
            .is_some_and(|t| t.starts_with("Thread-"))
    );

    // A view and a table: DuckDB reports no rows for them, so none are made up.
    for name in ["stg_orders", "orders"] {
        let s = stats(name);
        assert_eq!(s.status, NodeRunStatus::Success, "{name}");
        assert_eq!(s.rows_affected, None, "{name}");
        assert!(s.adapter.is_empty(), "{name}: {:?}", s.adapter);
    }
    // Its own two tests, and the relationship test from `customer_order_rank`, passed;
    // the one from `customers` didn't run.
    let tests = stats("orders").tests.unwrap();
    assert_eq!((tests.passed, tests.skipped), (3, 1));

    // The failure: its kind and first line, without the literal.
    let failed = stats("customers");
    assert_eq!(failed.status, NodeRunStatus::Error);
    let error = failed.error.as_ref().unwrap();
    assert_eq!(error.kind(), Some("Conversion Error"));
    assert_eq!(
        error.message(),
        "Conversion Error: Could not convert string [value removed] to INT32"
    );
    assert_eq!(failed.tests.unwrap().skipped, 3);

    // What it stopped never ran: no duration, not a zero one.
    let skipped = stats("segment_summary");
    assert_eq!(skipped.status, NodeRunStatus::Skipped);
    assert_eq!(skipped.took_ms(), None);
    assert_eq!(skipped.rows_affected, None);

    assert_eq!(run.totals.count(NodeRunStatus::Success), 9);
    assert_eq!(
        run.totals.rows_unreported, 7,
        "the views and tables, and the failed model"
    );
    assert_eq!(run.totals.count(NodeRunStatus::Error), 1);
    assert_eq!(run.totals.count(NodeRunStatus::Skipped), 3);
    assert_eq!(run.totals.rows_affected, 17);
    assert!(run.totals.rows_is_lower_bound());

    // No SQL, literal or variable reaches the events. The SQL and the arguments dbt
    // logs at debug level aren't shown either; its own error line is, as dbt shows it.
    let json = serde_json::to_string(&events).unwrap();
    assert!(!json.contains("SENTINEL_42"), "{json}");
    assert!(!json.contains("select"), "{json}");
    let shown = shown.join("\n");
    assert!(!shown.contains("VARS_SENTINEL_42"), "{shown}");
    assert!(
        !shown.contains("On model.jaffle_ods.customers: /*"),
        "{shown}"
    );
    assert!(
        shown.contains("START sql view model main.stg_orders"),
        "{shown}"
    );
    assert!(
        events
            .iter()
            .filter(|e| matches!(e.kind, RunEventKind::NodeStarted { .. }))
            .count()
            >= 8
    );
}

#[test]
fn without_a_structured_log_the_results_still_give_the_stats() {
    let run = RunResults::read(&fixture("dbt-1.10-events/run_results.json")).unwrap();
    let (request, report) = request_and_report(&run);
    let sink = CollectedEvents::new();
    let mut bridge = Bridge::new(&sink, &request, BTreeMap::default(), LogLevel::Info);
    // dbt logged as text: nothing to read.
    assert_eq!(
        bridge
            .line("22:00:12  1 of 25 START seed file main.raw_customers")
            .map(|l| l.text)
            .as_deref(),
        Some("22:00:12  1 of 25 START seed file main.raw_customers")
    );
    bridge.finish(&report, &run);
    let run_summary = RunSummary::from_events(&sink.events());
    assert!(!run_summary.live);
    let seed = &run_summary
        .get("seed.jaffle_ods.raw_customers")
        .unwrap()
        .stats;
    assert_eq!(seed.rows_affected, Some(4));
    let error = run_summary
        .get("model.jaffle_ods.customers")
        .unwrap()
        .stats
        .error
        .as_ref()
        .unwrap();
    assert!(!error.message().contains("SENTINEL"), "{error:?}");
}

#[test]
fn a_run_that_fails_to_finish_ends_unknown() {
    let run = RunResults::read(&fixture("dbt-1.10-events/run_results.json")).unwrap();
    let (request, _) = request_and_report(&run);
    let sink = CollectedEvents::new();
    let mut bridge = Bridge::new(&sink, &request, BTreeMap::default(), LogLevel::Info);
    let log = std::fs::read_to_string(fixture("dbt-1.10-events/dbt-stdout.jsonl")).unwrap();
    for line in log.lines().take(20) {
        bridge.line(line);
    }
    bridge.abort();
    let summary = RunSummary::from_events(&sink.events());
    assert_eq!(summary.outcome, Some(RunOutcome::Unknown));
    assert!(
        summary
            .nodes
            .iter()
            .all(|n| n.stats.status != NodeRunStatus::Success || n.stats.finished_at.is_some())
    );
    assert!(
        summary
            .nodes
            .iter()
            .any(|n| n.stats.status == NodeRunStatus::Unknown)
    );
}

/// The reviewers' probes (#322): what dbt prints is shown only when it can be read for
/// sure; a line that can't be read never shows its text, and never reaches the events.
#[test]
fn unreadable_or_unlevelled_lines_are_never_shown() {
    let request = ExecutionRequest::new(
        vec![RequestedNode::new("model.p.a", "a")],
        ExecutionMode::Build,
    );
    let sink = CollectedEvents::new();
    let mut bridge = Bridge::new(&sink, &request, BTreeMap::default(), LogLevel::Info);
    let cases = [
        // A number out of range: not readable JSON.
        r#"{"info":{"level":"debug","name":"SQLQuery","msg":"select 'SECRET1'","invocation_id":"x"},"data":{"n":1e400}}"#,
        // Cut short.
        r#"{"info":{"level":"debug","name":"SQLQuery","msg":"select 'SECRET2'"},"data":{"sql":"select 'SECRET2'"}"#,
        // No level.
        r#"{"info":{"name":"SQLQuery","msg":"select 'SECRET3'"}}"#,
        // A level in capitals is still debug.
        r#"{"info":{"level":"DEBUG","name":"SQLQuery","msg":"select 'SECRET4'"}}"#,
        // A model's print merged with a debug line.
        r#"printed by a model {"info":{"level":"debug","name":"SQLQuery","msg":"select 'SECRET5'"}}"#,
        // No `info`.
        r#"{"data":{"msg":"select 'SECRET6'"}}"#,
    ];
    for line in cases {
        let shown = bridge.line(line);
        assert!(
            shown.as_ref().is_none_or(|l| l.text == UNREADABLE_LINE),
            "{line} => {shown:?}"
        );
    }
    // A plain line (a model's print) is shown; an info line shows its message.
    assert_eq!(
        bridge.line("hello from a model").map(|l| l.text).as_deref(),
        Some("hello from a model")
    );
    let info = bridge
        .line(r#"{"info":{"level":"INFO","name":"Note","msg":"1 of 1 START","ts":"2026-09-29T22:00:12.1Z"}}"#)
        .unwrap();
    assert_eq!(
        (info.time.as_deref(), info.text.as_str()),
        (Some("22:00:12"), "1 of 1 START")
    );
    // A finish without a status is unknown, never an error or a success.
    bridge.line(r#"{"info":{"level":"debug","name":"NodeStart","msg":"m","invocation_id":"x"},"data":{"node_info":{"unique_id":"model.p.a"}}}"#);
    bridge.line(r#"{"info":{"level":"debug","name":"NodeFinished","msg":"m","invocation_id":"x"},"data":{"node_info":{"unique_id":"model.p.a"},"run_result":{"thread":"t"}}}"#);
    let events = sink.events();
    let json = serde_json::to_string(&events).unwrap();
    assert!(!json.contains("SECRET"), "{json}");
    let finished = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::NodeFinished { stats, .. } => Some(stats.status),
            _ => None,
        })
        .unwrap();
    assert_eq!(finished, NodeRunStatus::Unknown);
}

/// The report's status wins over the log's (ADR-0024): a node the log finished as
/// unknown finishes again, as the report says.
#[test]
fn the_reports_status_wins_over_the_logs() {
    let run = RunResults::read(&fixture("dbt-1.10-events/run_results.json")).unwrap();
    let (request, report) = request_and_report(&run);
    let sink = CollectedEvents::new();
    let mut bridge = Bridge::new(&sink, &request, BTreeMap::default(), LogLevel::Info);
    let id = &request.nodes[0].id;
    bridge.line(&format!(r#"{{"info":{{"level":"debug","name":"NodeFinished","msg":"m","invocation_id":"{}"}},"data":{{"node_info":{{"unique_id":"{id}"}},"run_result":{{"status":"weird"}}}}}}"#, report.run_id));
    bridge.finish(&report, &run);
    let summary = RunSummary::from_events(&sink.events());
    assert_eq!(
        summary.get(id).unwrap().stats.status,
        NodeRunStatus::Success
    );
}

/// Codex review on #327: a log with structured lines but no node event (dbt dropped
/// them, e.g. at a higher log level) doesn't make the run `live`: its stats come from
/// the results.
#[test]
fn a_log_without_node_events_is_not_live() {
    let run = RunResults::read(&fixture("dbt-1.10-events/run_results.json")).unwrap();
    let (request, report) = request_and_report(&run);
    let sink = CollectedEvents::new();
    let mut bridge = Bridge::new(&sink, &request, BTreeMap::default(), LogLevel::Info);
    let shown = bridge.line(&format!(
        r#"{{"info":{{"level":"info","name":"Note","msg":"Running with dbt","invocation_id":"{}","ts":"2026-09-29T22:00:12Z"}}}}"#,
        report.run_id
    ));
    assert!(shown.is_some());
    assert!(
        sink.events().is_empty(),
        "nothing starts before a node event"
    );
    bridge.finish(&report, &run);
    let summary = RunSummary::from_events(&sink.events());
    assert!(!summary.live);
    assert_eq!(summary.run_id.as_deref(), Some(report.run_id.as_str()));
}

/// #323: real dbt 1.10, 1.11 and 1.12 + `DuckDB` logs of two broken builds
/// (`fixtures/dbt/jaffle-ods/capture-errors.sh`): the failed node's summary keeps the
/// line `DuckDB` reported and is recognised; a compile that stopped the project is found
/// in what dbt printed.
#[test]
fn real_failures_are_summarised_and_recognised() {
    use ods_core::failure::Symptom;
    use ods_provider_dbt::error_catalogue::DbtErrorCatalogue;
    use ods_provider_dbt::events::project_failure;
    use ods_sdk::contracts::error_catalogue::{Classification, ErrorCatalogue};

    for version in ["1.10", "1.11", "1.12"] {
        let log = std::fs::read_to_string(fixture(&format!(
            "dbt-{version}-errors/missing-column.jsonl"
        )))
        .unwrap();
        let nodes: Vec<RequestedNode> = log
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|l| l["info"]["name"] == "NodeFinished")
            .filter_map(|l| {
                l["data"]["node_info"]["unique_id"]
                    .as_str()
                    .map(str::to_owned)
            })
            .filter(|id| !is_check(id))
            .map(|id| RequestedNode::new(id.clone(), id))
            .collect();
        let request = ExecutionRequest::new(nodes, ExecutionMode::Build);
        let sink = CollectedEvents::new();
        let mut bridge = Bridge::new(&sink, &request, BTreeMap::new(), LogLevel::Info);
        for line in log.lines() {
            bridge.line(line);
        }
        let run = RunSummary::from_events(&sink.events());
        let customers = run.get("model.jaffle_ods.customers").unwrap();
        assert_eq!(customers.stats.status, NodeRunStatus::Error);
        let error = customers.stats.error.as_ref().unwrap();
        assert_eq!(error.kind(), Some("Binder Error"));
        // dbt 1.11 and 1.12 put the failing line one earlier than 1.10 here.
        let line = if version == "1.10" { 25 } else { 24 };
        assert_eq!(error.line(), Some(line), "{version}");
        assert!(!error.message().contains("first_name"), "{error:?}");
        assert!(matches!(
            DbtErrorCatalogue.classify(error),
            Classification::Recognised(m) if m.symptom == Symptom::MissingColumn
        ));

        // `dbt compile` stopped at the undefined macro: what people were shown holds it.
        let log = std::fs::read_to_string(fixture(&format!(
            "dbt-{version}-errors/unknown-macro.jsonl"
        )))
        .unwrap();
        let request = ExecutionRequest::new(Vec::new(), ExecutionMode::Build);
        let mut bridge = Bridge::new(&sink, &request, BTreeMap::new(), LogLevel::Info);
        let shown: Vec<String> = log
            .lines()
            .filter_map(|l| bridge.line(l))
            .map(|l| l.text)
            .collect();
        let failure = project_failure(&shown.join("\n")).unwrap();
        assert_eq!(failure.node_name.as_deref(), Some("stg_payments"));
        assert!(matches!(
            DbtErrorCatalogue.classify(&failure.summary),
            Classification::Recognised(m) if m.symptom == Symptom::UnknownMacro
        ));
    }
}

/// Each dbt version whose errors are recorded (`capture-errors.sh`).
const ERROR_VERSIONS: [&str; 3] = ["1.10", "1.11", "1.12"];

/// #323: a real dbt build whose `accepted_values` test fails (`capture-errors.sh`,
/// accepted-values), with each recorded version: the check finishes failed, with the
/// rows dbt counted (`num_failures` in the log, `failures` in `run_results.json`) and
/// its message redacted, which the catalogue recognises as a failed test. The values
/// the test accepts (one a secret) are in dbt's id for the test, which the journal
/// keeps as the event's `check` (as before #323); they are in nothing this adds: the
/// count, the message, or the run's summary as it is serialized.
#[test]
fn a_failing_test_reports_its_failing_rows_and_message() {
    use ods_core::failure::Symptom;
    use ods_provider_dbt::error_catalogue::DbtErrorCatalogue;
    use ods_sdk::contracts::error_catalogue::{Classification, ErrorCatalogue};
    use ods_sdk::contracts::run_events::{CheckStatus, CheckSummary, ErrorSummary, RunEvent};

    const SENTINEL: &str = "sk_live_SENTINEL_42";
    for version in ERROR_VERSIONS {
        let dir = format!("dbt-{version}-errors");
        let run =
            RunResults::read(&fixture(&format!("{dir}/accepted-values-run_results.json"))).unwrap();
        let failing = run.results.iter().find(|r| r.raw_status == "fail").unwrap();
        assert_eq!(failing.details.failures, Some(3), "{version}");
        let test = failing.unique_id.clone();
        assert!(test.contains(SENTINEL), "{version}: {test}");
        let (request, report) = request_and_report(&run);
        let manifest = Manifest::read(&fixture("dbt-1.10-build/manifest.json")).unwrap();

        let check_of = |events: &[RunEvent]| -> CheckSummary {
            let summary = RunSummary::from_events(events);
            // The summary as it is shown (e.g. `ods state history --run`), whole.
            let shown = serde_json::to_string(&summary).unwrap();
            assert!(!shown.contains(SENTINEL), "{version}: {shown}");
            let check = summary.check(&test).cloned().unwrap();
            let said = format!("{:?} {:?}", check.failures, check.error);
            assert!(!said.contains(SENTINEL), "{version}: {said}");
            assert!(!said.to_lowercase().contains("select"), "{version}: {said}");
            check
        };
        let expect = |error: Option<&ErrorSummary>| {
            let error = error.unwrap();
            assert_eq!(
                error.message(),
                "Got [value removed] results, configured to fail if != [value removed]",
                "{version}"
            );
            assert!(matches!(
                DbtErrorCatalogue.classify(error),
                Classification::Recognised(m) if m.symptom == Symptom::TestFailed
            ));
        };

        // Live, from dbt's log.
        let log =
            std::fs::read_to_string(fixture(&format!("{dir}/accepted-values.jsonl"))).unwrap();
        let sink = CollectedEvents::new();
        let mut bridge = Bridge::new(&sink, &request, coverage(&manifest), LogLevel::Info);
        for line in log.lines() {
            bridge.line(line);
        }
        bridge.finish(&report, &run);
        let events = sink.events();
        let check = check_of(&events);
        assert_eq!(check.status, CheckStatus::Failed, "{version}");
        assert_eq!(check.failures, Some(3), "{version}");
        expect(check.error.as_ref());
        // The tests that passed say nothing failed them.
        let summary = RunSummary::from_events(&events);
        assert!(
            summary
                .checks
                .iter()
                .filter(|c| c.status == CheckStatus::Passed)
                .all(|c| c.failures.is_none() && c.error.is_none())
        );

        // From `run_results.json` alone.
        let sink = CollectedEvents::new();
        let mut bridge = Bridge::new(&sink, &request, BTreeMap::new(), LogLevel::Info);
        bridge.finish(&report, &run);
        let check = check_of(&sink.events());
        assert_eq!(
            (check.status, check.failures),
            (CheckStatus::Failed, Some(3)),
            "{version}"
        );
        expect(check.error.as_ref());
    }
}
