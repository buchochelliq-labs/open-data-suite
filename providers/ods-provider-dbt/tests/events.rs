//! dbt's structured log, as dbt 1.10 printed it for a real `dbt build` with `DuckDB`
//! (`fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-events`), turned into run events (#322).
//! `customers` fails on a literal (`'sk_live_SENTINEL_42'`), which skips what reads it,
//! and the build ran with `--vars '{secret_var: VARS_SENTINEL_42}'`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ods_core::state::Timestamp;
use ods_provider_dbt::events::{Bridge, coverage};
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
    let mut bridge = Bridge::new(&sink, &request, coverage(&manifest), false);
    let log = std::fs::read_to_string(fixture("dbt-1.10-events/dbt-stdout.jsonl")).unwrap();
    let shown: Vec<String> = log.lines().filter_map(|l| bridge.line(l)).collect();
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
    assert_eq!(error.kind.as_deref(), Some("Conversion Error"));
    assert_eq!(
        error.message,
        "Conversion Error: Could not convert string [value removed] to INT32"
    );
    assert_eq!(failed.tests.unwrap().skipped, 3);

    // What it stopped never ran: no duration, not a zero one.
    let skipped = stats("segment_summary");
    assert_eq!(skipped.status, NodeRunStatus::Skipped);
    assert_eq!(skipped.took_ms(), None);
    assert_eq!(skipped.rows_affected, None);

    assert_eq!(run.totals.count(NodeRunStatus::Success), 9);
    assert_eq!(run.totals.rows_unreported, 6, "the views and tables");
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
    let mut bridge = Bridge::new(&sink, &request, BTreeMap::default(), false);
    // dbt logged as text: nothing to read.
    assert_eq!(
        bridge
            .line("22:00:12  1 of 25 START seed file main.raw_customers")
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
    assert!(!error.message.contains("SENTINEL"), "{error:?}");
}

#[test]
fn a_run_that_fails_to_finish_ends_unknown() {
    let run = RunResults::read(&fixture("dbt-1.10-events/run_results.json")).unwrap();
    let (request, _) = request_and_report(&run);
    let sink = CollectedEvents::new();
    let mut bridge = Bridge::new(&sink, &request, BTreeMap::default(), false);
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
