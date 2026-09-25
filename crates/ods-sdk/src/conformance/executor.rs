//! Conformance suite for [`Executor`].

use std::sync::Arc;

use async_trait::async_trait;

use super::Report;
use crate::contracts::executor::{
    ExecutionMode, ExecutionRequest, ExecutionStatus, Executor, PrepareRequest, RequestedNode,
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
    report
}
