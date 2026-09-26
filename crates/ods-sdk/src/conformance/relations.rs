//! Conformance suite for [`RelationInspector`].

use std::sync::Arc;

use async_trait::async_trait;

use super::Report;
use crate::contracts::executor::RequestedNode;
use crate::contracts::relations::{RelationInspector, RelationPresence, RelationReport};

/// What the suite needs from an inspector under test.
#[async_trait]
pub trait RelationHarness: Send + Sync {
    /// A fresh inspector over a warehouse where every [`present`](Self::present) node's
    /// relation exists. Called once per case.
    async fn inspector(&self) -> Arc<dyn RelationInspector>;

    /// At least two nodes whose relations exist.
    fn present(&self) -> Vec<RequestedNode>;

    /// Drops `node`'s relation in the warehouse of the last
    /// [`inspector`](Self::inspector). Returns `false` if the harness can't, which skips
    /// the case that needs it.
    async fn drop_relation(&self, node: &RequestedNode) -> bool;
}

fn ids(nodes: &[RequestedNode]) -> Vec<String> {
    nodes.iter().map(|n| n.id.clone()).collect()
}

fn listed(report: &RelationReport) -> Vec<String> {
    report.nodes.iter().map(|(id, _)| id.clone()).collect()
}

fn presence<'a>(report: &'a RelationReport, id: &str) -> &'a RelationPresence {
    &report
        .nodes
        .iter()
        .find(|(node, _)| node == id)
        .unwrap_or_else(|| panic!("{id} is not in the report: {report:?}"))
        .1
}

async fn lists_every_node_once_in_request_order(harness: &dyn RelationHarness) {
    let case = "lists_every_node_once_in_request_order";
    let inspector = harness.inspector().await;
    let mut nodes = harness.present();
    assert!(
        nodes.len() >= 2,
        "{case}: the harness needs two present nodes"
    );
    nodes.reverse();
    let report = inspector
        .inspect(&nodes)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_eq!(listed(&report), ids(&nodes), "{case}: listed nodes");
    for node in &nodes {
        assert!(
            matches!(
                presence(&report, &node.id),
                RelationPresence::Present { .. }
            ),
            "{case}: {report:?}"
        );
    }
}

async fn unknown_nodes_are_never_present(harness: &dyn RelationHarness) {
    let case = "unknown_nodes_are_never_present";
    let inspector = harness.inspector().await;
    let unknown = RequestedNode::new("model.ods_conformance.no_such_node", "no_such_node");
    let report = inspector
        .inspect(std::slice::from_ref(&unknown))
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_eq!(
        listed(&report),
        std::slice::from_ref(&unknown.id),
        "{case}: listed"
    );
    assert!(
        !matches!(
            presence(&report, &unknown.id),
            RelationPresence::Present { .. }
        ),
        "{case}: {report:?}"
    );
}

async fn a_dropped_relation_is_missing(harness: &dyn RelationHarness) -> bool {
    let case = "a_dropped_relation_is_missing";
    let inspector = harness.inspector().await;
    let nodes = harness.present();
    if !harness.drop_relation(&nodes[0]).await {
        return false;
    }
    let report = inspector
        .inspect(&nodes)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_eq!(
        presence(&report, &nodes[0].id),
        &RelationPresence::Missing,
        "{case}: {report:?}"
    );
    assert!(
        matches!(
            presence(&report, &nodes[1].id),
            RelationPresence::Present { .. }
        ),
        "{case}: the others are still there: {report:?}"
    );
    // Read-only: asking again changes nothing.
    let again = inspector
        .inspect(&nodes)
        .await
        .unwrap_or_else(|e| panic!("{case}: {e}"));
    assert_eq!(again, report, "{case}: inspecting changed the warehouse");
    true
}

/// Runs every case against `harness`.
pub async fn run(harness: &dyn RelationHarness) -> Report {
    let mut report = Report::default();
    lists_every_node_once_in_request_order(harness).await;
    report.passed.push("lists_every_node_once_in_request_order");
    unknown_nodes_are_never_present(harness).await;
    report.passed.push("unknown_nodes_are_never_present");
    if a_dropped_relation_is_missing(harness).await {
        report.passed.push("a_dropped_relation_is_missing");
    } else {
        report.skipped.push((
            "a_dropped_relation_is_missing",
            "the harness can't drop a relation".to_owned(),
        ));
    }
    report
}
