//! The State pages (#311): Plan and its Why panel, Runs, one Run, and their JSON API.

use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLink, RelationLinkFields};
use ods_web::catalog::{CatalogInput, CatalogNode};
use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};

use ods_core::FreshnessPolicy;
use ods_core::state::{
    Evidence, Exactness, ExecutionPlan, Fingerprint, NodeState, PlanAction, PlanEntry, Reason,
    ReasonCode, SnapshotId, StateSnapshot, TargetIdentity, Timestamp,
};
use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, RelationName};
use ods_lineage::{GraphFilter, LineageNode, LineageProject, MemoryCache, NodeKind, build};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage};
use ods_web::dashboard::state::{History, LastOutcome, LastRun, RunFilter, RunOutcome};
use ods_web::dashboard::{Recorded, RunRecord, StateInput, Target};
use ods_web::{Dashboard, ServeOptions, Snapshot, router};

fn rel(name: &str) -> RelationName {
    RelationName::new(["db", name]).unwrap()
}

/// A tiny lineage graph: the server needs one; it also names a source.
fn lineage() -> Snapshot {
    let lineage = QueryLineage::new(
        vec![OutputColumn::new(
            "id",
            [(
                ColumnRef::new(rel("raw_orders"), "id"),
                EdgeKind::Direct(DirectKind::Identity),
            )]
            .into(),
            "id",
            Confidence::Exact,
        )],
        std::collections::BTreeSet::default(),
        [rel("raw_orders")].into(),
        "rows",
        vec![],
    );
    let analyzer = FakeSqlLineageAnalyzer::new().with("orders.sql", lineage);
    let project = LineageProject::new(vec![
        LineageNode::new(
            "source.shop.raw.orders",
            rel("raw_orders"),
            NodeKind::Source,
        )
        .with_columns(["id"]),
        LineageNode::new("model.shop.orders", rel("orders"), NodeKind::Model)
            .with_sql("orders.sql")
            .with_depends_on(["source.shop.raw.orders"]),
    ]);
    let (graph, _) = build(&project, &analyzer, &MemoryCache::default()).unwrap();
    let name = |id: &str| id.rsplit('.').next().unwrap_or(id).to_owned();
    let document = graph.document(&name, &GraphFilter::default());
    Snapshot::new(document, graph, "fixture")
}

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

const RUN_1: &str = "e6f54fe3-0000-4000-8000-000000000001";
const RUN_2: &str = "9ea38bd5-0000-4000-8000-000000000002";
const RUN_3: &str = "4c0b5c8f-0000-4000-8000-000000000003";
const NOW: &str = "2026-09-29T12:00:00Z";

fn entry(id: &str, action: PlanAction, code: ReasonCode, message: &str, depth: u32) -> PlanEntry {
    let name = id.rsplit('.').next().unwrap();
    let kind = id.split('.').next().unwrap();
    PlanEntry::new(
        id,
        name,
        kind,
        action,
        vec![Reason::new(code, message)],
        FreshnessPolicy::conservative(),
        depth,
    )
}

fn node(sql: &str, run: &str, built_at: &str) -> NodeState {
    NodeState::new(
        Fingerprint::from_content([("sql", sql), ("config", "{}")]),
        at(built_at),
        run,
        BTreeMap::new(),
    )
}

/// Three snapshots: seeds, then everything, then `customers` and its reader.
fn snapshots() -> Vec<(u64, StateSnapshot)> {
    let target = Some(TargetIdentity::new("dev").kind(Some("warehouse".into())));
    let seed = node("raw", RUN_1, "2026-09-29T00:00:15Z");
    let s1 = StateSnapshot::new(
        None,
        at("2026-09-29T00:00:15Z"),
        RUN_1,
        BTreeMap::from([("seed.raw_orders".into(), seed.clone())]),
    )
    .with_target(target.clone());
    let mut customers = node("select 20", RUN_2, "2026-09-29T00:01:03Z");
    customers
        .parents
        .insert("seed.raw_orders".into(), RUN_1.into());
    let mut view = node("select *", RUN_2, "2026-09-29T00:01:03Z");
    view.parents.insert("model.customers".into(), RUN_2.into());
    let s2 = StateSnapshot::new(
        Some(SnapshotId(1)),
        at("2026-09-29T00:01:03Z"),
        RUN_2,
        BTreeMap::from([
            ("seed.raw_orders".into(), seed.clone()),
            ("model.customers".into(), customers),
            ("model.customers_view".into(), view),
        ]),
    )
    .with_target(target.clone());
    let mut customers = node("select 25", RUN_3, "2026-09-29T00:02:37Z");
    customers
        .parents
        .insert("seed.raw_orders".into(), RUN_1.into());
    let mut view = node("select *", RUN_3, "2026-09-29T00:02:37Z");
    view.parents.insert("model.customers".into(), RUN_3.into());
    let s3 = StateSnapshot::new(
        Some(SnapshotId(2)),
        at("2026-09-29T00:02:37Z"),
        RUN_3,
        BTreeMap::from([
            ("seed.raw_orders".into(), seed),
            ("model.customers".into(), customers),
            ("model.customers_view".into(), view),
        ]),
    )
    .with_target(target);
    vec![(3, s3), (2, s2), (1, s1)]
}

/// The plan against snapshot 3: `customers` changed, its reader follows; the seed and
/// a model reading a source are reused, the latter on exact source-version evidence.
fn plan() -> ExecutionPlan {
    let mut customers = entry(
        "model.customers",
        PlanAction::Build,
        ReasonCode::CodeChanged,
        &format!("code changed since run {RUN_3}: sql"),
        1,
    );
    let before = Fingerprint::from_content([("sql", "select 25"), ("config", "{}")]);
    let after = Fingerprint::from_content([("sql", "select 30"), ("config", "{}")]);
    customers.before = Some(before.digest.clone());
    customers.after = Some(after.digest.clone());
    customers.changed_components = vec!["sql".into()];
    customers.depends_on = vec!["seed.raw_orders".into()];
    customers.evidence = vec![
        Evidence::new(
            "fingerprint",
            "model.customers",
            Some(after.digest.clone()),
            Exactness::Exact,
        ),
        Evidence::new(
            "parent_decision",
            "seed.raw_orders",
            Some("reuse: unchanged".into()),
            Exactness::Exact,
        ),
    ];
    let mut view = entry(
        "model.customers_view",
        PlanAction::Build,
        ReasonCode::UpstreamCodeChanged,
        "upstream code changed: customers will be rebuilt",
        2,
    );
    view.depends_on = vec!["model.customers".into()];
    let mut seed = entry(
        "seed.raw_orders",
        PlanAction::Reuse,
        ReasonCode::Unchanged,
        &format!("code and inputs unchanged since run {RUN_1}"),
        0,
    );
    seed.before = Some("aa".into());
    seed.after = Some("aa".into());
    seed.evidence = vec![
        Evidence::new(
            "fingerprint",
            "seed.raw_orders",
            Some("aa".into()),
            Exactness::Exact,
        ),
        Evidence::new("relation_exists", "seed.raw_orders", None, Exactness::None),
    ];
    let mut orders = entry(
        "model.orders",
        PlanAction::Reuse,
        ReasonCode::Unchanged,
        &format!("code and inputs unchanged since run {RUN_2}"),
        1,
    );
    orders.depends_on = vec!["source.shop.raw.orders".into()];
    orders.evidence = vec![
        Evidence::new(
            "source_data_version",
            "source.shop.raw.orders",
            Some("v42".into()),
            Exactness::Exact,
        ),
        Evidence::new(
            "source_version_strategy",
            "source.shop.raw.orders",
            Some("relation_version".into()),
            Exactness::Exact,
        ),
        Evidence::new(
            "source_version_origin",
            "source.shop.raw.orders",
            Some("table history".into()),
            Exactness::Exact,
        ),
        Evidence::new(
            "source_version_skipped",
            "source.shop.raw.orders",
            Some("freshness: no loaded_at_field".into()),
            Exactness::None,
        ),
        Evidence::new(
            "relation_exists",
            "model.orders",
            Some("table".into()),
            Exactness::Exact,
        ),
    ];
    ExecutionPlan::new(
        Some(SnapshotId(3)),
        at(NOW),
        vec![seed, customers, orders, view],
    )
}

fn recorded(history: History, plan: ExecutionPlan) -> Dashboard {
    let runs = history
        .snapshots
        .iter()
        .map(|(id, s)| RunRecord::of(*id, s))
        .collect();
    Dashboard::new("jaffle_ods", "dev")
        .with_target(Some(Target::new("dev", Some("warehouse".into()))))
        .with_state(StateInput::Recorded(Box::new(
            Recorded::new(".ods/state.db", runs, 3, Ok(plan)).with_history(history),
        )))
}

fn demo() -> Dashboard {
    recorded(History::new(snapshots()), plan())
}

/// A failed last run: started before snapshot 3 was recorded, so tied to it.
fn failed_last_run(started: &str) -> LastRun {
    LastRun::new(
        "ods state build --select +customers_view",
        "ods state build",
        at(started),
        "/home/me/p/.ods/state.db.last-run.json",
    )
    .with_outcome(Some(LastOutcome::new(
        vec!["model.orders".into()],
        vec!["model.orders_view".into()],
        vec![],
    )))
    .with_retry(
        Some("ods state retry".into()),
        Some("ods state retry --failed".into()),
    )
}

/// The failed last run, for this page's scope, with a run id.
fn scoped(last: LastRun, run_id: Option<&str>) -> LastRun {
    last.with_run(Some("jaffle_ods/dev".into()), run_id.map(str::to_owned))
}

/// The demo, with `last` as the last run.
fn with_last(last: LastRun) -> Dashboard {
    recorded(History::new(snapshots()).with_last_run(Some(last)), plan())
}

fn start(dashboard: Dashboard) -> SocketAddr {
    let options = ServeOptions::new(([127, 0, 0, 1], 0).into());
    serve_app(router(lineage().with_dashboard(dashboard), &options))
}

/// Serves `app` on a loopback port, whatever address its options name.
fn serve_app(app: axum::Router) -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

fn request(addr: SocketAddr, method: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        addr.port()
    )
    .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let response = String::from_utf8_lossy(&bytes);
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, body.to_owned())
}

fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    request(addr, "GET", path)
}

/// Pretty JSON with keys sorted, and the plan's time (now, per request) fixed.
fn pretty(body: &str) -> String {
    fn sorted(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let map: BTreeMap<String, serde_json::Value> =
                    map.into_iter().map(|(k, v)| (k, sorted(v))).collect();
                serde_json::Value::Object(map.into_iter().collect())
            }
            serde_json::Value::Array(items) => items.into_iter().map(sorted).collect(),
            other => other,
        }
    }
    let value: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"));
    serde_json::to_string_pretty(&sorted(value)).unwrap()
}

#[test]
fn the_plan_api_and_page_show_every_decision_with_its_reasons() {
    let addr = start(demo());
    let (status, body) = get(addr, "/api/state/plan");
    assert_eq!(status, 200);
    insta::assert_snapshot!("plan", pretty(&body));

    let (status, page) = get(addr, "/state/plan");
    assert_eq!(status, 200);
    // The table: every node, its pill and its reason, run ids shortened.
    for name in ["raw_orders", "customers", "orders", "customers_view"] {
        assert!(page.contains(&format!(">{name}</a>")), "{name}");
    }
    assert!(page.contains(r#"<span class="st-pill build">BUILD</span>"#));
    assert!(page.contains(r#"<span class="st-pill reuse">REUSE</span>"#));
    assert!(page.contains("code changed since run 4c0b5c8f: sql"));
    // The Why panel opens on the first build.
    assert!(
        page.contains(r#"aria-label="Why customers builds""#),
        "{page}"
    );
    assert!(page.contains("Recorded build found"));
    assert!(page.contains("Fingerprint differs in one part"));
    assert!(page.contains(r#"<span class="st-changed">changed</span>"#));
    assert!(
        page.contains("customers_view</a>."),
        "its reader rebuilds with it"
    );
    // Offline, relations aren't checked: never shown as checked.
    assert!(
        page.contains("1 / 2"),
        "only the one with relation evidence"
    );
    assert!(page.contains("reused relations found in the warehouse"));
    // Builds first, then reuses, each in plan order.
    let customers = page.find(r#"data-node="model.customers""#).unwrap();
    let seed = page.find(r#"data-node="seed.raw_orders""#).unwrap();
    assert!(customers < seed, "builds come first");
    assert!(page.contains(
        r#"<tr id="r0" class="selected" aria-current="true" data-node="model.customers">"#
    ));
    // State's pages in the navigation: Plan and Runs link, the rest are planned.
    assert!(
        page.contains(r#"<a href="../state/plan" aria-current="page" data-item="plan">Plan</a>"#)
    );
    assert!(page.contains(r#"<a href="../state/runs" data-item="runs">Runs</a>"#));
    assert!(page.contains(r#"data-item="history">History<span class="chip">Planned</span>"#));
    assert!(page.contains(r#"<a class="crumb" href="plan">State</a>"#));
    assert!(page.contains("Plan for dev"), "named after the target");
    // Every link stays below the dashboard's root.
    assert!(page.contains(r#"<a href="../" data-section="home">"#));
    assert!(page.contains(r#"<a href="../state/plan" aria-current="page" data-section="state">"#));
    assert!(page.contains("url('../assets/fonts/"));
    assert!(page.contains(r#"<meta name="ods-root" content="../">"#));
    assert!(page.contains(r#"href="../lineage?node=model%2Ecustomers""#));
    // The Why panel links to the node's Model page (#313), from one level down.
    assert!(
        page.contains(r#" · <a href="../catalog/model%2Ecustomers">Model page</a>"#),
        "{page}"
    );
    assert!(!page.contains("https://"), "no CDN");
}

#[test]
fn the_why_panel_says_what_ods_state_explain_says() {
    let dashboard = demo();
    let names = BTreeMap::new();
    for node in [
        "model.customers",
        "model.customers_view",
        "seed.raw_orders",
        "model.orders",
    ] {
        let why = dashboard.why_view(at(NOW), node, &names).unwrap();
        let explained = ods_state::explain(&plan(), node).unwrap();
        assert_eq!(why.explanation, explained, "{node}");
        assert_eq!(
            serde_json::to_value(&why.explanation).unwrap(),
            serde_json::to_value(&explained).unwrap()
        );
        let verb = if why.action == PlanAction::Build {
            "built"
        } else {
            "reused"
        };
        assert_eq!(why.verdict, format!("{} would be {verb}", why.name));
    }
    // The chain goes upstream to the root cause, as `explain` traces it.
    let view = dashboard
        .why_view(at(NOW), "customers_view", &names)
        .unwrap();
    assert_eq!(view.node, "model.customers_view", "a unique name resolves");
    let chain: Vec<(usize, &str)> = view
        .chain
        .iter()
        .map(|l| (l.depth, l.name.as_str()))
        .collect();
    assert_eq!(chain, [(0, "customers_view"), (1, "customers")]);
    assert_eq!(
        view.chain[1].reasons,
        ["code changed since run 4c0b5c8f: sql"]
    );

    let addr = start(demo());
    let (status, body) = get(addr, "/api/state/plan/model%2Ecustomers");
    assert_eq!(status, 200);
    insta::assert_snapshot!("why_customers", pretty(&body));
    let (status, _) = get(addr, "/api/state/plan/model.nope");
    assert_eq!(status, 404);
}

#[test]
fn a_node_reused_on_source_version_evidence_shows_its_strategy_and_origin() {
    let why = demo()
        .why_view(at(NOW), "model.orders", &BTreeMap::new())
        .unwrap();
    assert_eq!(why.action, PlanAction::Reuse);
    let source = &why.sources[0];
    assert_eq!(source.source, "source.shop.raw.orders");
    assert_eq!(source.version.as_deref(), Some("v42"));
    assert_eq!(source.grade, "exact");
    assert!(source.usable);
    assert_eq!(source.strategy.as_deref(), Some("relation_version"));
    assert_eq!(source.origin.as_deref(), Some("table history"));
    assert_eq!(source.skipped, ["freshness: no loaded_at_field"]);
    assert_eq!(why.relation.status, "present");

    let addr = start(demo());
    let (_, page) = get(addr, "/state/plan?node=model.orders");
    assert!(
        page.contains(r#"aria-label="Why orders is reused""#),
        "{page}"
    );
    // Named from the lineage graph, which knows sources.
    assert!(page.contains(r#"<span class="mono">orders</span> <span class="st-grade fact""#));
    assert!(page.contains("version v42"));
    assert!(page.contains("strategy relation_version · from table history"));
    assert!(page.contains("passed over freshness: no loaded_at_field"));
    assert!(page.contains("found in the warehouse (table)"));
}

#[test]
fn unknown_or_inferred_evidence_is_never_shown_as_fact() {
    let mut entry = entry(
        "model.orders",
        PlanAction::Build,
        ReasonCode::MissingDataEvidence,
        "no usable data version for raw.orders: will build",
        1,
    );
    entry.evidence = vec![
        Evidence::new(
            "source_data_version",
            "source.shop.raw.orders",
            Some("2026-09-28T00:00:00Z".into()),
            Exactness::Proxy,
        ),
        Evidence::new(
            "source_version_strategy",
            "source.shop.raw.orders",
            Some("freshness".into()),
            Exactness::Proxy,
        ),
        Evidence::new("parent_decision", "model.x", None, Exactness::Inferred),
    ];
    let plan = ExecutionPlan::new(Some(SnapshotId(3)), at(NOW), vec![entry]);
    let dashboard = recorded(History::new(snapshots()), plan);
    let why = dashboard
        .why_view(at(NOW), "model.orders", &BTreeMap::new())
        .unwrap();
    assert_eq!(why.sources[0].grade, "proxy");
    assert!(!why.sources[0].usable, "a proxy version isn't fact");
    assert!(why.evidence.iter().all(|e| !e.fact));
    assert_eq!(why.evidence[2].grade, "inferred");
    assert!(!why.fingerprint.compared);
    assert!(
        why.fingerprint.components.is_empty(),
        "nothing said about parts"
    );
    insta::assert_snapshot!(
        "why_unknown_evidence",
        serde_json::to_string_pretty(&why).unwrap()
    );

    let addr = start(dashboard);
    let (_, page) = get(addr, "/state/plan");
    assert!(page.contains(">unknown evidence</span>"), "{page}");
    assert!(page.contains(r#"<span class="st-grade inferred""#));
    assert!(page.contains("(not usable for reuse)"));
    assert!(!page.contains("Fingerprint unchanged"));
}

#[test]
fn a_plan_whose_target_changed_compares_nothing() {
    // As the planner plans state recorded in another target: nothing compared.
    let target_plan = ExecutionPlan::new(
        Some(SnapshotId(3)),
        at(NOW),
        vec![entry(
            "model.customers",
            PlanAction::Build,
            ReasonCode::TargetChanged,
            "last built in another target: prod",
            0,
        )],
    );
    let dashboard = Dashboard::new("jaffle_ods", "dev").with_state(StateInput::Recorded(Box::new(
        Recorded::new(".ods/state.db", vec![], 3, Ok(target_plan))
            .with_warnings(vec![
                "the recorded state was built in target prod, not dev: nothing in it is reused"
                    .into(),
            ])
            .with_history(History::new(snapshots())),
    )));
    let view = dashboard.plan_view(true, at(NOW), None, None, &BTreeMap::new());
    assert_eq!(view.counts.build, 1);
    assert_eq!(view.rows[0].reason, "target changed");
    let why = view.selected.unwrap();
    assert!(!why.fingerprint.compared);
    assert_eq!(
        why.fingerprint.summary,
        "not compared: the recorded state is from another target"
    );
    assert_eq!(view.warnings.len(), 1);
    let addr = start(dashboard);
    let (_, page) = get(addr, "/state/plan");
    assert!(page.contains("Fingerprint not compared: the recorded state is from another target"));
    assert!(page.contains("nothing in it is reused"));
}

#[test]
fn without_a_state_store_the_pages_say_how_to_record_a_first_run() {
    let dashboard = Dashboard::new("jaffle_ods", "dev").with_state(StateInput::NoStore {
        store: ".ods/state.db".into(),
    });
    let addr = start(dashboard);
    let (status, body) = get(addr, "/api/state/plan");
    assert_eq!(status, 200);
    insta::assert_snapshot!("plan_no_store", pretty(&body));
    let (status, body) = get(addr, "/api/state/runs");
    assert_eq!(status, 200);
    let runs: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(runs["state"], "no_store");
    assert_eq!(runs["runs"].as_array().unwrap().len(), 0);
    assert_eq!(runs["empty"]["commands"][0]["command"], "ods state build");
    for path in ["/state/plan", "/state/runs"] {
        let (status, page) = get(addr, path);
        assert_eq!(status, 200, "{path}");
        assert!(page.contains(r#"data-state="no_store""#), "{path}");
        assert!(
            page.contains(r#"data-copy="ods state build""#),
            "{path}: copyable"
        );
    }
    let (_, page) = get(addr, "/state/plan");
    assert!(page.contains("No plan to show yet"), "its own words");
    let (_, page) = get(addr, "/state/runs");
    assert!(page.contains("No runs recorded yet"));
    let (status, _) = get(addr, "/state/runs/e6f54fe3");
    assert_eq!(status, 404);
    let (status, _) = get(addr, "/api/state/runs/e6f54fe3");
    assert_eq!(status, 404);
    let (status, _) = get(addr, "/api/state/plan/model.orders");
    assert_eq!(status, 404);
}

#[test]
fn runs_list_what_the_store_records_and_nothing_more() {
    let addr = start(demo());
    let (status, body) = get(addr, "/api/state/runs");
    assert_eq!(status, 200);
    insta::assert_snapshot!("runs", pretty(&body));
    let runs: serde_json::Value = serde_json::from_str(&body).unwrap();
    let rows = runs["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["snapshot"], 3, "newest first");
    assert_eq!(rows[0]["built"], 2);
    assert_eq!(rows[0]["kept"], 1);
    // Nothing recorded says whether they failed, or what they ran.
    assert!(rows.iter().all(|r| r["outcome"] == "recorded"));
    assert!(
        rows.iter()
            .all(|r| r["failed"].is_null() && r["command"].is_null())
    );

    let (_, page) = get(addr, "/state/runs");
    assert!(page.contains("ODS doesn't schedule anything."));
    assert!(page.contains("CI runs — coming with server mode"));
    assert!(page.contains(r#"aria-disabled="true""#));
    assert!(page.contains(">Kept</th>"));
    assert!(page.contains(r#"<th scope="rowgroup" colspan="13">2026-09-29 · UTC</th>"#));
    assert!(page.contains(r#"aria-label="not recorded""#));
    assert!(page.contains("recorded: outcome not stored"));
    assert!(!page.contains(">Reused<"), "kept isn't claimed as reuse");
    assert!(
        !page.contains("✓") && !page.contains("SUCCEEDED"),
        "nor success"
    );
    assert!(page.contains(r#"href="runs/4c0b5c8f%2D0000%2D4000%2D8000%2D000000000003""#));
    assert!(page.contains("3 runs · 3 recorded a snapshot"), "{page}");
    assert!(page.contains("2026-09-29 · UTC"));
    // No journals: durations and rows say why they're missing, never a placeholder.
    assert!(!page.contains("[duration]") && page.contains("[user]"));
    assert!(page.contains("no run journal: the run predates run journals"));
    assert!(page.contains(r#"<a class="crumb" href="plan">State</a>"#));
    assert!(page.contains("Local runs · from <code"));

    // Filters, with counts.
    let (_, body) = get(addr, "/api/state/runs?outcome=failed");
    let runs: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(runs["runs"].as_array().unwrap().len(), 0);
    let dashboard = demo();
    let mut filter = RunFilter::default();
    filter.date = Some("1d".into());
    let view = dashboard.runs_view(true, at("2026-10-05T00:00:00Z"), &filter, &BTreeMap::new());
    assert!(view.runs.is_empty(), "all older than a day");
    let dates = view.facets.iter().find(|f| f.key == "date").unwrap();
    assert_eq!(dates.options[0].count, 3, "All counts every run");
    assert_eq!(dates.options[3].count, 3, "last 30 days");
}

#[test]
fn a_failed_last_run_is_tied_to_the_snapshot_that_records_its_run_id() {
    // Its run id is snapshot 3's: a partial run, whose successes were recorded.
    let dashboard = with_last(scoped(failed_last_run("2026-09-29T00:02:00Z"), Some(RUN_3)));
    let addr = start(dashboard.clone());
    let (_, body) = get(addr, "/api/state/runs");
    insta::assert_snapshot!("runs_failed_partial", pretty(&body));
    let view = dashboard.runs_view(true, at(NOW), &RunFilter::default(), &BTreeMap::new());
    let row = &view.runs[0];
    assert_eq!(row.outcome, RunOutcome::Failed);
    assert!(row.from_last_run);
    assert_eq!((row.failed, row.skipped), (Some(1), Some(1)));
    assert_eq!(view.failed, 1);
    assert_eq!(view.selected.as_deref(), Some(RUN_3));
    let last = view.last_run.as_ref().unwrap();
    assert_eq!(last.snapshot, Some(3));
    assert!(!last.recorded_nothing_inferred);
    assert_eq!(last.next[0].command, "ods state retry --failed");
    let (_, page) = get(addr, "/state/runs");
    assert!(page.contains("Failed or not recorded"));
    assert!(page.contains("ods state retry --failed"));
    assert!(
        page.contains("from the last run&#x27;s record")
            || page.contains("from the last run's record")
    );
    assert!(!page.contains("[error excerpt]"));
    assert!(page.contains("the run has no journal, which keeps each error"));
    assert!(page.contains(r#"aria-label="Show run 4c0b5c8f in the side panel, failed (from the last run's record)""#), "{page}");
    let (status, page) = get(addr, &format!("/state/runs/{RUN_3}"));
    assert_eq!(status, 200);
    assert!(
        page.contains(r#"href="../../catalog/model%2E"#),
        "a run's nodes link to their Model pages, from two levels down (#313): {page}"
    );
    assert!(
        page.contains("The last run&#x27;s record says some nodes failed")
            || page.contains("The last run's record says some nodes failed"),
        "{page}"
    );
    // Nothing else is claimed about other runs.
    let (_, page) = get(addr, &format!("/state/runs/{RUN_2}"));
    assert!(
        page.contains("Whether any node failed isn't stored")
            || page.contains("Whether any node failed isn&#x27;t stored")
    );
}

#[test]
fn a_last_run_whose_id_no_snapshot_records_probably_recorded_nothing() {
    let dashboard = with_last(scoped(
        failed_last_run("2026-09-29T00:04:10Z"),
        Some("7d21a0c4-0000-4000-8000-000000000004"),
    ));
    let view = dashboard.runs_view(false, at(NOW), &RunFilter::default(), &BTreeMap::new());
    assert!(view.last_run_listed);
    assert_eq!(view.failed, 1);
    assert_eq!(view.listed, 4);
    assert_eq!(view.selected.as_deref(), Some("last"));
    assert!(view.runs.iter().all(|r| r.outcome == RunOutcome::Recorded));
    let last = view.last_run.as_ref().unwrap();
    assert!(last.recorded_nothing_inferred, "inferred, never fact");
    assert_eq!(last.snapshot, None);
    assert_eq!(last.last_good, Some(3));
    assert_eq!(
        last.command, "ods state build",
        "no options beyond loopback"
    );
    assert_eq!(last.file, None, "no paths beyond loopback");
    let addr = start(dashboard);
    let (_, page) = get(addr, "/state/runs");
    assert!(page.contains("kept 3"), "{page}");
    assert!(page.contains("Last good state: snapshot 3"));
    assert!(page.contains(r#"<span class="st-grade inferred""#));
    assert!(page.contains("recorded nothing (inferred)"));
    assert!(
        page.contains("4 runs · 3 recorded a snapshot · 1 failed"),
        "{page}"
    );
    let (_, body) = get(addr, "/api/state/runs?outcome=recorded");
    let runs: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(runs["last_run_listed"], false, "filtered out");
    let (_, page) = get(addr, "/state/runs?outcome=recorded");
    assert!(page.contains("Showing 3 of 4 runs"), "{page}");

    // Two snapshots recorded since it started, but neither is its own: still only
    // inferred that it recorded nothing.
    let mut later = snapshots();
    for (id, s) in &mut later {
        if *id >= 2 {
            s.created_at = at("2026-09-29T00:05:00Z");
        }
    }
    let dashboard = recorded(
        History::new(later).with_last_run(Some(scoped(
            failed_last_run("2026-09-29T00:04:10Z"),
            Some("7d21a0c4-0000-4000-8000-000000000004"),
        ))),
        plan(),
    );
    let view = dashboard.runs_view(true, at(NOW), &RunFilter::default(), &BTreeMap::new());
    let last = view.last_run.as_ref().unwrap();
    assert!(last.recorded_nothing_inferred && last.snapshot.is_none());
    assert!(view.runs.iter().all(|r| !r.from_last_run));
}

#[test]
fn without_a_run_id_two_snapshots_since_it_started_leave_it_untied() {
    let mut later = snapshots();
    for (id, s) in &mut later {
        if *id >= 2 {
            s.created_at = at("2026-09-29T00:05:00Z");
        }
    }
    let dashboard = recorded(
        History::new(later)
            .with_last_run(Some(scoped(failed_last_run("2026-09-29T00:04:10Z"), None))),
        plan(),
    );
    let view = dashboard.runs_view(true, at(NOW), &RunFilter::default(), &BTreeMap::new());
    let last = view.last_run.as_ref().unwrap();
    assert_eq!(last.snapshot, None);
    assert!(!last.recorded_nothing_inferred);
    assert!(!view.last_run_listed);
    assert!(
        view.runs
            .iter()
            .all(|r| r.outcome == RunOutcome::Recorded && !r.from_last_run)
    );
    let addr = start(dashboard);
    let (_, page) = get(addr, "/state/runs");
    assert!(page.contains(r#"data-state="untied_last_run""#), "{page}");
}

#[test]
fn a_last_run_for_another_scope_is_not_shown() {
    let other = failed_last_run("2026-09-29T00:04:10Z")
        .with_run(Some("jaffle_ods/prod".into()), Some("x".into()));
    let dashboard = with_last(other);
    let view = dashboard.runs_view(true, at(NOW), &RunFilter::default(), &BTreeMap::new());
    assert!(view.last_run.is_none());
    assert!(view.unscoped_last_run.is_none());
    assert!(!view.last_run_listed);
    assert_eq!(view.failed, 0);
    let addr = start(dashboard);
    let (_, page) = get(addr, "/state/runs");
    assert!(
        !page.contains("orders_view") && !page.contains("retry --failed"),
        "{page}"
    );
}

#[test]
fn a_last_run_from_an_older_file_may_belong_to_another_target() {
    // An older file names no scope: shown apart, never tied or listed.
    let dashboard = with_last(failed_last_run("2026-09-29T00:04:10Z"));
    let view = dashboard.runs_view(true, at(NOW), &RunFilter::default(), &BTreeMap::new());
    assert!(view.last_run.is_none());
    assert!(!view.last_run_listed);
    assert!(view.runs.iter().all(|r| !r.from_last_run));
    let unscoped = view.unscoped_last_run.as_ref().unwrap();
    assert_eq!(unscoped.snapshot, None);
    assert!(!unscoped.recorded_nothing_inferred);
    let addr = start(dashboard);
    let (_, page) = get(addr, "/state/runs");
    assert!(page.contains("may belong to another target"), "{page}");
}

#[test]
fn beyond_loopback_the_pages_show_no_paths_options_or_errors() {
    let options = ServeOptions::new(([0, 0, 0, 0], 0).into());
    let dashboard = with_last(scoped(
        failed_last_run("2026-09-29T00:04:10Z"),
        Some("7d21a0c4-0000-4000-8000-000000000004"),
    ));
    let app = router(lineage().with_dashboard(dashboard), &options);
    let addr = serve_app(app);
    for path in [
        "/state/plan",
        "/state/runs",
        "/api/state/runs",
        "/api/state/plan",
        &format!("/state/runs/{RUN_3}"),
        &format!("/api/state/runs/{RUN_3}"),
    ] {
        let (status, body) = get(addr, path);
        assert_eq!(status, 200, "{path}");
        assert!(!body.contains("/home/me"), "{path}: {body}");
        assert!(!body.contains("+customers_view"), "{path}: options");
        assert!(!body.contains(".ods/state.db"), "{path}: store path");
    }
    let broken =
        Dashboard::new("p", "dev").with_state(StateInput::Recorded(Box::new(Recorded::new(
            "/home/me/.ods/state.db",
            vec![],
            0,
            Err("can't read /home/me/x".into()),
        ))));
    let addr = serve_app(router(lineage().with_dashboard(broken), &options));
    let (_, page) = get(addr, "/state/plan");
    assert!(!page.contains("/home/me"), "{page}");
    assert!(page.contains("see the server log"));
}

#[test]
fn a_run_shows_its_timeline_why_each_node_was_built_and_earlier_runs() {
    let addr = start(demo());
    let (status, body) = get(addr, &format!("/api/state/runs/{RUN_3}"));
    assert_eq!(status, 200);
    insta::assert_snapshot!("run", pretty(&body));
    let run: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(run["built"][0]["why"], "code: sql changed");
    assert_eq!(run["built"][1]["why"], "reads customers, which was rebuilt");
    assert_eq!(run["timeline"][2]["lane"], 1, "waits on customers");
    assert_eq!(run["compared_with"]["snapshot"], 2);
    assert_eq!(run["earlier"].as_array().unwrap().len(), 2);
    // A prefix of the id is enough, if only one run has it.
    let (status, _) = get(addr, "/api/state/runs/4c0b5c8f");
    assert_eq!(status, 200);
    let (status, _) = get(addr, "/api/state/runs/4c0b");
    assert_eq!(status, 404, "too short to be sure");

    let (status, page) = get(addr, &format!("/state/runs/{RUN_3}"));
    assert_eq!(status, 200);
    assert!(page.contains("run 4c0b5c8f"));
    // Without a journal, times say why they're missing: no placeholders.
    for placeholder in ["[wall clock]", "[duration]", "[start time]"] {
        assert!(!page.contains(placeholder), "{placeholder}");
    }
    assert!(page.contains("Not recorded: this run has no journal"));
    assert!(page.contains("outcome not stored: no run journal"));
    assert!(page.contains("Kept earlier build"));
    assert!(page.contains("3 (replaces 2)"));
    assert!(page.contains("Earlier runs on dev"));
    assert!(page.contains(r#"<meta name="ods-root" content="../../">"#));
    assert!(page.contains(r#"<a href="../../lineage" data-section="lineage">"#));
    assert!(page.contains(r#"<a class="crumb" href="../runs">Runs</a>"#));
    let (_, page) = get(addr, &format!("/state/runs/{RUN_1}?tab=nodes"));
    assert!(page.contains("first recorded build"));
    assert!(page.contains(r#"<th scope="col">Build kept from</th>"#));
}

#[test]
fn nothing_writes() {
    let addr = start(demo());
    for path in [
        "/state/plan",
        "/state/runs",
        &format!("/state/runs/{RUN_3}"),
        "/api/state/plan",
        "/api/state/plan/model.customers",
        "/api/state/runs",
        &format!("/api/state/runs/{RUN_3}"),
    ] {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let (status, _) = request(addr, method, path);
            assert_eq!(status, 405, "{method} {path}");
        }
    }
}

#[test]
fn hostile_names_are_escaped() {
    let evil = "<script>alert(1)</script>";
    let mut e = entry(
        "model.x",
        PlanAction::Build,
        ReasonCode::CodeChanged,
        evil,
        0,
    );
    e.name = evil.into();
    e.evidence = vec![Evidence::new(
        "source_data_version",
        evil,
        Some(evil.into()),
        Exactness::Exact,
    )];
    let plan = ExecutionPlan::new(Some(SnapshotId(1)), at(NOW), vec![e]);
    let mut n = node(evil, evil, NOW);
    n.parents.insert(evil.into(), evil.into());
    let snapshot = StateSnapshot::new(None, at(NOW), evil, BTreeMap::from([(evil.to_owned(), n)]))
        .with_target(Some(TargetIdentity::new(evil)));
    let history = History::new(vec![(1, snapshot)]).with_last_run(Some(
        LastRun::new(evil, evil, at("2026-09-29T11:00:00Z"), evil)
            .with_outcome(Some(LastOutcome::new(vec![evil.into()], vec![], vec![]))),
    ));
    let addr = start(recorded(history, plan));
    for path in [
        "/state/plan",
        "/state/plan?view=json",
        "/state/runs",
        "/state/runs/%3Cscript%3Ealert(1)%3C%2Fscript%3E",
        "/state/runs/%3Cscript%3Ealert(1)%3C%2Fscript%3E?tab=nodes",
        "/state/runs/%3Cscript%3Eunknown",
    ] {
        let (_, page) = get(addr, path);
        assert!(!page.contains(evil), "{path}: {page}");
        assert!(page.contains("&lt;script&gt;"), "{path}");
    }
}

#[test]
fn home_links_to_the_runs() {
    let addr = start(demo());
    let (_, page) = get(addr, "/");
    assert!(page.contains(r#"<a href="state/runs">All runs</a>"#));
    assert!(page.contains(r#"href="state/runs/4c0b5c8f%2D0000"#));
    assert!(page.contains(r#"<a href="state/plan" data-section="state">"#));
    let (status, _) = get(addr, "/state");
    assert_eq!(status, 307, "the State section opens on the plan");
}

#[test]
fn the_plan_is_reused_for_a_while_and_made_again_after() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    // A lag tolerance that runs out at noon: nothing on disk changes, the plan does.
    let deadline = at("2026-09-29T12:00:00Z");
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let planner: ods_web::dashboard::Planner = Arc::new(move |now| {
        counted.fetch_add(1, Ordering::SeqCst);
        let (action, code) = if now < deadline {
            (PlanAction::Reuse, ReasonCode::WithinLagTolerance)
        } else {
            (PlanAction::Build, ReasonCode::NewUpstreamData)
        };
        Ok((
            ExecutionPlan::new(
                Some(SnapshotId(3)),
                now,
                vec![entry("model.orders", action, code, "lag tolerance", 0)],
            ),
            Vec::new(),
        ))
    });
    let dashboard = Dashboard::new("p", "dev").with_state(StateInput::Recorded(Box::new(
        Recorded::new("db", vec![], 3, Err("stale".into()))
            .with_planner(planner)
            .with_history(History::new(snapshots())),
    )));
    let names = BTreeMap::new();
    let plan = |t: &str| dashboard.plan_view(true, at(t), None, None, &names);
    assert_eq!(plan("2026-09-29T11:59:31Z").counts.reuse, 1);
    assert_eq!(plan("2026-09-29T11:59:45Z").counts.reuse, 1);
    // Clones share the plan: each request clones the dashboard.
    assert_eq!(
        dashboard
            .clone()
            .home_at(true, at("2026-09-29T11:59:50Z"))
            .plan
            .unwrap()
            .reuse,
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "made once for the 30 s");
    assert_eq!(
        plan("2026-09-29T12:00:01Z").counts.build,
        1,
        "flips after the deadline"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_plan_that_builds_nothing_says_so_and_what_checks_relations() {
    let mut reused = entry(
        "model.orders",
        PlanAction::Reuse,
        ReasonCode::Unchanged,
        "unchanged",
        0,
    );
    reused.evidence = vec![Evidence::new(
        "relation_exists",
        "model.orders",
        None,
        Exactness::None,
    )];
    let plan = ExecutionPlan::new(Some(SnapshotId(3)), at(NOW), vec![reused]);
    let addr = start(recorded(History::new(snapshots()), plan));
    let (_, page) = get(addr, "/state/plan");
    assert!(page.contains("Nothing to build:"), "{page}");
    assert!(page.contains(">not checked</span>"));
    assert!(page.contains("0 of 1 reused relations · planned offline"));
    assert!(page.contains("<code>ods state build --dry-run</code> checks them"));
    let (status, _) = get(addr, "/state/");
    assert_eq!(status, 307, "the State section opens on the plan");
}

#[test]
fn a_run_whose_only_failures_are_source_tests_failed() {
    let only_sources = LastRun::new(
        "ods state build",
        "ods state build",
        at("2026-09-29T00:04:10Z"),
        "state.db.last-run.json",
    )
    .with_outcome(Some(LastOutcome::new(
        vec![],
        vec![],
        vec!["source.shop.raw.orders".into()],
    )))
    .with_retry(
        Some("ods state retry".into()),
        Some("ods state retry --failed".into()),
    );
    // It recorded nothing (no snapshot has its id): its own row, failed.
    let dashboard = with_last(scoped(
        only_sources.clone(),
        Some("7d21a0c4-0000-4000-8000-000000000004"),
    ));
    let view = dashboard.runs_view(true, at(NOW), &RunFilter::default(), &BTreeMap::new());
    assert!(view.last_run_listed);
    assert_eq!(view.failed, 1, "counted as failed");
    assert_eq!(view.selected.as_deref(), Some("last"));
    let outcomes = view.facets.iter().find(|f| f.key == "outcome").unwrap();
    let count = |value: &str| {
        outcomes
            .options
            .iter()
            .find(|o| o.value == value)
            .unwrap()
            .count
    };
    assert_eq!((count("failed"), count("succeeded")), (1, 0));
    let last = view.last_run.as_ref().unwrap();
    assert!(last.has_failures());
    assert_eq!(last.failures(), 1);
    assert_eq!(last.next[0].command, "ods state retry --failed");
    let addr = start(dashboard);
    let (_, page) = get(addr, "/state/runs");
    assert!(page.contains("Source tests failed"), "{page}");
    assert!(page.contains(">FAILED<"), "{page}");
    assert!(!page.contains(">SUCCEEDED<"), "{page}");

    // Tied to snapshot 3 by its run id: the row is failed too.
    let dashboard = with_last(scoped(only_sources, Some(RUN_3)));
    let view = dashboard.runs_view(true, at(NOW), &RunFilter::default(), &BTreeMap::new());
    assert_eq!(view.runs[0].outcome, RunOutcome::Failed);
    assert_eq!(view.runs[0].failed, Some(1));
}

// ------------------------------------------------------------------ run journals (#322)

mod journals {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use ods_core::state::TimestampMs;
    use ods_sdk::contracts::executor::ExecutionMode;
    use ods_sdk::contracts::run_events::{
        CheckStatus, ErrorSummary, NodeRunStats, NodeRunStatus, RunEvent, RunEventKind,
        RunOutcome as Ended,
    };
    use ods_sdk::run_journal::Journals;

    use super::*;

    /// A failed run that recorded no snapshot: `orders` failed while `payments` built
    /// beside it, and `orders_view` was skipped.
    pub(super) const RUN_4: &str = "7d21a0c4-0000-4000-8000-000000000004";
    /// A run of another target, sharing the state database.
    const RUN_PROD: &str = "0b0b0b0b-0000-4000-8000-00000000000b";
    /// A run whose journal doesn't say it ended.
    const RUN_5: &str = "5e5e5e5e-0000-4000-8000-000000000005";
    pub(super) const RUN_5_ID: &str = RUN_5;
    const SCOPE: &str = "jaffle_ods/dev";

    fn ms(text: &str) -> TimestampMs {
        TimestampMs::parse(text).unwrap()
    }

    fn event(run: &str, scope: &str, at: &str, kind: RunEventKind) -> RunEvent {
        RunEvent::new(run, Some(scope.to_owned()), ms(at), kind)
    }

    fn started(run: &str, scope: &str, at: &str, nodes: &[&str]) -> RunEvent {
        event(
            run,
            scope,
            at,
            RunEventKind::RunStarted {
                nodes: nodes.iter().map(|n| (*n).to_owned()).collect(),
                mode: ExecutionMode::Build,
                live: true,
            },
        )
    }

    fn node_started(run: &str, at: &str, node: &str, thread: &str) -> RunEvent {
        event(
            run,
            SCOPE,
            at,
            RunEventKind::NodeStarted {
                node: node.into(),
                thread: Some(thread.into()),
            },
        )
    }

    fn finished(run: &str, at: &str, node: &str, stats: NodeRunStats) -> RunEvent {
        event(
            run,
            SCOPE,
            at,
            RunEventKind::NodeFinished {
                node: node.into(),
                stats,
            },
        )
    }

    fn ended(run: &str, at: &str, outcome: Ended) -> RunEvent {
        event(run, SCOPE, at, RunEventKind::RunFinished { outcome })
    }

    fn write(dir: &Path, run: &str, events: &[RunEvent], tail: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{run}.jsonl"));
        let mut file = std::fs::File::create(&path).unwrap();
        for e in events {
            writeln!(file, "{}", serde_json::to_string(e).unwrap()).unwrap();
        }
        file.write_all(tail.as_bytes()).unwrap();
        path
    }

    /// Snapshot 3's run, timed: `customers` wrote 99 rows, then its view reported none.
    fn run_3(dir: &Path) {
        write(
            dir,
            RUN_3,
            &[
                started(
                    RUN_3,
                    SCOPE,
                    "2026-09-29T00:02:30.000Z",
                    &["model.customers", "model.customers_view"],
                ),
                node_started(
                    RUN_3,
                    "2026-09-29T00:02:30.500Z",
                    "model.customers",
                    "Thread-1",
                ),
                finished(
                    RUN_3,
                    "2026-09-29T00:02:34.700Z",
                    "model.customers",
                    NodeRunStats::new(NodeRunStatus::Success)
                        .with_times(
                            Some(ms("2026-09-29T00:02:30.500Z")),
                            Some(ms("2026-09-29T00:02:34.700Z")),
                        )
                        .with_durations(Some(4_200), Some(400), Some(3_800))
                        .with_rows_affected(Some(99))
                        .with_extra("query_id", "q-123"),
                ),
                event(
                    RUN_3,
                    SCOPE,
                    "2026-09-29T00:02:34.800Z",
                    RunEventKind::CheckFinished {
                        check: "test.not_null_customers_id".into(),
                        covers: vec!["model.customers".into()],
                        status: CheckStatus::Passed,
                    },
                ),
                node_started(
                    RUN_3,
                    "2026-09-29T00:02:34.800Z",
                    "model.customers_view",
                    "Thread-1",
                ),
                finished(
                    RUN_3,
                    "2026-09-29T00:02:35.900Z",
                    "model.customers_view",
                    NodeRunStats::new(NodeRunStatus::Success).with_times(
                        Some(ms("2026-09-29T00:02:34.800Z")),
                        Some(ms("2026-09-29T00:02:35.900Z")),
                    ),
                ),
                ended(RUN_3, "2026-09-29T00:02:36.000Z", Ended::Succeeded),
            ],
            "",
        );
    }

    /// The failed run: its error line is written around the writer, with a value the
    /// redaction removes, and its last line was cut short.
    fn run_4(dir: &Path) {
        let mut orders = serde_json::to_value(finished(
            RUN_4,
            "2026-09-29T00:04:13.800Z",
            "model.orders",
            NodeRunStats::new(NodeRunStatus::Error)
                .with_times(
                    Some(ms("2026-09-29T00:04:10.200Z")),
                    Some(ms("2026-09-29T00:04:13.800Z")),
                )
                .with_error(ErrorSummary::from_message("Database Error: boom")),
        ))
        .unwrap();
        orders["stats"]["error"]["message"] =
            "Database Error: division by zero where token = 'SENTINEL-4242'".into();
        orders["stats"]["thread"] = "Thread-1 'SENTINEL-4242'".into();
        let events = [
            started(
                RUN_4,
                SCOPE,
                "2026-09-29T00:04:10.000Z",
                &["model.orders", "model.payments", "model.orders_view"],
            ),
            node_started(
                RUN_4,
                "2026-09-29T00:04:10.200Z",
                "model.orders",
                "Thread-1",
            ),
            node_started(
                RUN_4,
                "2026-09-29T00:04:10.300Z",
                "model.payments",
                "Thread-2",
            ),
            finished(
                RUN_4,
                "2026-09-29T00:04:11.500Z",
                "model.payments",
                NodeRunStats::new(NodeRunStatus::Success)
                    .with_times(
                        Some(ms("2026-09-29T00:04:10.300Z")),
                        Some(ms("2026-09-29T00:04:11.500Z")),
                    )
                    .with_rows_affected(Some(12)),
            ),
        ];
        let path = write(dir, RUN_4, &events, "");
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{orders}").unwrap();
        for e in [
            finished(
                RUN_4,
                "2026-09-29T00:04:13.900Z",
                "model.orders_view",
                NodeRunStats::new(NodeRunStatus::Skipped)
                    .with_blocked_by(vec!["model.orders".into()]),
            ),
            ended(RUN_4, "2026-09-29T00:04:14.000Z", Ended::Failed),
        ] {
            writeln!(file, "{}", serde_json::to_string(&e).unwrap()).unwrap();
        }
        // A run killed mid-write leaves a line cut short.
        file.write_all(br#"{"schema_version":{"major":1,"minor":0},"run_id":"7d21"#)
            .unwrap();
    }

    /// Journals beside the demo's store: snapshot 3's run, the failed run, a run of
    /// another scope, and one that didn't say it ended, `quiet_for` ago.
    pub(super) fn dashboard(quiet_for: Duration) -> (Dashboard, PathBuf) {
        let dir = tempfile::tempdir().unwrap().keep().join("state.db.runs");
        run_3(&dir);
        run_4(&dir);
        write(
            &dir,
            RUN_PROD,
            &[
                started(
                    RUN_PROD,
                    "jaffle_ods/prod",
                    "2026-09-29T00:05:00.000Z",
                    &["model.orders"],
                ),
                ended(RUN_PROD, "2026-09-29T00:05:01.000Z", Ended::Failed),
            ],
            "",
        );
        let path = write(
            &dir,
            RUN_5,
            &[
                started(RUN_5, SCOPE, "2026-09-29T00:06:00.000Z", &["model.orders"]),
                node_started(
                    RUN_5,
                    "2026-09-29T00:06:00.100Z",
                    "model.orders",
                    "Thread-1",
                ),
            ],
            "",
        );
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - quiet_for)
            .unwrap();
        let history = History::new(snapshots()).with_journals(Journals::in_dir(&dir));
        (recorded(history, plan()), dir)
    }

    fn now() -> Timestamp {
        Timestamp::now()
    }

    #[test]
    fn runs_merge_journals_with_snapshots_newest_first_with_real_outcomes() {
        let (dashboard, _) = dashboard(Duration::from_secs(5));
        let view = dashboard.runs_view(true, now(), &RunFilter::default(), &BTreeMap::new());
        let ids: Vec<&str> = view.runs.iter().map(|r| r.run_id.as_str()).collect();
        // Another scope's run isn't listed; the failed run without a snapshot is.
        assert_eq!(ids, [super::journals::RUN_5, RUN_4, RUN_3, RUN_2, RUN_1]);
        let by = |id: &str| view.runs.iter().find(|r| r.run_id == id).unwrap();

        let unfinished = by(RUN_5);
        assert_eq!(unfinished.outcome, RunOutcome::Unfinished, "never success");
        assert_eq!(unfinished.duration, None);

        let failed = by(RUN_4);
        assert_eq!(failed.outcome, RunOutcome::Partial);
        assert_eq!(failed.snapshot, None);
        assert_eq!(failed.kept_state, Some(3), "the last good state stayed");
        assert_eq!((failed.failed, failed.skipped), (Some(1), Some(1)));
        assert_eq!(failed.duration.as_deref(), Some("4.0s"));
        let stats = failed.stats.as_ref().unwrap();
        assert_eq!(stats.unreadable_lines, 1, "the torn last line is skipped");
        // `orders` failed without reporting rows: 12 is only a lower bound.
        assert_eq!((stats.rows_affected, stats.rows_at_least), (Some(12), true));

        let ok = by(RUN_3);
        assert_eq!(ok.outcome, RunOutcome::Succeeded);
        assert_eq!(ok.snapshot, Some(3));
        assert_eq!(ok.duration.as_deref(), Some("6.0s"));
        let stats = ok.stats.as_ref().unwrap();
        assert_eq!(stats.rows, "at least 99", "the view didn't report rows");

        let pruned = by(RUN_2);
        assert_eq!(pruned.outcome, RunOutcome::Recorded);
        assert!(
            pruned
                .no_journal
                .as_deref()
                .unwrap()
                .starts_with("no run journal")
        );
        assert_eq!(pruned.duration, None);

        // A failed run is selected first, with its failures and redacted errors.
        assert_eq!(view.selected.as_deref(), Some(RUN_4));
        let failures: Vec<&str> = view
            .selected_nodes
            .iter()
            .map(|n| n.node.as_str())
            .collect();
        assert_eq!(failures, ["model.orders", "model.orders_view"]);
        assert_eq!(
            (view.failed, view.partial),
            (0, 1),
            "partial is counted apart"
        );
        assert_eq!(view.recorded_snapshots, 3);

        // The outcome filter uses the journals' outcomes.
        let only = |outcome: &str| {
            let mut filter = RunFilter::default();
            filter.outcome = Some(outcome.into());
            dashboard
                .runs_view(true, now(), &filter, &BTreeMap::new())
                .runs
                .into_iter()
                .map(|r| r.run_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(only("partial"), [RUN_4]);
        assert_eq!(only("succeeded"), [RUN_3]);
        assert_eq!(only("unfinished"), [RUN_5]);
        assert_eq!(only("recorded"), [RUN_2, RUN_1]);
        assert!(only("failed").is_empty());
        // Journal-only runs name no target: a target filter leaves them out.
        let mut filter = RunFilter::default();
        filter.target = Some("dev".into());
        assert_eq!(
            dashboard
                .runs_view(true, now(), &filter, &BTreeMap::new())
                .runs
                .len(),
            3
        );
    }

    #[test]
    fn a_run_whose_journal_went_quiet_is_unknown_not_running() {
        let (dashboard, _) = dashboard(Duration::from_secs(60 * 60));
        let view = dashboard.runs_view(true, now(), &RunFilter::default(), &BTreeMap::new());
        let row = view.runs.iter().find(|r| r.run_id == RUN_5).unwrap();
        assert_eq!(row.outcome, RunOutcome::Unknown);
        // A long node writes nothing meanwhile: that it stopped is only inferred.
        assert!(row.outcome_inferred);
        assert!(
            row.outcome_note
                .starts_with("Probably stopped: no event for over 10 minutes")
        );
        let addr = start(dashboard);
        let (_, page) = get(addr, &format!("/state/runs?run={RUN_5}"));
        assert!(page.contains("probably stopped: no event for over 10 minutes"));
        assert!(page.contains(r#"class="st-grade inferred""#));
        let (_, page) = get(addr, &format!("/state/runs/{RUN_5}"));
        assert!(page.contains("probably stopped: no event for over 10 minutes"));
    }

    #[test]
    fn a_run_page_shows_each_nodes_stats_from_its_journal() {
        let (dashboard, _) = dashboard(Duration::from_secs(5));
        let view = dashboard.run_view(true, RUN_3, &BTreeMap::new()).unwrap();
        assert_eq!(view.started_at, Some(at("2026-09-29T00:02:30Z")));
        let customers = view
            .nodes
            .iter()
            .find(|n| n.node == "model.customers")
            .unwrap();
        assert_eq!(customers.took.as_deref(), Some("4.2s"));
        assert_eq!(
            (customers.compile.as_deref(), customers.execute.as_deref()),
            (Some("400ms"), Some("3.8s"))
        );
        assert_eq!(customers.rows_affected, Some(99));
        assert_eq!(customers.start_offset_ms, Some(500));
        assert_eq!(customers.tests.unwrap().passed, 1);
        let view_node = view
            .nodes
            .iter()
            .find(|n| n.node == "model.customers_view")
            .unwrap();
        assert_eq!(view_node.rows_affected, None, "missing is never 0");
        assert_eq!(view_node.rows_missing, Some("not reported by the adapter"));
        // The timeline: the kept seed first, then by start.
        let order: Vec<&str> = view.timeline.iter().map(|t| t.node.as_str()).collect();
        assert_eq!(
            order,
            ["seed.raw_orders", "model.customers", "model.customers_view"]
        );
        assert_eq!(view.timeline[1].end_offset_ms, Some(4_700));

        let addr = start(dashboard.clone());
        let (_, page) = get(addr, &format!("/state/runs/{RUN_3}"));
        for placeholder in ["[wall clock]", "[duration]", "[start time]"] {
            assert!(!page.contains(placeholder), "{placeholder}");
        }
        assert!(page.contains(r#"class="st-value mono">6.0s<"#), "{page}");
        assert!(page.contains("at least <strong class=\"mono\">99</strong> rows"));
        assert!(page.contains(r#"<rect class="tl-bar" x="#));
        assert!(page.contains("reported as it ran"));
        let (_, page) = get(addr, &format!("/state/runs/{RUN_3}?tab=nodes"));
        assert!(page.contains("compile 400ms") && page.contains("execute 3.8s"));
        assert!(page.contains("not reported by the adapter"));
        assert!(page.contains("query_id q-123"));
        assert!(
            page.contains("code: sql changed"),
            "why it ran, from the snapshots"
        );
        assert!(page.contains(r#"data-status="kept""#));
    }

    #[test]
    fn a_failed_run_that_recorded_nothing_has_its_own_page() {
        let (dashboard, _) = dashboard(Duration::from_secs(5));
        let view = dashboard.run_view(true, RUN_4, &BTreeMap::new()).unwrap();
        assert_eq!(view.run.snapshot, None);
        assert!(
            view.state_rule
                .contains("Snapshot 3 was the last good state when it ran"),
            "{}",
            view.state_rule
        );
        assert!(
            view.state_rule
                .contains("A failed run never replaces the last good state")
        );
        // Kept: snapshot 3's nodes the run didn't run; its failed node is counted apart.
        assert_eq!(view.run.kept, Some(3));
        assert!(view.built.is_empty());
        let names: Vec<&str> = view.timeline.iter().map(|t| t.node.as_str()).collect();
        // Parallel nodes by start; the skipped one never started.
        assert_eq!(
            names,
            ["model.orders", "model.payments", "model.orders_view"]
        );
        let orders = view
            .nodes
            .iter()
            .find(|n| n.node == "model.orders")
            .unwrap();
        let error = orders.error.as_ref().unwrap();
        assert_eq!(error.kind.as_deref(), Some("Database Error"));
        assert!(!error.message.contains("SENTINEL"));
        let skipped = view
            .nodes
            .iter()
            .find(|n| n.node == "model.orders_view")
            .unwrap();
        assert_eq!(skipped.rows_missing, Some("the node didn't build"));
        assert_eq!(skipped.blocked_by[0].node, "model.orders");
        assert_eq!(view.earlier[0].run_id, RUN_3);

        let addr = start(dashboard);
        let (_, body) = get(addr, &format!("/api/state/runs/{RUN_4}"));
        insta::assert_snapshot!("run_failed_from_journal", pretty(&body));
        let (_, body) = get(addr, "/api/state/runs?outcome=partial");
        insta::assert_snapshot!("runs_from_journals", pretty(&body));
        let (status, page) = get(addr, &format!("/state/runs/{RUN_4}"));
        assert_eq!(status, 200);
        assert!(page.contains("no snapshot · started"));
        assert!(page.contains("Failed node"));
        assert!(page.contains("Database Error"));
        assert!(page.contains(r#"class="tl-bar failed""#));
        assert!(page.contains("1 line of its journal couldn"));
        let (_, page) = get(addr, "/state/runs");
        assert!(
            page.contains(
                r#"data-run="7d21a0c4-0000-4000-8000-000000000004" data-outcome="partial""#
            )
        );
        assert!(page.contains("kept 3"));
        assert!(page.contains("running or stopped without finishing"));
    }

    #[test]
    fn nothing_beyond_the_sanitized_journal_reaches_a_page_or_the_api() {
        let (dashboard, _) = dashboard(Duration::from_secs(5));
        let addr = start(dashboard);
        for path in [
            "/".to_owned(),
            "/api/home".to_owned(),
            "/state/runs".to_owned(),
            format!("/state/runs?run={RUN_4}"),
            "/api/state/runs".to_owned(),
            format!("/api/state/runs?run={RUN_4}"),
            format!("/state/runs/{RUN_4}"),
            format!("/state/runs/{RUN_4}?tab=nodes"),
            format!("/api/state/runs/{RUN_4}"),
        ] {
            let (status, body) = get(addr, &path);
            assert_eq!(status, 200, "{path}");
            assert!(!body.contains("SENTINEL"), "{path}: {body}");
        }
    }

    #[test]
    fn beyond_loopback_an_errors_log_path_isnt_shown() {
        let dir = tempfile::tempdir().unwrap();
        let journals = dir.path().join("state.db.runs");
        let with_path = finished(
            RUN_3,
            "2026-09-29T00:02:34.700Z",
            "model.customers",
            NodeRunStats::new(NodeRunStatus::Error).with_error(
                ErrorSummary::from_message("Runtime Error: boom")
                    .map(|e| e.with_details_at("logs/dbt.log")),
            ),
        );
        write(
            &journals,
            RUN_3,
            &[
                started(
                    RUN_3,
                    SCOPE,
                    "2026-09-29T00:02:30.000Z",
                    &["model.customers"],
                ),
                with_path,
            ],
            "",
        );
        let dashboard = recorded(
            History::new(snapshots()).with_journals(Journals::in_dir(&journals)),
            plan(),
        );
        let local = dashboard.run_view(true, RUN_3, &BTreeMap::new()).unwrap();
        assert_eq!(
            local.nodes[0].error.as_ref().unwrap().details_at.as_deref(),
            Some("logs/dbt.log")
        );
        let remote = dashboard.run_view(false, RUN_3, &BTreeMap::new()).unwrap();
        assert_eq!(remote.nodes[0].error.as_ref().unwrap().details_at, None);
        // It didn't say it ended, but its journal is old: unknown, never success.
        assert_ne!(remote.run.outcome, RunOutcome::Succeeded);
    }

    const RUN_7: &str = "7e7e7e7e-0000-4000-8000-000000000007";

    /// #323: a failed node is explained on the Run page's Nodes tab and in the Runs
    /// side panel, from the catalogue the binary gives, the journal and ODS's
    /// evidence: a missing column confirmed by lineage, with the commands to copy. An
    /// unrecognised error gets no cause. No value from the error gets through.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one journal, then the view model, the pages and the API, top to bottom"
    )]
    fn a_failed_node_is_explained_on_the_run_pages() {
        use ods_core::failure::{Confidence, MissingColumn};
        use ods_web::dashboard::explain::Explainer;

        let dir = tempfile::tempdir().unwrap().keep().join("state.db.runs");
        let error = |message: &str| {
            let mut stats = NodeRunStats::new(NodeRunStatus::Error)
                .with_times(
                    Some(ms("2026-09-29T00:08:01.000Z")),
                    Some(ms("2026-09-29T00:08:02.500Z")),
                )
                .with_error(
                    ErrorSummary::from_message(message).map(|e| e.with_details_at("logs/x.log")),
                );
            stats.thread = Some("Thread-1".into());
            stats
        };
        write(
            &dir,
            RUN_7,
            &[
                started(
                    RUN_7,
                    SCOPE,
                    "2026-09-29T00:08:00.000Z",
                    &[
                        "seed.raw_orders",
                        "model.customers",
                        "model.orders",
                        "model.customers_view",
                    ],
                ),
                // The upstream lineage reads was rebuilt in this run.
                finished(
                    RUN_7,
                    "2026-09-29T00:08:00.900Z",
                    "seed.raw_orders",
                    success("2026-09-29T00:08:00.900Z"),
                ),
                node_started(
                    RUN_7,
                    "2026-09-29T00:08:01.000Z",
                    "model.customers",
                    "Thread-1",
                ),
                finished(
                    RUN_7,
                    "2026-09-29T00:08:02.500Z",
                    "model.customers",
                    error("Query Error: no such column 'SENTINEL-7777' in the select list"),
                ),
                finished(
                    RUN_7,
                    "2026-09-29T00:08:02.600Z",
                    "model.orders",
                    error("Query Error: the disk <b>said</b> & \"broke\" 'SENTINEL-7777'"),
                ),
                finished(
                    RUN_7,
                    "2026-09-29T00:08:02.700Z",
                    "model.customers_view",
                    NodeRunStats::new(NodeRunStatus::Skipped)
                        .with_blocked_by(vec!["model.customers".into()]),
                ),
                ended(RUN_7, "2026-09-29T00:08:03.000Z", Ended::Failed),
            ],
            "",
        );
        let explainer = Explainer::new(std::sync::Arc::new(
            ods_provider_fake::FakeErrorCatalogue::new(),
        ))
        .with_artifacts_from(Some(RUN_7.to_owned()))
        .with_index(
            ods_sdk::contracts::error_catalogue::ProjectIndex::new(Vec::<String>::new()).with_node(
                "model.customers",
                ods_sdk::contracts::error_catalogue::IndexedNode::new("customers").in_file(
                    Some("models/marts/customers.sql"),
                    Some("target/compiled/shop/models/marts/customers.sql"),
                ),
            ),
        )
        .with_missing_columns(BTreeMap::from([(
            "model.customers".to_owned(),
            vec![MissingColumn::new(
                "seed.raw_orders",
                "first_name",
                vec!["given_name".into()],
            )],
        )]));
        let dashboard = recorded(
            History::new(snapshots())
                .with_journals(Journals::in_dir(&dir))
                .with_explainer(explainer),
            plan(),
        );
        let view = dashboard.run_view(true, RUN_7, &BTreeMap::new()).unwrap();
        let explanation_of = |node: &str| {
            view.nodes
                .iter()
                .find(|n| n.node == node)
                .unwrap()
                .explanation
                .clone()
                .unwrap()
        };
        let customers = explanation_of("model.customers");
        assert_eq!(customers.confidence(), Confidence::KnownPatternWithEvidence);
        assert_eq!(
            customers.impact().unwrap().blocked,
            ["model.customers_view"]
        );
        let orders = explanation_of("model.orders");
        assert_eq!(orders.confidence(), Confidence::NotRecognised);
        assert_eq!(orders.symptom(), None);
        assert!(
            view.nodes
                .iter()
                .all(|n| n.status == NodeRunStatus::Error || n.explanation.is_none())
        );
        insta::assert_snapshot!(
            "run_failure_explained",
            serde_json::to_string_pretty(&customers).unwrap()
        );

        // Beyond loopback: no file paths, no log path (#323 review).
        let remote = dashboard.run_view(false, RUN_7, &BTreeMap::new()).unwrap();
        let remote_json = serde_json::to_string(&remote).unwrap();
        for path in [
            "logs/x.log",
            "models/marts/customers.sql",
            "target/compiled",
        ] {
            assert!(!remote_json.contains(path), "{path}: {remote_json}");
        }
        let local = serde_json::to_string(&view).unwrap();
        assert!(local.contains("logs/x.log") && local.contains("models/marts/customers.sql"));
        let panel = dashboard.runs_view(
            false,
            now(),
            &{
                let mut f = RunFilter::default();
                f.run = Some(RUN_7.into());
                f
            },
            &BTreeMap::new(),
        );
        assert!(
            !serde_json::to_string(&panel)
                .unwrap()
                .contains("logs/x.log")
        );

        let addr = start(dashboard);
        let (_, page) = get(addr, &format!("/state/runs/{RUN_7}?tab=nodes"));
        // Engine text is escaped; the aside links to the row instead of a second card.
        assert!(page.contains("&lt;b&gt;said&lt;/b&gt; &amp;"), "{page}");
        assert!(!page.contains("<b>said"));
        assert!(page.contains(r##"<a href="#why-model.customers">Why it failed</a>"##));
        assert!(page.contains(r#"id="why-model.customers""#));
        assert!(page.contains(r#"<span class="st-src">[column lineage]</span>"#));
        assert!(!page.contains("<ol></ol>"));
        assert!(page.contains("Full text: <code>logs/<wbr>x.log</code>"));
        assert!(
            page.contains(
                r#"<div class="st-explain" data-confidence="known_pattern_with_evidence">"#
            ),
            "{page}"
        );
        assert!(page.contains("database error · missing column"));
        assert!(page.contains("Why ODS thinks so"));
        assert!(page.contains("now outputs <code>given_name</code>, not <code>first_name</code>"));
        assert!(page.contains(
            r#"data-copy="ods lineage impact --column seed.raw_orders.first_name=removed""#
        ));
        assert!(page.contains("What the fake engine said"));
        // Not recognised: facts, no cause, the engine's words shown open.
        assert!(page.contains(r#"data-confidence="not_recognised""#));
        assert!(page.contains("What ODS knows"));
        assert!(page.contains("Ask the ODS agent to investigate"));
        assert!(page.contains(r#"<details class="st-explain-said" open>"#));
        let (_, panel) = get(addr, &format!("/state/runs?run={RUN_7}"));
        assert!(
            panel.contains("A column this model reads no longer exists upstream"),
            "{panel}"
        );
        for path in [
            format!("/state/runs/{RUN_7}?tab=nodes"),
            format!("/state/runs?run={RUN_7}"),
            format!("/api/state/runs/{RUN_7}"),
            format!("/api/state/runs?run={RUN_7}"),
        ] {
            let (status, body) = get(addr, &path);
            assert_eq!(status, 200, "{path}");
            assert!(!body.contains("SENTINEL"), "{path}: {body}");
        }
    }

    /// The demo's snapshots, with only the journals `journals` writes.
    fn with(journals: impl FnOnce(&Path)) -> Dashboard {
        let dir = tempfile::tempdir().unwrap().keep().join("state.db.runs");
        std::fs::create_dir_all(&dir).unwrap();
        journals(&dir);
        recorded(
            History::new(snapshots()).with_journals(Journals::in_dir(&dir)),
            plan(),
        )
    }

    fn row_of(
        dashboard: &Dashboard,
        run: &str,
        filter: &RunFilter,
    ) -> Option<ods_web::dashboard::state::RunRow> {
        dashboard
            .runs_view(true, now(), filter, &BTreeMap::new())
            .runs
            .into_iter()
            .find(|r| r.run_id == run)
    }

    const RUN_6: &str = "6f6f6f6f-0000-4000-8000-000000000006";

    fn success(at: &str) -> NodeRunStats {
        NodeRunStats::new(NodeRunStatus::Success).with_times(Some(ms(at)), Some(ms(at)))
    }

    #[test]
    fn a_reported_success_the_nodes_dont_bear_out_is_never_shown_as_one() {
        let start = "2026-09-29T00:07:00.000Z";
        let succeeded = |nodes: Vec<RunEvent>| {
            let mut events = vec![started(RUN_6, SCOPE, start, &["model.a", "model.b"])];
            events.extend(nodes);
            events.push(ended(RUN_6, "2026-09-29T00:07:05.000Z", Ended::Succeeded));
            events
        };
        // One node never finished: its outcome is unknown, so the run's is too.
        let dashboard = with(|dir| {
            write(
                dir,
                RUN_6,
                &succeeded(vec![finished(RUN_6, start, "model.a", success(start))]),
                "",
            );
        });
        let row = row_of(&dashboard, RUN_6, &RunFilter::default()).unwrap();
        assert_eq!(row.outcome, RunOutcome::Unknown, "{}", row.outcome_note);
        assert!(row.outcome_note.contains("not read as a success"));
        let (_, page) = get(start_server(dashboard), "/state/runs");
        assert!(!page.contains(&format!(r#"data-run="{RUN_6}" data-outcome="succeeded""#)));
        // A failed node beside a success: partial, whatever the executor said.
        let dashboard = with(|dir| {
            write(
                dir,
                RUN_6,
                &succeeded(vec![
                    finished(RUN_6, start, "model.a", success(start)),
                    finished(
                        RUN_6,
                        start,
                        "model.b",
                        NodeRunStats::new(NodeRunStatus::Error),
                    ),
                ]),
                "",
            );
        });
        assert_eq!(
            row_of(&dashboard, RUN_6, &RunFilter::default())
                .unwrap()
                .outcome,
            RunOutcome::Partial
        );
        // Every node succeeded, but a line couldn't be read: unknown.
        let dashboard = with(|dir| {
            write(
                dir,
                RUN_6,
                &succeeded(vec![
                    finished(RUN_6, start, "model.a", success(start)),
                    finished(RUN_6, start, "model.b", success(start)),
                ]),
                "{not json}\n",
            );
        });
        assert_eq!(
            row_of(&dashboard, RUN_6, &RunFilter::default())
                .unwrap()
                .outcome,
            RunOutcome::Unknown
        );
    }

    fn start_server(dashboard: Dashboard) -> SocketAddr {
        super::start(dashboard)
    }

    #[test]
    fn a_failed_run_with_no_failed_node_is_failed_not_partial() {
        let start = "2026-09-29T00:07:00.000Z";
        let dashboard = with(|dir| {
            write(
                dir,
                RUN_6,
                &[
                    started(RUN_6, SCOPE, start, &["model.a", "model.b"]),
                    finished(RUN_6, start, "model.a", success(start)),
                    finished(
                        RUN_6,
                        start,
                        "model.b",
                        NodeRunStats::new(NodeRunStatus::Skipped),
                    ),
                    ended(RUN_6, "2026-09-29T00:07:05.000Z", Ended::Failed),
                ],
                "",
            );
        });
        let row = row_of(&dashboard, RUN_6, &RunFilter::default()).unwrap();
        assert_eq!(row.outcome, RunOutcome::Failed);
        assert!(
            row.outcome_note.contains("no node failed itself"),
            "{}",
            row.outcome_note
        );
    }

    #[test]
    fn a_run_that_never_said_it_started_has_no_time_and_no_rows_total() {
        let dashboard = with(|dir| {
            write(
                dir,
                RUN_6,
                &[finished(
                    RUN_6,
                    "2026-09-29T00:07:00.000Z",
                    "model.a",
                    NodeRunStats::new(NodeRunStatus::Error),
                )],
                "",
            );
        });
        let row = row_of(&dashboard, RUN_6, &RunFilter::default()).unwrap();
        assert_eq!(row.at, None, "not now: unknown");
        assert_eq!(row.kept, None);
        assert_eq!(
            row.stats.as_ref().unwrap().rows_affected,
            None,
            "no total, not 0"
        );
        let mut filter = RunFilter::default();
        filter.date = Some("30d".into());
        assert!(
            row_of(&dashboard, RUN_6, &filter).is_none(),
            "never said to be recent"
        );
    }

    #[test]
    fn a_run_that_ran_nothing_has_no_rows_total() {
        let dashboard = with(|dir| {
            write(
                dir,
                RUN_6,
                &[
                    started(RUN_6, SCOPE, "2026-09-29T00:07:00.000Z", &[]),
                    ended(RUN_6, "2026-09-29T00:07:01.000Z", Ended::Succeeded),
                ],
                "",
            );
        });
        let row = row_of(&dashboard, RUN_6, &RunFilter::default()).unwrap();
        assert_eq!(row.stats.unwrap().rows_affected, None);
        // It succeeded, yet recorded nothing: said so, not "a failed run".
        let view = dashboard.run_view(true, RUN_6, &BTreeMap::new()).unwrap();
        assert!(
            view.state_rule.contains("its builds weren't recorded"),
            "{}",
            view.state_rule
        );
        assert!(!view.state_rule.contains("A failed run"));
    }

    #[test]
    fn kept_counts_only_nodes_the_run_didnt_run() {
        // Snapshot 3's run failed its seed (kept from run 1) and built the rest.
        let dashboard = with(|dir| {
            let at = "2026-09-29T00:02:31.000Z";
            write(
                dir,
                RUN_3,
                &[
                    started(
                        RUN_3,
                        SCOPE,
                        "2026-09-29T00:02:30.000Z",
                        &["seed.raw_orders", "model.customers", "model.customers_view"],
                    ),
                    finished(
                        RUN_3,
                        at,
                        "seed.raw_orders",
                        NodeRunStats::new(NodeRunStatus::Error),
                    ),
                    finished(RUN_3, at, "model.customers", success(at)),
                    finished(RUN_3, at, "model.customers_view", success(at)),
                    ended(RUN_3, "2026-09-29T00:02:36.000Z", Ended::Failed),
                ],
                "",
            );
        });
        let row = row_of(&dashboard, RUN_3, &RunFilter::default()).unwrap();
        assert_eq!(row.outcome, RunOutcome::Partial);
        assert_eq!((row.built, row.kept, row.failed), (2, Some(0), Some(1)));
        // Without a journal the snapshot can't tell: the seed would read kept.
        let without = demo();
        let row = row_of(&without, RUN_3, &RunFilter::default()).unwrap();
        assert_eq!((row.built, row.kept), (2, Some(1)));
        let view = dashboard.run_view(true, RUN_3, &BTreeMap::new()).unwrap();
        assert!(view.state_rule.contains("some nodes failed"));
        let (_, page) = get(
            start_server(dashboard),
            &format!("/state/runs/{RUN_3}?tab=nodes"),
        );
        assert!(page.contains("kept its last good build"));
        assert!(
            page.contains(r#"<span class="pill-snap"><span class="dot warn">"#),
            "partial is amber"
        );
    }

    #[test]
    fn an_unfinished_run_that_recorded_nothing_may_still_be_running() {
        let (dashboard, _) = dashboard(Duration::from_secs(5));
        let view = dashboard
            .run_view(true, super::journals::RUN_5_ID, &BTreeMap::new())
            .unwrap();
        assert!(
            view.state_rule.contains("It may still be running"),
            "{}",
            view.state_rule
        );
        assert!(!view.state_rule.contains("A failed run"));
    }
}

/// The demo, whose catalog gives each node `link`.
fn with_links(link: &Result<RelationLink, NoRelationLink>) -> Dashboard {
    let nodes = ["seed.raw_orders", "model.customers", "model.customers_view"]
        .into_iter()
        .map(|id| {
            let mut node = CatalogNode::new(id, id.rsplit('.').next().unwrap(), "model");
            node.relation_link = RelationLinkFields::from(
                link.clone()
                    .map(|l| RelationLink::new(format!("{}/{}", l.url, node.name), l.label)),
            );
            node
        })
        .collect();
    demo().with_catalog(CatalogInput::new(nodes))
}

#[test]
fn the_nodes_table_links_each_relation_to_the_warehouse() {
    let addr = start(with_links(&Ok(RelationLink::new(
        "https://w.example/explore/data/main/jaffle",
        "Open in Catalog Explorer",
    ))));
    let (_, page) = get(addr, &format!("/state/runs/{RUN_3}?tab=nodes"));
    assert!(
        page.contains(r#"<a class="st-wh" href="https://w.example/explore/data/main/jaffle/customers" target="_blank" rel="noopener noreferrer""#),
        "{page}"
    );
    assert!(!page.contains("no_relation_link"), "{page}");
    // A past run's link comes from the current manifest, and says so.
    assert!(
        page.contains("Expected location from the current manifest, not checked"),
        "{page}"
    );
    let (_, body) = get(addr, &format!("/api/state/runs/{RUN_3}"));
    let run: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        run["relation_links"]["model.customers"]["relation_url"],
        "https://w.example/explore/data/main/jaffle/customers"
    );

    // Without links, the table says why once, and links nothing.
    let addr = start(with_links(&Err(NoRelationLink::NotConfigured {
        setting: "host".into(),
    })));
    let (_, page) = get(addr, &format!("/state/runs/{RUN_3}?tab=nodes"));
    assert!(!page.contains("data-relation-link"), "{page}");
    assert_eq!(
        page.matches(
            r#"data-state="no_relation_link">No warehouse link: <code>host</code> isn't configured</p>"#
        )
        .count(),
        1,
        "{page}"
    );
}
