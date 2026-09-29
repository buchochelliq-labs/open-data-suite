//! Conformance suite for [`Executor`].

use std::sync::Arc;

use async_trait::async_trait;

use ods_core::Capability;

use super::Report;
use crate::contracts::executor::{
    ExecutionMode, ExecutionReport, ExecutionRequest, ExecutionStatus, Executor, PrepareRequest,
    RequestedNode,
};
use crate::contracts::run_events::{
    CollectedEvents, NodeRunStatus, RunEvent, RunEventKind, RunOutcome,
};

/// What the suite needs from an executor under test.
#[async_trait]
pub trait ExecutorHarness: Send + Sync {
    /// A fresh executor over a project the harness controls. Called once per case.
    async fn executor(&self) -> Arc<dyn Executor>;

    /// At least two nodes that build successfully and don't depend on each other.
    fn buildable(&self) -> Vec<RequestedNode>;

    /// A node that fails to build, if the harness can arrange one; `None` skips the case
    /// that needs it.
    fn failing(&self) -> Option<RequestedNode>;

    /// The ids of the nodes built since the last [`executor`](Self::executor) call, or
    /// `None` if the harness can't observe builds (which skips those checks).
    async fn built(&self) -> Option<Vec<String>>;

    /// A buildable node with checks that pass, and one without checks, if the harness
    /// can arrange them; `None` skips the case that needs them.
    fn checked_and_unchecked(&self) -> Option<(RequestedNode, RequestedNode)> {
        None
    }

    /// A value that appears inside quotes, after a word with an apostrophe, in the
    /// engine's message for the [failing](Self::failing) node (e.g. the engine says
    /// `Can't cast 'SECRET' to INT`), if the harness can arrange one. The events must
    /// never contain it (#322, AGENTS.md rule 9).
    fn failing_secret(&self) -> Option<String> {
        None
    }

    /// A source with checks that pass, if the harness can arrange one; `None` skips the
    /// case that needs it (#232).
    fn checked_source(&self) -> Option<RequestedNode> {
        None
    }

    /// A source with a check that fails, and a buildable node that reads it, if the
    /// harness can arrange them; `None` skips the case that needs them (#232).
    fn failing_source(&self) -> Option<(RequestedNode, RequestedNode)> {
        None
    }
}

fn ids(nodes: &[RequestedNode]) -> Vec<String> {
    nodes.iter().map(|n| n.id.clone()).collect()
}

async fn assert_built(harness: &dyn ExecutorHarness, case: &str, expected: &[String]) {
    if let Some(mut built) = harness.built().await {
        built.sort();
        let mut expected = expected.to_vec();
        expected.sort();
        assert_eq!(built, expected, "{case}: built");
    }
}

async fn builds_exactly_the_requested_nodes(harness: &dyn ExecutorHarness) {
    let case = "builds_exactly_the_requested_nodes";
    let executor = harness.executor().await;
    let all = harness.buildable();
    assert!(
        all.len() >= 2,
        "{case}: the harness needs two buildable nodes"
    );
    let one = vec![all[0].clone()];
    let report = executor
        .execute(&ExecutionRequest::new(one.clone(), ExecutionMode::Run))
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_eq!(
        report
            .nodes
            .iter()
            .map(|n| n.node.clone())
            .collect::<Vec<_>>(),
        ids(&one),
        "{case}: reported nodes"
    );
    assert_eq!(
        report.nodes[0].status,
        ExecutionStatus::Success,
        "{case}: status"
    );
    assert!(report.succeeded, "{case}: succeeded");
    assert!(report.unrequested.is_empty(), "{case}: {report:?}");
    assert_built(harness, case, &ids(&one)).await;
}

async fn reports_every_node_once_in_request_order(harness: &dyn ExecutorHarness) {
    let case = "reports_every_node_once_in_request_order";
    let executor = harness.executor().await;
    let mut nodes = harness.buildable();
    nodes.reverse();
    let report = executor
        .execute(&ExecutionRequest::new(nodes.clone(), ExecutionMode::Build))
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_eq!(
        report
            .nodes
            .iter()
            .map(|n| n.node.clone())
            .collect::<Vec<_>>(),
        ids(&nodes),
        "{case}: reported nodes"
    );
    assert!(
        report
            .nodes
            .iter()
            .all(|n| n.status == ExecutionStatus::Success),
        "{case}: {report:?}"
    );
    assert_built(harness, case, &ids(&nodes)).await;
}

async fn refuses_an_empty_request(harness: &dyn ExecutorHarness) {
    let case = "refuses_an_empty_request";
    let executor = harness.executor().await;
    let result = executor
        .execute(&ExecutionRequest::new(Vec::new(), ExecutionMode::Build))
        .await;
    assert!(result.is_err(), "{case}: {result:?}");
    assert_built(harness, case, &[]).await;
}

async fn failures_are_reported_not_errors(harness: &dyn ExecutorHarness, failing: RequestedNode) {
    let case = "failures_are_reported_not_errors";
    let executor = harness.executor().await;
    let report = executor
        .execute(&ExecutionRequest::new(
            vec![failing.clone()],
            ExecutionMode::Run,
        ))
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_eq!(report.nodes.len(), 1, "{case}: {report:?}");
    assert_eq!(report.nodes[0].node, failing.id, "{case}: node");
    assert_eq!(
        report.nodes[0].status,
        ExecutionStatus::Failed,
        "{case}: status"
    );
    assert!(!report.succeeded, "{case}: succeeded");
}

async fn unknown_nodes_never_succeed(harness: &dyn ExecutorHarness) {
    let case = "unknown_nodes_never_succeed";
    let executor = harness.executor().await;
    let unknown = RequestedNode::new("model.suite.no_such_node", "no_such_node");
    match executor
        .execute(&ExecutionRequest::new(vec![unknown], ExecutionMode::Run))
        .await
    {
        Err(_) => {}
        Ok(report) => {
            assert!(!report.succeeded, "{case}: {report:?}");
            assert!(
                report
                    .nodes
                    .iter()
                    .all(|n| n.status != ExecutionStatus::Success),
                "{case}: {report:?}"
            );
        }
    }
    assert_built(harness, case, &[]).await;
}

async fn run_ids_are_unique(harness: &dyn ExecutorHarness) {
    let case = "run_ids_are_unique";
    let executor = harness.executor().await;
    let request = ExecutionRequest::new(vec![harness.buildable()[0].clone()], ExecutionMode::Run);
    let first = executor
        .execute(&request)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    let second = executor
        .execute(&request)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_ne!(first.run_id, second.run_id, "{case}");
}

async fn prepare_builds_nothing(harness: &dyn ExecutorHarness) {
    let case = "prepare_builds_nothing";
    let executor = harness.executor().await;
    executor
        .prepare(&PrepareRequest::new())
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_built(harness, case, &[]).await;
}

async fn a_test_run_builds_nothing(harness: &dyn ExecutorHarness) {
    let case = "a_test_run_builds_nothing";
    let executor = harness.executor().await;
    let nodes = harness.buildable();
    let report = executor
        .execute(&ExecutionRequest::new(nodes.clone(), ExecutionMode::Test))
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    let reported: Vec<String> = report.nodes.iter().map(|n| n.node.clone()).collect();
    assert_eq!(
        reported,
        ids(&nodes),
        "{case}: every node reported once, in order"
    );
    assert_built(harness, case, &[]).await;
}

/// Only checks that ran and passed vouch for a node: a test run of a node without
/// checks doesn't fully check it (#220).
async fn only_checks_that_ran_vouch_for_a_node(
    harness: &dyn ExecutorHarness,
    checked: RequestedNode,
    unchecked: RequestedNode,
) {
    let case = "only_checks_that_ran_vouch_for_a_node";
    let executor = harness.executor().await;
    let report = executor
        .execute(&ExecutionRequest::new(
            vec![checked.clone(), unchecked.clone()],
            ExecutionMode::Test,
        ))
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    let of = |id: &str| {
        report
            .nodes
            .iter()
            .find(|n| n.node == id)
            .unwrap_or_else(|| panic!("{case}: {id} not reported"))
    };
    let checked = of(&checked.id);
    assert!(
        checked.fully_checked() && !checked.checks_passed.is_empty(),
        "{case}: a node whose checks passed is fully checked and lists them: {checked:?}"
    );
    let unchecked = of(&unchecked.id);
    assert!(
        !unchecked.fully_checked() && unchecked.status != ExecutionStatus::Success,
        "{case}: a node without checks isn't tested: {unchecked:?}"
    );
    assert_built(harness, case, &[]).await;
}

/// A source's checks run when asked, and it is reported once, apart from the nodes
/// (#232). Run mode runs no checks, so sources alone are nothing to run.
async fn source_checks_are_reported_per_source(
    harness: &dyn ExecutorHarness,
    source: RequestedNode,
) {
    let case = "source_checks_are_reported_per_source";
    let executor = harness.executor().await;
    let report = executor
        .execute(
            &ExecutionRequest::new(Vec::new(), ExecutionMode::Test)
                .with_sources(vec![source.clone()]),
        )
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert!(report.nodes.is_empty(), "{case}: {report:?}");
    assert_eq!(report.sources.len(), 1, "{case}: {report:?}");
    let checked = &report.sources[0];
    assert_eq!(checked.node, source.id, "{case}: source");
    assert!(
        checked.fully_checked(),
        "{case}: a source whose checks passed is fully checked: {checked:?}"
    );
    assert!(report.succeeded, "{case}: {report:?}");
    let run_only = executor
        .execute(&ExecutionRequest::new(Vec::new(), ExecutionMode::Run).with_sources(vec![source]))
        .await;
    assert!(run_only.is_err(), "{case}: {run_only:?}");
    assert_built(harness, case, &[]).await;
}

/// A failing source check fails the execution, and in a build the requested nodes that
/// read the source aren't built, as when a parent fails (#232).
async fn a_failing_source_check_skips_its_readers(
    harness: &dyn ExecutorHarness,
    source: RequestedNode,
    reader: RequestedNode,
) {
    let case = "a_failing_source_check_skips_its_readers";
    let executor = harness.executor().await;
    let report = executor
        .execute(
            &ExecutionRequest::new(vec![reader.clone()], ExecutionMode::Build)
                .with_sources(vec![source.clone()]),
        )
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_eq!(report.sources.len(), 1, "{case}: {report:?}");
    assert_eq!(report.sources[0].node, source.id, "{case}: source");
    assert_eq!(
        report.sources[0].status,
        ExecutionStatus::Failed,
        "{case}: {report:?}"
    );
    assert!(!report.checks_failed.is_empty(), "{case}: {report:?}");
    assert_eq!(report.nodes.len(), 1, "{case}: {report:?}");
    assert_eq!(
        report.nodes[0].status,
        ExecutionStatus::Skipped,
        "{case}: the reader isn't built: {report:?}"
    );
    assert!(!report.succeeded, "{case}: {report:?}");
    assert_built(harness, case, &[]).await;
}

/// The scope the event cases pass; every event must carry it.
const SCOPE: &str = "conformance/suite";

/// Runs `request` through [`Executor::execute_with_events`] and checks the rules every
/// event stream follows (ADR-0024), whether the executor reports events live or not.
async fn events_of(
    case: &str,
    executor: &dyn Executor,
    request: &ExecutionRequest,
) -> (ExecutionReport, Vec<RunEvent>) {
    let sink = CollectedEvents::new();
    let report = executor
        .execute_with_events(request, &sink)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    let events = sink.events();
    check_events(case, executor, request, &report, &events);
    (report, events)
}

fn check_events(
    case: &str,
    executor: &dyn Executor,
    request: &ExecutionRequest,
    report: &ExecutionReport,
    events: &[RunEvent],
) {
    let live = executor
        .info()
        .capabilities
        .contains(&Capability::RunEvents);

    // Framed by one start and one finish, all about this run and scope, in time order.
    assert!(
        matches!(
            events.first().map(|e| &e.kind),
            Some(RunEventKind::RunStarted { .. })
        ),
        "{case}: the first event starts the run: {events:?}"
    );
    assert!(
        matches!(
            events.last().map(|e| &e.kind),
            Some(RunEventKind::RunFinished { .. })
        ),
        "{case}: the last event finishes the run: {events:?}"
    );
    let count = |kind: fn(&RunEventKind) -> bool| events.iter().filter(|e| kind(&e.kind)).count();
    assert_eq!(
        (
            count(|k| matches!(k, RunEventKind::RunStarted { .. })),
            count(|k| matches!(k, RunEventKind::RunFinished { .. }))
        ),
        (1, 1),
        "{case}: one start and one finish: {events:?}"
    );
    for event in events {
        assert_eq!(event.run_id, report.run_id, "{case}: run id of {event:?}");
        assert_eq!(
            event.scope.as_deref(),
            Some(SCOPE),
            "{case}: scope of {event:?}"
        );
    }
    assert!(
        events.windows(2).all(|w| w[0].at <= w[1].at),
        "{case}: times never go backwards: {events:?}"
    );
    if let RunEventKind::RunStarted {
        nodes,
        mode,
        live: said,
    } = &events[0].kind
    {
        let requested: Vec<String> = request.nodes.iter().map(|n| n.id.clone()).collect();
        assert_eq!(nodes, &requested, "{case}: run_started lists the request");
        assert_eq!(*mode, request.mode, "{case}: mode");
        // An executor with the capability may still fall back for a run, e.g. when
        // its engine was told to log in a way it can't read.
        assert!(
            live || !*said,
            "{case}: live only with the run_events capability"
        );
    }

    check_node_events(case, request, report, events);
    let outcome = match &events[events.len() - 1].kind {
        RunEventKind::RunFinished { outcome } => *outcome,
        _ => unreachable!("checked above"),
    };
    if report.succeeded {
        assert_eq!(outcome, RunOutcome::Succeeded, "{case}: outcome");
    } else {
        assert_ne!(outcome, RunOutcome::Succeeded, "{case}: outcome");
    }
}

fn check_node_events(
    case: &str,
    request: &ExecutionRequest,
    report: &ExecutionReport,
    events: &[RunEvent],
) {
    // Every requested node finishes, last as the report says (unknown when the
    // report doesn't list it); node events come in order (queued, started, finished)
    // and only for nodes the run touched.
    for requested in &request.nodes {
        let expected = report
            .nodes
            .iter()
            .find(|n| n.node == requested.id)
            .map_or(NodeRunStatus::Unknown, |n| NodeRunStatus::from(n.status));
        let node = requested;
        let of: Vec<&RunEvent> = events
            .iter()
            .filter(|e| e.node() == Some(node.id.as_str()))
            .collect();
        let finished: Vec<_> = of
            .iter()
            .filter_map(|e| match &e.kind {
                RunEventKind::NodeFinished { stats, .. } => Some(stats),
                _ => None,
            })
            .collect();
        // It finishes; it may finish again only to take the report's status, so its
        // last finish says what the report says.
        let Some(&stats) = finished.last() else {
            panic!("{case}: {} never finishes: {of:?}", node.id);
        };
        assert_eq!(
            stats.status, expected,
            "{case}: {} finishes as the report says",
            node.id
        );
        let first_finish = of
            .iter()
            .position(|e| matches!(e.kind, RunEventKind::NodeFinished { .. }))
            .unwrap_or(of.len());
        assert!(
            of[first_finish..]
                .iter()
                .all(|e| matches!(e.kind, RunEventKind::NodeFinished { .. })),
            "{case}: nothing about {} after it finished but a correction: {of:?}",
            node.id
        );
        let position = |kind: fn(&RunEventKind) -> bool| of.iter().position(|e| kind(&e.kind));
        let queued = position(|k| matches!(k, RunEventKind::NodeQueued { .. }));
        let started = position(|k| matches!(k, RunEventKind::NodeStarted { .. }));
        if let (Some(q), Some(s)) = (queued, started) {
            assert!(q < s, "{case}: {} queued before it started", node.id);
        }
        if stats.status == NodeRunStatus::Success {
            assert!(
                stats.error.is_none(),
                "{case}: a success has no error: {stats:?}"
            );
        }
        if let (Some(start), Some(end)) = (stats.started_at, stats.finished_at) {
            assert!(start <= end, "{case}: {} ends after it starts", node.id);
        }
        if let Some(error) = &stats.error {
            // Redacting a summary again changes nothing: no quoted value or SQL is left.
            assert_eq!(
                ods_core::redact::summary_line(error.message(), usize::MAX).as_deref(),
                Some(error.message()),
                "{case}: error summaries quote nothing: {error:?}"
            );
        }
    }
    for event in events {
        if let Some(node) = event.node() {
            assert!(
                request.nodes.iter().any(|n| n.id == node)
                    || report.unrequested.iter().any(|n| n == node),
                "{case}: {node} is neither requested nor reported unrequested"
            );
        }
    }
}

/// Events frame the run and every requested node finishes as the report says
/// (#322).
async fn events_follow_the_run(harness: &dyn ExecutorHarness) {
    let case = "events_follow_the_run";
    let executor = harness.executor().await;
    let nodes = harness.buildable();
    let request = ExecutionRequest::new(nodes.clone(), ExecutionMode::Build).with_scope(SCOPE);
    let (report, _) = events_of(case, executor.as_ref(), &request).await;
    assert!(report.succeeded, "{case}: {report:?}");
    assert_built(harness, case, &ids(&nodes)).await;
}

/// A failed node finishes as an error in the events, and so does the run (#322).
async fn failed_nodes_finish_as_errors(harness: &dyn ExecutorHarness, failing: RequestedNode) {
    let case = "failed_nodes_finish_as_errors";
    let executor = harness.executor().await;
    let mut nodes = vec![failing.clone()];
    nodes.push(harness.buildable()[0].clone());
    let request = ExecutionRequest::new(nodes, ExecutionMode::Run).with_scope(SCOPE);
    let (report, events) = events_of(case, executor.as_ref(), &request).await;
    assert!(!report.succeeded, "{case}: {report:?}");
    let failed = events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::NodeFinished { node, stats } if *node == failing.id => Some(stats),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{case}: {} never finished", failing.id));
    assert_eq!(failed.status, NodeRunStatus::Error, "{case}: {failed:?}");
    if let Some(secret) = harness.failing_secret() {
        let text = format!("{events:?}");
        assert!(
            !text.contains(&secret),
            "{case}: the engine's quoted value reached the events: {text}"
        );
    }
}

/// A node the engine doesn't know never finishes as a success in the events (#322).
async fn unknown_nodes_never_finish_as_success(harness: &dyn ExecutorHarness) {
    let case = "unknown_nodes_never_finish_as_success";
    let executor = harness.executor().await;
    let unknown = RequestedNode::new("model.suite.no_such_node", "no_such_node");
    let request = ExecutionRequest::new(vec![unknown], ExecutionMode::Run).with_scope(SCOPE);
    let sink = CollectedEvents::new();
    let Ok(report) = executor.execute_with_events(&request, &sink).await else {
        return;
    };
    let events = sink.events();
    check_events(case, executor.as_ref(), &request, &report, &events);
    assert!(
        events.iter().all(|e| !matches!(
            &e.kind,
            RunEventKind::NodeFinished { stats, .. } if stats.status == NodeRunStatus::Success
        )),
        "{case}: {events:?}"
    );
}

/// Runs every case. Panics with the case name on the first failure.
pub async fn run(harness: &dyn ExecutorHarness) -> Report {
    let mut report = Report::default();
    builds_exactly_the_requested_nodes(harness).await;
    report.passed.push("builds_exactly_the_requested_nodes");
    reports_every_node_once_in_request_order(harness).await;
    report
        .passed
        .push("reports_every_node_once_in_request_order");
    refuses_an_empty_request(harness).await;
    report.passed.push("refuses_an_empty_request");
    match harness.failing() {
        Some(failing) => {
            failures_are_reported_not_errors(harness, failing).await;
            report.passed.push("failures_are_reported_not_errors");
        }
        None => report.skipped.push((
            "failures_are_reported_not_errors",
            "the harness can't arrange a failing node".to_owned(),
        )),
    }
    unknown_nodes_never_succeed(harness).await;
    report.passed.push("unknown_nodes_never_succeed");
    run_ids_are_unique(harness).await;
    report.passed.push("run_ids_are_unique");
    prepare_builds_nothing(harness).await;
    report.passed.push("prepare_builds_nothing");
    a_test_run_builds_nothing(harness).await;
    report.passed.push("a_test_run_builds_nothing");
    match harness.checked_and_unchecked() {
        Some((checked, unchecked)) => {
            only_checks_that_ran_vouch_for_a_node(harness, checked, unchecked).await;
            report.passed.push("only_checks_that_ran_vouch_for_a_node");
        }
        None => report.skipped.push((
            "only_checks_that_ran_vouch_for_a_node",
            "the harness can't arrange a node with checks and one without".to_owned(),
        )),
    }
    match harness.checked_source() {
        Some(source) => {
            source_checks_are_reported_per_source(harness, source).await;
            report.passed.push("source_checks_are_reported_per_source");
        }
        None => report.skipped.push((
            "source_checks_are_reported_per_source",
            "the harness can't arrange a source with checks".to_owned(),
        )),
    }
    match harness.failing_source() {
        Some((source, reader)) => {
            a_failing_source_check_skips_its_readers(harness, source, reader).await;
            report
                .passed
                .push("a_failing_source_check_skips_its_readers");
        }
        None => report.skipped.push((
            "a_failing_source_check_skips_its_readers",
            "the harness can't arrange a source whose check fails".to_owned(),
        )),
    }
    events_follow_the_run(harness).await;
    report.passed.push("events_follow_the_run");
    match harness.failing() {
        Some(failing) => {
            failed_nodes_finish_as_errors(harness, failing).await;
            report.passed.push("failed_nodes_finish_as_errors");
        }
        None => report.skipped.push((
            "failed_nodes_finish_as_errors",
            "the harness can't arrange a failing node".to_owned(),
        )),
    }
    unknown_nodes_never_finish_as_success(harness).await;
    report.passed.push("unknown_nodes_never_finish_as_success");
    report
}
