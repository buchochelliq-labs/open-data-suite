//! The fake executor's simulated run: parallel nodes, a failure, a node skipped
//! because of it, and nodes that report rows and nodes that don't (#322).

use ods_core::Capability;
use ods_provider_fake::{FakeClock, FakeExecutor};
use ods_sdk::Provider;
use ods_sdk::contracts::executor::{
    ExecutionMode, ExecutionRequest, ExecutionStatus, Executor, RequestedNode,
};
use ods_sdk::contracts::run_events::{
    CollectedEvents, NodeRunStatus, RunEventKind, RunOutcome, RunSummary,
};

fn node(id: &str) -> RequestedNode {
    RequestedNode::new(id, id.rsplit('.').next().unwrap())
}

/// The design's demo run (docs/design/dashboard/boards/live-run): two threads, a view
/// that reports no rows, tables that do, a failure and the node it stops.
fn demo() -> FakeExecutor {
    FakeExecutor::new(
        FakeClock::new(),
        [
            "model.shop.stg_orders",
            "model.shop.orders",
            "model.shop.customers",
            "model.shop.segment_summary",
        ],
    )
    .with_duration("model.shop.stg_orders", 1900)
    .with_duration("model.shop.orders", 4200)
    .with_rows("model.shop.orders", 99)
    .with_extra("model.shop.orders", "query_id", "q-123")
    .with_duration("model.shop.customers", 2700)
    .with_rows("model.shop.customers", 100)
    .failing_with(
        "model.shop.customer_segments",
        "KeyError: 'sk_live_SECRET'\nTraceback (most recent call last): …",
    )
    .with_duration("model.shop.customer_segments", 3600)
    .with_upstream("model.shop.orders", ["model.shop.stg_orders"])
    .with_upstream("model.shop.customer_segments", ["model.shop.customers"])
    .with_upstream(
        "model.shop.segment_summary",
        ["model.shop.customer_segments"],
    )
    .with_checks("model.shop.orders", ["test.shop.orders_id_unique"])
}

fn request() -> ExecutionRequest {
    ExecutionRequest::new(
        vec![
            node("model.shop.stg_orders"),
            node("model.shop.customers"),
            node("model.shop.orders"),
            node("model.shop.customer_segments"),
            node("model.shop.segment_summary"),
        ],
        ExecutionMode::Build,
    )
    .with_scope("shop/dev")
}

#[tokio::test]
async fn a_simulated_run_reports_parallel_nodes_a_failure_and_what_it_stopped() {
    let executor = demo();
    assert!(
        executor
            .info()
            .capabilities
            .contains(&Capability::RunEvents)
    );
    let sink = CollectedEvents::new();
    let report = executor
        .execute_with_events(&request(), &sink)
        .await
        .unwrap();
    let events = sink.events();
    assert!(!report.succeeded);

    // Two nodes run at once on two threads.
    let starts: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::NodeStarted { node, thread } => Some((e.at, node, thread.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(starts[0].0, starts[1].0, "{starts:?}");
    assert_ne!(starts[0].2, starts[1].2, "{starts:?}");

    let run = RunSummary::from_events(&events);
    assert_eq!(run.outcome, Some(RunOutcome::Failed));
    assert_eq!(run.scope.as_deref(), Some("shop/dev"));
    assert!(run.live);

    let orders = &run.get("model.shop.orders").unwrap().stats;
    assert_eq!(orders.status, NodeRunStatus::Success);
    assert_eq!(orders.rows_affected, Some(99));
    assert_eq!(orders.adapter["query_id"], "q-123");
    assert_eq!(orders.took_ms(), Some(4200));
    assert_eq!(
        orders.compile_ms.zip(orders.execute_ms).map(|(c, e)| c + e),
        Some(4200)
    );
    assert_eq!(orders.tests.unwrap().passed, 1);
    // It waited for its parent.
    assert!(orders.started_at >= run.get("model.shop.stg_orders").unwrap().stats.finished_at);

    let view = &run.get("model.shop.stg_orders").unwrap().stats;
    assert_eq!(view.status, NodeRunStatus::Success);
    assert_eq!(
        view.rows_affected, None,
        "not reported is missing, not zero"
    );
    assert_eq!(view.tests, None, "no checks ran on it");

    let failed = &run.get("model.shop.customer_segments").unwrap().stats;
    assert_eq!(failed.status, NodeRunStatus::Error);
    let error = failed.error.as_ref().unwrap();
    assert_eq!(error.message(), "KeyError: [value removed]");
    assert_eq!(failed.rows_affected, None);

    let skipped = &run.get("model.shop.segment_summary").unwrap().stats;
    assert_eq!(skipped.status, NodeRunStatus::Skipped);
    assert_eq!(skipped.blocked_by, ["model.shop.customer_segments"]);
    assert_eq!(skipped.started_at, None, "never started");
    assert_eq!(
        report
            .nodes
            .iter()
            .find(|n| n.node == "model.shop.segment_summary")
            .unwrap()
            .status,
        ExecutionStatus::Skipped
    );

    assert_eq!(run.totals.count(NodeRunStatus::Success), 3);
    assert_eq!(run.totals.count(NodeRunStatus::Error), 1);
    assert_eq!(run.totals.count(NodeRunStatus::Skipped), 1);
    assert_eq!(run.totals.rows_affected, 199);
    assert!(run.totals.rows_is_lower_bound(), "the view didn't report");
    // stg_orders 1.9s then orders 4.2s on one thread; customers 2.7s then
    // customer_segments 3.6s on the other.
    assert_eq!(run.totals.duration_ms, Some(6300));

    let journal = serde_json::to_string(&events).unwrap();
    assert!(!journal.contains("sk_live_SECRET"), "{journal}");
}

#[tokio::test]
async fn without_run_events_the_run_is_still_recorded_without_stats() {
    let executor = demo().without_run_events();
    assert!(
        !executor
            .info()
            .capabilities
            .contains(&Capability::RunEvents)
    );
    let sink = CollectedEvents::new();
    executor
        .execute_with_events(&request(), &sink)
        .await
        .unwrap();
    let run = RunSummary::from_events(&sink.events());
    assert!(!run.live);
    assert_eq!(run.outcome, Some(RunOutcome::Failed));
    let orders = &run.get("model.shop.orders").unwrap().stats;
    assert_eq!(orders.status, NodeRunStatus::Success);
    assert_eq!(orders.rows_affected, None);
    assert_eq!(orders.took_ms(), None);
    assert_eq!(
        run.get("model.shop.segment_summary").unwrap().stats.status,
        NodeRunStatus::Skipped
    );
}

#[tokio::test]
async fn execute_and_execute_with_events_report_the_same() {
    let plain = demo().execute(&request()).await.unwrap();
    let sink = CollectedEvents::new();
    let with_events = demo().execute_with_events(&request(), &sink).await.unwrap();
    assert_eq!(plain, with_events);
}
