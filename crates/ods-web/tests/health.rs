//! Health badges and coverage (#354) on Home and in the Catalog: each from a real
//! signal, linking to the nodes behind it, or said to be not measured.

use std::collections::BTreeMap;

use ods_core::FreshnessPolicy;
use ods_core::RelationName;
use ods_core::state::{
    ExecutionPlan, PlanAction, PlanEntry, Reason, ReasonCode, SnapshotId, Timestamp,
};
use ods_lineage::{GraphFilter, LineageNode, LineageProject, MemoryCache, NodeKind, build};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_web::Dashboard;
use ods_web::catalog::{CatalogInput, CatalogNode, CatalogQuery, CatalogTest, LastBuild, TestKind};
use ods_web::dashboard::state::{History, LastOutcome, LastRun};
use ods_web::dashboard::{Recorded, RunRecord, StateInput};
use ods_web::freshness::{FreshnessInput, SourceInput};

const RUN: &str = "9ea38bd5-0000-4000-8000-000000000002";

fn at(t: &str) -> Timestamp {
    Timestamp::parse(t).unwrap()
}

fn document() -> ods_lineage::GraphDocument {
    let rel = |n: &str| RelationName::new(["db", n]).unwrap();
    let project = LineageProject::new(vec![LineageNode::new(
        "seed.shop.raw",
        rel("raw"),
        NodeKind::Seed,
    )]);
    let (graph, _) = build(
        &project,
        &FakeSqlLineageAnalyzer::new(),
        &MemoryCache::default(),
    )
    .unwrap();
    graph.document(&|id: &str| id.to_owned(), &GraphFilter::default())
}

fn test_on(id: &str) -> CatalogTest {
    CatalogTest::new(format!("test.shop.{id}"), "not_null", None, TestKind::Data).covered(true)
}

/// `good` is tested and passed; `broken` failed in the last run; `untested` has no
/// tests; `fresh` was never built.
fn nodes() -> Vec<CatalogNode> {
    let mut good = CatalogNode::new("model.shop.good", "good", "model");
    good.tests = vec![test_on("good")];
    good.description = Some("Good.".into());
    let mut broken = CatalogNode::new("model.shop.broken", "broken", "model");
    broken.tests = vec![test_on("broken")];
    let untested = CatalogNode::new("model.shop.untested", "untested", "model");
    let fresh = CatalogNode::new("model.shop.fresh", "fresh", "model");
    vec![good, broken, untested, fresh]
}

fn builds() -> BTreeMap<String, LastBuild> {
    let built = || LastBuild::new(Some(2), RUN, at("2026-09-28T09:00:00Z"));
    let passed = || built().with_tested(RUN, at("2026-09-28T09:05:00Z"), None, true);
    BTreeMap::from([
        ("model.shop.good".to_owned(), passed()),
        ("model.shop.broken".to_owned(), passed()),
        ("model.shop.untested".to_owned(), built()),
    ])
}

fn plan() -> ExecutionPlan {
    let entry = |id: &str, action, code| {
        PlanEntry::new(
            id,
            id,
            "model",
            action,
            vec![Reason::new(code, "why")],
            FreshnessPolicy::conservative(),
            0,
        )
    };
    ExecutionPlan::new(
        Some(SnapshotId(2)),
        at("2026-09-29T12:00:00Z"),
        vec![
            entry("model.shop.good", PlanAction::Reuse, ReasonCode::Unchanged),
            entry(
                "model.shop.broken",
                PlanAction::Build,
                ReasonCode::CodeChanged,
            ),
            entry(
                "model.shop.untested",
                PlanAction::Reuse,
                ReasonCode::Unchanged,
            ),
            entry(
                "model.shop.fresh",
                PlanAction::Build,
                ReasonCode::NeverBuilt,
            ),
        ],
    )
}

fn dashboard(last_run: Option<LastRun>) -> Dashboard {
    let runs = vec![RunRecord::new(
        2,
        RUN,
        at("2026-09-28T09:00:00Z"),
        vec!["model.shop.good".into()],
        0,
    )];
    Dashboard::new("shop", "dev")
        .with_state(StateInput::Recorded(Box::new(
            Recorded::new(".ods/state.db", runs, 2, Ok(plan()))
                .with_history(History::new(Vec::new()).with_last_run(last_run)),
        )))
        .with_catalog(CatalogInput::new(nodes()).with_last_builds(builds()))
        .with_freshness(FreshnessInput::new(vec![
            SourceInput::new("source.shop.app.events", "app.events")
                .measured_with(Some("max(_at)".into())),
            SourceInput::new("source.shop.app.clicks", "app.clicks"),
        ]))
}

fn failed_run() -> LastRun {
    LastRun::new(
        "ods state build",
        "ods state build",
        at("2026-09-29T09:00:00Z"),
        ".ods/last_run.json",
    )
    .with_outcome(Some(LastOutcome::new(
        vec!["model.shop.broken".into()],
        vec![],
        vec![],
    )))
    .with_run(Some("shop/dev".into()), None)
}

fn json<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap()
}

fn row<'a>(rows: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == key)
        .unwrap()
}

#[test]
fn home_counts_each_health_and_links_to_it() {
    let home = json(&dashboard(Some(failed_run())).home_at(true, at("2026-09-29T12:00:00Z")));
    let counts: Vec<(&str, serde_json::Value)> = home["health"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["key"].as_str().unwrap(), r["count"].clone()))
        .collect();
    assert_eq!(
        counts,
        [
            ("healthy", 1.into()),
            ("warning", 1.into()),
            ("failing", 1.into()),
            ("unknown", 1.into()),
        ]
    );
    assert_eq!(
        row(&home["health"], "failing")["href"],
        "catalog?health=failing"
    );
    // Stale: built before and rebuilt by the plan; the never-built one isn't counted.
    assert_eq!(row(&home["signals"], "stale")["count"], 1);
    assert_eq!(
        row(&home["signals"], "stale")["href"],
        "catalog?decision=build"
    );
}

#[test]
fn failures_without_a_record_are_not_measured_never_zero() {
    let home = json(&dashboard(None).home_at(true, at("2026-09-29T12:00:00Z")));
    let failing = row(&home["health"], "failing");
    assert!(failing["count"].is_null(), "{failing}");
    assert!(failing["href"].is_null());
    assert!(failing["how"].as_str().unwrap().starts_with("Not measured"));
    // Without the record, whether any node failed since can't be told: every node
    // reads unknown, none healthy (AGENTS rule 3).
    assert_eq!(row(&home["health"], "healthy")["count"], 0);
    assert_eq!(row(&home["health"], "unknown")["count"], 4);
    assert!(row(&home["signals"], "failed_runs")["count"].is_null());
}

#[test]
fn coverage_counts_and_lists_what_is_missing() {
    let home = json(&dashboard(None).home_at(true, at("2026-09-29T12:00:00Z")));
    let tests = row(&home["coverage"], "tests");
    assert_eq!(
        (tests["count"].as_u64(), tests["total"].as_u64()),
        (Some(2), Some(4))
    );
    let missing: Vec<&str> = tests["uncovered"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect();
    assert_eq!(missing, ["fresh", "untested"]);
    assert_eq!(tests["uncovered"][0]["href"], "catalog/model.shop.fresh");
    assert_eq!(row(&home["coverage"], "descriptions")["count"], 1);
    assert_eq!(
        row(&home["coverage"], "constraints")["count"],
        0,
        "measured: none have any"
    );
    let sources = row(&home["coverage"], "source_freshness");
    assert_eq!(
        (sources["count"].as_u64(), sources["total"].as_u64()),
        (Some(1), Some(2))
    );
    assert_eq!(sources["uncovered"][0]["href"], "catalog/sources");
}

#[test]
fn the_catalog_filters_by_health_and_explains_each_badge() {
    let dashboard = dashboard(Some(failed_run()));
    let query = CatalogQuery::from_pairs(&[("health".to_owned(), "failing".to_owned())]);
    let view = json(&dashboard.catalog_at(&document(), &query, true, at("2026-09-29T12:00:00Z")));
    let rows = view["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{view:#}");
    assert_eq!(rows[0]["id"], "model.shop.broken");
    assert_eq!(rows[0]["health"]["health"], "failing");
    assert!(
        rows[0]["health"]["reasons"][0]
            .as_str()
            .unwrap()
            .starts_with("failed in the last run (ods state build"),
        "{}",
        rows[0]
    );
    let facet = view["facets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "health")
        .unwrap();
    let counts: Vec<u64> = facet["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["count"].as_u64().unwrap())
        .collect();
    assert_eq!(counts, [1, 1, 1, 1]);
}

#[test]
fn another_scopes_failures_are_never_this_ones() {
    // The same database, another target: its record says nothing about this scope.
    let other = failed_run().with_run(Some("shop/prod".into()), None);
    let home = json(&dashboard(Some(other)).home_at(true, at("2026-09-29T12:00:00Z")));
    assert!(
        row(&home["health"], "failing")["count"].is_null(),
        "not measured"
    );
    // Nor does a record that doesn't say which scope it ran for.
    let unscoped = failed_run().with_run(None, None);
    let home = json(&dashboard(Some(unscoped)).home_at(true, at("2026-09-29T12:00:00Z")));
    assert!(row(&home["health"], "failing")["count"].is_null());
}

#[test]
fn another_check_at_error_is_counted_even_without_the_runs_record() {
    let config: ods_config::HealthConfig =
        toml::from_str("[builtin.tests_required]\nseverity = \"error\"").unwrap();
    let settings = ods_health::HealthSettings::from_config(&config).unwrap();
    let home = json(
        &dashboard(None)
            .with_health(settings)
            .home_at(true, at("2026-09-29T12:00:00Z")),
    );
    let failing = row(&home["health"], "failing");
    // `untested` and `fresh` (never built) have no tests: tests_required fails at error,
    // which outranks unknown. Nodes the last run may have failed aren't counted, and the
    // row says so.
    assert_eq!(failing["count"], 2, "{failing}");
    assert_eq!(failing["href"], "catalog?health=failing");
    assert_eq!(
        failing["note"],
        "at least: the last run's failures aren't measured"
    );
}

/// What `ods health check` records: the built-ins, and a registered check that finds
/// `good` without an owner.
async fn health_record() -> ods_health::record::HealthRecord {
    use ods_health::{HealthSettings, LastFailures, Severity};
    use ods_provider_fake::FakeHealthCheck;
    let settings = HealthSettings::default()
        .with_check(std::sync::Arc::new(
            FakeHealthCheck::new("owner", Severity::Error).failing("model.shop.good"),
        ))
        .unwrap();
    let catalog = CatalogInput::new(nodes()).with_last_builds(builds());
    let failures = LastFailures::new(
        at("2026-09-29T09:00:00Z"),
        "ods state build",
        ["model.shop.broken".to_owned()],
        Vec::<String>::new(),
    );
    let scope = ods_web::health::check_scope(&catalog, Some(failures));
    let report = settings.run(&scope, ods_health::CHECK_TIMEOUT).await;
    ods_health::record::HealthRecord::new("shop/dev", at("2026-09-29T10:00:00Z"), report)
}

#[tokio::test]
async fn a_recorded_checks_findings_count_with_their_time() {
    let record = health_record().await;
    let dashboard = dashboard(Some(failed_run())).with_health_record(Some(&record));
    let now = at("2026-09-29T12:00:00Z");

    let home = json(&dashboard.home_at(true, now));
    assert_eq!(row(&home["health"], "failing")["count"], 2, "{home:#}");
    assert_eq!(row(&home["health"], "healthy")["count"], 0);
    assert_eq!(home["health_recorded_at"], "2026-09-29T10:00:00Z");
    let how = home["health_how"].as_str().unwrap();
    assert!(
        how.contains("2026-09-29T10:00:00Z") && how.contains("`owner`"),
        "{how}"
    );

    let query = CatalogQuery::from_pairs(&[("health".to_owned(), "failing".to_owned())]);
    let view = json(&dashboard.catalog_at(&document(), &query, true, now));
    assert_eq!(view["health_recorded_at"], "2026-09-29T10:00:00Z");
    let good = view["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "model.shop.good")
        .unwrap();
    let owner: Vec<&serde_json::Value> = good["health"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["check"] == "owner")
        .collect();
    assert_eq!(owner.len(), 1, "{good:#}");
    assert_eq!(owner[0]["source"], "plugin");
    assert_eq!(owner[0]["status"], "fail");
}

#[tokio::test]
async fn a_node_the_record_didnt_cover_is_unknown_for_its_checks() {
    let mut record = health_record().await;
    record.report.badges.remove("model.shop.good");
    let dashboard = dashboard(Some(failed_run())).with_health_record(Some(&record));
    let home = json(&dashboard.home_at(true, at("2026-09-29T12:00:00Z")));
    // `good` would be healthy on the built-ins; the recorded check never vouched for it.
    assert_eq!(row(&home["health"], "healthy")["count"], 0, "{home:#}");
    assert_eq!(row(&home["health"], "unknown")["count"], 2);
}

#[test]
fn without_a_record_nothing_is_said_about_one() {
    let home = json(&dashboard(Some(failed_run())).home_at(true, at("2026-09-29T12:00:00Z")));
    assert!(home.get("health_recorded_at").is_none(), "{home:#}");
    assert!(
        !home["health_how"]
            .as_str()
            .unwrap()
            .contains("ods health check")
    );
}

/// #401 review: a recorded check at severity error is measured on Home even without the
/// last run's record, which only leaves that run's failures out.
#[tokio::test]
async fn a_recorded_error_check_is_measured_without_the_runs_record() {
    let record = health_record().await;
    let dashboard = dashboard(None).with_health_record(Some(&record));
    let home = json(&dashboard.home_at(true, at("2026-09-29T12:00:00Z")));
    let failing = row(&home["health"], "failing");
    assert_eq!(failing["count"], 1, "{failing:#}");
    assert_eq!(failing["href"], "catalog?health=failing");
    assert!(
        failing["note"].as_str().unwrap().starts_with("at least"),
        "the run's failures still aren't measured: {failing:#}"
    );
}
