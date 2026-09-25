//! The planning rules of ADR-0013, one behaviour per test.

use std::collections::{BTreeMap, BTreeSet};

use ods_core::state::{
    DataVersion, Exactness, Fingerprint, PlanAction, ReasonCode, SnapshotId, StateSnapshot,
    Timestamp,
};
use ods_core::{FreshnessPolicy, Quorum, UnappliedSetting};
use ods_state::{Node, Outcome, PlanError, Project, RunResult, Source, plan, record, select};

const T0: i64 = 1_000_000;

#[allow(clippy::unnecessary_wraps, reason = "nodes hold a Result")]
fn fp(code: &str) -> Result<Fingerprint, String> {
    Ok(Fingerprint::from_content([
        ("file", code),
        ("config", "{}"),
    ]))
}

fn node(id: &str, parents: &[&str], code: &str) -> Node {
    Node::new(
        format!("model.p.{id}"),
        id,
        "model",
        parents
            .iter()
            .map(|p| {
                if p.starts_with("raw_") {
                    format!("source.p.{p}")
                } else {
                    format!("model.p.{p}")
                }
            })
            .collect(),
        fp(code),
        FreshnessPolicy::conservative(),
    )
}

fn source(id: &str, version: Option<&str>) -> Source {
    Source::new(
        format!("source.p.{id}"),
        id,
        version.map(|v| DataVersion::new(v, Exactness::Semantic, "sources.json")),
    )
}

/// `raw_orders → stg_orders → orders → report`, `raw_users → stg_users → report`,
/// and `lonely` (no parents).
fn project() -> Project {
    Project::new(
        vec![
            node("stg_orders", &["raw_orders"], "so"),
            node("stg_users", &["raw_users"], "su"),
            node("orders", &["stg_orders"], "o"),
            node("report", &["orders", "stg_users"], "r"),
            node("lonely", &[], "l"),
        ],
        vec![
            source("raw_orders", Some("v1")),
            source("raw_users", Some("u1")),
        ],
    )
}

fn all(project: &Project) -> BTreeSet<String> {
    select(project, &[]).unwrap()
}

/// Everything in `project` built successfully at T0, with sources predating the run.
fn built(project: &Project) -> StateSnapshot {
    let results: Vec<RunResult> = project
        .nodes
        .iter()
        .map(|n| RunResult::new(&n.id, Outcome::Success, Some(Timestamp::from_unix(T0))))
        .collect();
    record(
        project,
        None,
        &results,
        "run-1",
        Timestamp::from_unix(T0),
        true,
    )
    .snapshot
}

fn actions(
    project: &Project,
    snapshot: Option<&StateSnapshot>,
    now: i64,
) -> BTreeMap<String, (PlanAction, ReasonCode)> {
    let plan = plan(
        project,
        snapshot.map(|s| (SnapshotId(1), s)),
        &all(project),
        Timestamp::from_unix(now),
    )
    .unwrap();
    plan.entries
        .into_iter()
        .map(|e| (e.name, (e.action, e.reasons[0].code)))
        .collect()
}

fn build(code: ReasonCode) -> (PlanAction, ReasonCode) {
    (PlanAction::Build, code)
}

fn reuse(code: ReasonCode) -> (PlanAction, ReasonCode) {
    (PlanAction::Reuse, code)
}

#[test]
fn without_state_everything_is_built() {
    let p = project();
    for (name, decision) in actions(&p, None, T0) {
        assert_eq!(decision, build(ReasonCode::NeverBuilt), "{name}");
    }
}

#[test]
fn nothing_changed_means_everything_is_reused() {
    let p = project();
    let state = built(&p);
    for (name, decision) in actions(&p, Some(&state), T0 + 60) {
        assert_eq!(decision, reuse(ReasonCode::Unchanged), "{name}");
    }
}

#[test]
fn a_code_change_rebuilds_the_node_and_its_descendants_only() {
    let p = project();
    let state = built(&p);
    let mut changed = p.clone();
    changed.nodes[0].fingerprint = fp("so v2");
    let got = actions(&changed, Some(&state), T0 + 60);
    assert_eq!(got["stg_orders"], build(ReasonCode::CodeChanged));
    assert_eq!(got["orders"], build(ReasonCode::UpstreamCodeChanged));
    assert_eq!(got["report"], build(ReasonCode::UpstreamCodeChanged));
    assert_eq!(
        got["stg_users"],
        reuse(ReasonCode::Unchanged),
        "unrelated branch"
    );
    assert_eq!(
        got["lonely"],
        reuse(ReasonCode::Unchanged),
        "unrelated node"
    );
}

#[test]
fn a_code_change_names_the_changed_components() {
    let p = project();
    let state = built(&p);
    let mut changed = p.clone();
    changed.nodes[2].fingerprint = Ok(Fingerprint::from_content([
        ("file", "o"),
        ("config", "{\"x\":1}"),
    ]));
    let plan = plan(
        &changed,
        Some((SnapshotId(1), &state)),
        &all(&changed),
        Timestamp::from_unix(T0),
    )
    .unwrap();
    let orders = plan.entries.iter().find(|e| e.name == "orders").unwrap();
    assert_eq!(orders.changed_components, ["config"]);
    assert!(
        orders.reasons[0].message.contains("config"),
        "{:?}",
        orders.reasons
    );
    assert_ne!(orders.before, orders.after);
}

#[test]
fn new_source_data_rebuilds_its_readers_and_their_descendants() {
    let p = project();
    let state = built(&p);
    let mut fresh = p.clone();
    fresh.sources[0] = source("raw_orders", Some("v2"));
    let got = actions(&fresh, Some(&state), T0 + 60);
    assert_eq!(got["stg_orders"], build(ReasonCode::NewUpstreamData));
    assert_eq!(got["orders"], build(ReasonCode::NewUpstreamData));
    assert_eq!(got["report"], build(ReasonCode::NewUpstreamData));
    assert_eq!(got["stg_users"], reuse(ReasonCode::Unchanged));
}

#[test]
fn missing_or_weak_data_evidence_means_build() {
    let p = project();
    let state = built(&p);
    let mut unknown = p.clone();
    unknown.sources[1] = source("raw_users", None);
    let got = actions(&unknown, Some(&state), T0 + 60);
    assert_eq!(got["stg_users"], build(ReasonCode::MissingDataEvidence));
    assert_eq!(got["report"], build(ReasonCode::NewUpstreamData));
    assert_eq!(got["stg_orders"], reuse(ReasonCode::Unchanged));

    let mut proxy = p.clone();
    proxy.sources[1].version = Some(DataVersion::new("u1", Exactness::Proxy, "metadata"));
    assert_eq!(
        actions(&proxy, Some(&state), T0 + 60)["stg_users"],
        build(ReasonCode::MissingDataEvidence),
        "proxy evidence isn't enough to reuse"
    );
}

#[test]
fn source_versions_seen_after_the_run_are_not_trusted() {
    let p = project();
    let results: Vec<RunResult> = p
        .nodes
        .iter()
        .map(|n| RunResult::new(&n.id, Outcome::Success, None))
        .collect();
    let state = record(&p, None, &results, "run-1", Timestamp::from_unix(T0), false).snapshot;
    let got = actions(&p, Some(&state), T0 + 60);
    assert_eq!(got["stg_orders"], build(ReasonCode::MissingDataEvidence));
    assert_eq!(got["lonely"], reuse(ReasonCode::Unchanged));
}

#[test]
fn lag_tolerance_defers_new_data_until_due() {
    let mut p = project();
    p.nodes[0].policy.lag_tolerance_secs = 3600;
    let state = built(&p);
    let mut fresh = p.clone();
    fresh.sources[0] = source("raw_orders", Some("v2"));
    let got = actions(&fresh, Some(&state), T0 + 600);
    assert_eq!(got["stg_orders"], reuse(ReasonCode::WithinLagTolerance));
    assert_eq!(
        got["orders"],
        reuse(ReasonCode::Unchanged),
        "nothing new reaches it yet"
    );
    let later = actions(&fresh, Some(&state), T0 + 3600);
    assert_eq!(
        later["stg_orders"],
        build(ReasonCode::NewUpstreamData),
        "due exactly at the tolerance"
    );
}

#[test]
fn lag_tolerance_never_defers_an_upstream_code_change() {
    let mut p = project();
    p.nodes[2].policy.lag_tolerance_secs = 3600;
    let state = built(&p);
    let mut changed = p.clone();
    changed.nodes[0].fingerprint = fp("so v2");
    assert_eq!(
        actions(&changed, Some(&state), T0 + 60)["orders"],
        build(ReasonCode::UpstreamCodeChanged)
    );
}

#[test]
fn quorum_all_waits_for_every_parent() {
    let mut p = project();
    p.nodes[3].policy.require_fresh_data_from = Quorum::All;
    let state = built(&p);
    let mut fresh = p.clone();
    fresh.sources[0] = source("raw_orders", Some("v2"));
    assert_eq!(
        actions(&fresh, Some(&state), T0 + 60)["report"],
        reuse(ReasonCode::QuorumNotMet)
    );
    fresh.sources[1] = source("raw_users", Some("u2"));
    assert_eq!(
        actions(&fresh, Some(&state), T0 + 60)["report"],
        build(ReasonCode::NewUpstreamData)
    );
}

#[test]
fn a_policy_ods_cannot_honour_blocks_reuse() {
    let mut p = project();
    p.nodes[4].policy.unapplied.push(UnappliedSetting::new(
        "state.evaluate_volatile_sql",
        "true",
        true,
        "not supported yet",
    ));
    let state = built(&p);
    let got = actions(&p, Some(&state), T0 + 60);
    assert_eq!(got["lonely"], build(ReasonCode::PolicyBlocksReuse));
}

#[test]
fn incomplete_code_evidence_means_build() {
    let p = project();
    let state = built(&p);
    let mut parsed_only = p.clone();
    parsed_only.nodes[4].fingerprint = Err("no compiled SQL; run `dbt compile`".into());
    assert_eq!(
        actions(&parsed_only, Some(&state), T0)["lonely"],
        build(ReasonCode::CodeEvidenceIncomplete)
    );
}

#[test]
fn a_failed_run_keeps_the_last_successful_state_and_its_children_catch_up() {
    let p = project();
    let first = built(&p);
    // Run 2: stg_orders rebuilt at T0+100, orders failed, report skipped.
    let results = [
        RunResult::new(
            "model.p.stg_orders",
            Outcome::Success,
            Some(Timestamp::from_unix(T0 + 100)),
        ),
        RunResult::new("model.p.orders", Outcome::Failed, None),
        RunResult::new("model.p.report", Outcome::Skipped, None),
        RunResult::new("model.p.gone", Outcome::Success, None),
    ];
    let recorded = record(
        &p,
        Some((SnapshotId(1), &first)),
        &results,
        "run-2",
        Timestamp::from_unix(T0 + 200),
        true,
    );
    assert_eq!(recorded.advanced, ["model.p.stg_orders"]);
    assert_eq!(recorded.kept.len(), 2);
    assert_eq!(recorded.ignored, ["model.p.gone"]);
    let second = recorded.snapshot;
    assert_eq!(second.parent, Some(SnapshotId(1)));
    assert_eq!(
        second.nodes["model.p.orders"], first.nodes["model.p.orders"],
        "failed node kept"
    );
    assert_eq!(second.nodes["model.p.stg_orders"].run_id, "run-2");
    let got = actions(&p, Some(&second), T0 + 300);
    assert_eq!(got["stg_orders"], reuse(ReasonCode::Unchanged));
    assert_eq!(
        got["orders"],
        build(ReasonCode::NewUpstreamData),
        "its parent was rebuilt after it"
    );
    assert_eq!(got["report"], build(ReasonCode::NewUpstreamData));
}

#[test]
fn plans_are_ordered_by_depth_then_id_and_stable() {
    let p = project();
    let order: Vec<String> = plan(&p, None, &all(&p), Timestamp::from_unix(T0))
        .unwrap()
        .entries
        .iter()
        .map(|e| format!("{}:{}", e.depth, e.name))
        .collect();
    assert_eq!(
        order,
        [
            "0:lonely",
            "0:stg_orders",
            "0:stg_users",
            "1:orders",
            "2:report"
        ]
    );
    let mut reversed = p.clone();
    reversed.nodes.reverse();
    let again = plan(&reversed, None, &all(&reversed), Timestamp::from_unix(T0)).unwrap();
    let first = plan(&p, None, &all(&p), Timestamp::from_unix(T0)).unwrap();
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&again).unwrap(),
        "input order doesn't matter"
    );
}

#[test]
fn cycles_and_duplicates_are_errors() {
    let cyclic = Project::new(
        vec![
            node("a", &["b"], "a"),
            node("b", &["a"], "b"),
            node("c", &[], "c"),
        ],
        vec![],
    );
    assert_eq!(
        plan(&cyclic, None, &all(&cyclic), Timestamp::from_unix(T0)).unwrap_err(),
        PlanError::Cycle(vec!["model.p.a".into(), "model.p.b".into()])
    );
    let twice = Project::new(vec![node("a", &[], "a"), node("a", &[], "b")], vec![]);
    assert!(matches!(
        plan(&twice, None, &all(&twice), Timestamp::from_unix(T0)),
        Err(PlanError::Duplicate(_))
    ));
}

#[test]
fn selection_limits_the_plan_but_not_the_decisions() {
    let p = project();
    let state = built(&p);
    let mut changed = p.clone();
    changed.nodes[0].fingerprint = fp("so v2");
    let only_report = select(&changed, &["report".to_owned()]).unwrap();
    let plan = plan(
        &changed,
        Some((SnapshotId(1), &state)),
        &only_report,
        Timestamp::from_unix(T0),
    )
    .unwrap();
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(
        plan.entries[0].reasons[0].code,
        ReasonCode::UpstreamCodeChanged
    );

    let names = |specs: &[&str]| -> Vec<String> {
        let ids = select(
            &p,
            &specs.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap();
        ids.into_iter()
            .map(|i| i.trim_start_matches("model.p.").to_owned())
            .collect()
    };
    assert_eq!(names(&["+orders"]), ["orders", "stg_orders"]);
    assert_eq!(names(&["stg_orders+"]), ["orders", "report", "stg_orders"]);
    assert_eq!(names(&["+orders+"]), ["orders", "report", "stg_orders"]);
    assert_eq!(names(&["lonely stg_users"]), ["lonely", "stg_users"]);
    assert_eq!(names(&["model.p.lonely"]), ["lonely"]);
    assert!(
        select(&p, &["nope".to_owned()])
            .unwrap_err()
            .contains("nope")
    );
}

#[test]
fn every_reuse_says_the_relation_was_not_checked() {
    let p = project();
    let state = built(&p);
    let plan = plan(
        &p,
        Some((SnapshotId(1), &state)),
        &all(&p),
        Timestamp::from_unix(T0),
    )
    .unwrap();
    for entry in plan.with_action(PlanAction::Reuse) {
        assert!(
            entry
                .evidence
                .iter()
                .any(|e| e.kind == "relation_exists" && e.exactness == Exactness::None),
            "{}",
            entry.name
        );
    }
}
