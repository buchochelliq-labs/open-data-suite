//! The Freshness evidence screen (#350): its view model, JSON API and HTML, over a
//! project with a source whose table version is exact (as a Delta capability gives
//! one), a source with no evidence at all, and a seed.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};

use ods_core::FreshnessPolicy;
use ods_core::state::{
    DataVersion, Evidence, Exactness, ExecutionPlan, Fingerprint, NodeState, PlanAction, PlanEntry,
    Reason, ReasonCode, SnapshotId, StateSnapshot, Timestamp,
};
use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, RelationName};
use ods_lineage::{GraphFilter, LineageNode, LineageProject, MemoryCache, NodeKind, build};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage};
use ods_web::catalog::{CatalogInput, CatalogNode, Decision};
use ods_web::dashboard::state::History;
use ods_web::dashboard::{Recorded, RunRecord, StateInput};
use ods_web::freshness::{FreshnessInput, FreshnessView, Grade, InputKind, SourceInput};
use ods_web::{Dashboard, ServeOptions, Snapshot, router};

const RUN_1: &str = "e6f54fe3-0000-4000-8000-000000000001";
const RUN_2: &str = "9ea38bd5-0000-4000-8000-000000000002";
const SEED: &str = "seed.shop.raw_orders";
const EVENTS: &str = "source.shop.app.events";
const CLICKS: &str = "source.shop.app.clicks";
const HOSTILE: &str = "</script><img src=x onerror=alert(1)>";

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

fn rel(name: &str) -> RelationName {
    RelationName::new(["db", name]).unwrap()
}

fn seed_digest() -> Fingerprint {
    Fingerprint::from_content([("file", "id\n1\n")])
}

/// The lineage graph the server is started with; only its nodes matter here.
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
        LineageNode::new(SEED, rel("raw_orders"), NodeKind::Seed).with_columns(["id"]),
        LineageNode::new("model.shop.orders", rel("orders"), NodeKind::Model)
            .with_sql("orders.sql")
            .with_depends_on([SEED]),
    ]);
    let (graph, _) = build(&project, &analyzer, &MemoryCache::default()).unwrap();
    let name = |id: &str| id.rsplit('.').next().unwrap_or(id).to_owned();
    let document = graph.document(&name, &GraphFilter::default());
    Snapshot::new(document, graph, "fixture")
}

/// `raw_orders` (a seed) and `app.events` feed `orders`, which feeds `customers`;
/// `app.clicks` feeds `clicks_daily`.
fn nodes() -> Vec<CatalogNode> {
    let mut seed = CatalogNode::new(SEED, "raw_orders", "seed");
    seed.file = Some("seeds/raw_orders.csv".into());
    seed.relation = Some("db.raw_orders".into());
    let mut orders = CatalogNode::new("model.shop.orders", "orders", "model");
    orders.depends_on = vec![SEED.into(), EVENTS.into()];
    let mut customers = CatalogNode::new("model.shop.customers", HOSTILE, "model");
    customers.depends_on = vec!["model.shop.orders".into()];
    let mut clicks = CatalogNode::new("model.shop.clicks_daily", "clicks_daily", "model");
    clicks.depends_on = vec![CLICKS.into()];
    vec![seed, orders, customers, clicks]
}

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

/// The plan: the seed is reused on its checksum; `orders` builds on new data in
/// `app.events`, and `clicks_daily` because `app.clicks` has no version.
fn plan() -> ExecutionPlan {
    let mut seed = entry(
        SEED,
        PlanAction::Reuse,
        ReasonCode::Unchanged,
        &format!("code and inputs unchanged since run {RUN_1}"),
        0,
    );
    seed.evidence = vec![Evidence::new(
        "fingerprint",
        SEED,
        Some(seed_digest().digest),
        Exactness::Exact,
    )];
    ExecutionPlan::new(
        Some(SnapshotId(2)),
        at("2026-09-29T12:00:00Z"),
        vec![
            seed,
            entry(
                "model.shop.orders",
                PlanAction::Build,
                ReasonCode::NewUpstreamData,
                "new data in app.events",
                1,
            ),
            entry(
                "model.shop.clicks_daily",
                PlanAction::Build,
                ReasonCode::MissingDataEvidence,
                "no usable data version for app.clicks",
                0,
            ),
            entry(
                "model.shop.customers",
                PlanAction::Build,
                ReasonCode::NewUpstreamData,
                "orders is built with new data",
                2,
            ),
        ],
    )
}

/// Snapshot 2: what `orders` and the seed were last built from.
fn snapshot() -> StateSnapshot {
    let orders = NodeState::new(
        Fingerprint::from_content([("sql", "select 1")]),
        at("2026-09-28T09:00:00Z"),
        RUN_2,
        BTreeMap::from([(
            EVENTS.to_owned(),
            Some(DataVersion::new("41", Exactness::Exact, "table version")),
        )]),
    );
    let clicks = NodeState::new(
        Fingerprint::from_content([("sql", "select 2")]),
        at("2026-09-28T09:00:00Z"),
        RUN_2,
        BTreeMap::from([(CLICKS.to_owned(), None)]),
    );
    let seed = NodeState::new(
        seed_digest(),
        at("2026-09-27T09:00:00Z"),
        RUN_1,
        BTreeMap::new(),
    );
    StateSnapshot::new(
        Some(SnapshotId(1)),
        at("2026-09-28T09:00:00Z"),
        RUN_2,
        BTreeMap::from([
            ("model.shop.orders".to_owned(), orders),
            ("model.shop.clicks_daily".to_owned(), clicks),
            (SEED.to_owned(), seed),
        ]),
    )
}

fn sources() -> FreshnessInput {
    FreshnessInput::new(vec![
        SourceInput::new(EVENTS, "app.events")
            .with_relation(Some("db.app.events".into()))
            .measured_with(Some("table version".into()))
            .with_version(
                Some(DataVersion::new("42", Exactness::Exact, "table version")),
                Some(at("2026-09-29T11:00:00Z")),
                vec![Evidence::new(
                    "version_source",
                    EVENTS,
                    Some("table version, newer than sources.json".into()),
                    Exactness::Exact,
                )],
            ),
        SourceInput::new(CLICKS, "app.clicks"),
    ])
    .measured(
        Some(at("2026-09-29T11:00:00Z")),
        Some("sources.json".into()),
    )
}

fn recorded() -> Dashboard {
    let runs = vec![RunRecord::new(
        2,
        RUN_2,
        at("2026-09-28T09:00:00Z"),
        vec!["model.shop.orders".into()],
        1,
    )];
    let names = BTreeMap::from([
        (EVENTS.to_owned(), "app.events".to_owned()),
        (CLICKS.to_owned(), "app.clicks".to_owned()),
    ]);
    Dashboard::new("shop", "dev")
        .with_state(StateInput::Recorded(Box::new(
            Recorded::new(".ods/state.db", runs, 2, Ok(plan()))
                .with_history(History::new(vec![(2, snapshot())])),
        )))
        .with_catalog(CatalogInput::new(nodes()).with_names(names))
        .with_freshness(sources())
}

fn no_store() -> Dashboard {
    Dashboard::new("shop", "dev")
        .with_state(StateInput::NoStore {
            store: ".ods/state.db".into(),
        })
        .with_catalog(CatalogInput::new(nodes()))
        .with_freshness(sources())
}

fn view(dashboard: &Dashboard) -> FreshnessView {
    dashboard.freshness_at(&lineage().document, true, at("2026-09-29T12:00:00Z"))
}

fn input<'a>(view: &'a FreshnessView, id: &str) -> &'a ods_web::freshness::InputView {
    view.inputs.iter().find(|i| i.id == id).unwrap()
}

#[test]
fn sources_then_seeds_each_with_its_evidence() {
    let view = view(&recorded());
    let order: Vec<(&str, InputKind)> = view
        .inputs
        .iter()
        .map(|i| (i.name.as_str(), i.kind))
        .collect();
    assert_eq!(
        order,
        [
            ("app.clicks", InputKind::Source),
            ("app.events", InputKind::Source),
            ("raw_orders", InputKind::Seed),
        ]
    );
    assert_eq!((view.sources, view.seeds), (2, 1));
    assert_eq!(view.decisions.based_on, Some(2));

    // An exact table version now, and the one `orders` was last built from.
    let events = input(&view, EVENTS);
    let evidence = events.evidence.as_ref().unwrap();
    assert_eq!(evidence.grade, Grade::Exact);
    assert_eq!(evidence.value.as_deref(), Some("42"));
    assert_eq!(evidence.observed_at, Some(at("2026-09-29T11:00:00Z")));
    assert!(evidence.notes.iter().any(|n| n == "from table version"));
    assert!(
        evidence
            .notes
            .iter()
            .any(|n| n.contains("newer than sources.json"))
    );
    assert_eq!(events.recorded.len(), 1);
    assert_eq!(events.recorded[0].value.as_deref(), Some("41"));
    assert_eq!(events.recorded[0].grade, Grade::Exact);
    assert_eq!(events.recorded[0].run_id, "9ea38bd5");
    let readers: Vec<(&str, Decision)> = events
        .readers
        .iter()
        .map(|r| (r.node.name.as_str(), r.decision.decision))
        .collect();
    assert_eq!(readers, [("orders", Decision::Build)]);
    assert_eq!(events.readers[0].decision.summary, "new data in app.events");
    assert_eq!(
        events.readers[0].policy.as_deref(),
        Some("rebuild on any new data (ODS default)")
    );
    let downstream: Vec<&str> = events.downstream.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(downstream, ["model.shop.customers", "model.shop.orders"]);

    // No evidence at all: unknown, and its reader builds (AGENTS rule 3).
    let clicks = input(&view, CLICKS);
    let evidence = clicks.evidence.as_ref().unwrap();
    assert_eq!(evidence.grade, Grade::Unknown);
    assert_eq!(evidence.value, None);
    assert!(evidence.method.starts_with("nothing"));
    assert_eq!(clicks.recorded.len(), 1);
    assert_eq!(clicks.recorded[0].grade, Grade::Unknown);
    assert_eq!(clicks.readers[0].decision.decision, Decision::Build);

    // The seed: its file's checksum, the same as when it was built, so reused.
    let seed = input(&view, SEED);
    let evidence = seed.evidence.as_ref().unwrap();
    assert_eq!(evidence.method, "file checksum");
    assert_eq!(evidence.grade, Grade::Exact);
    assert_eq!(
        evidence.value.as_deref(),
        seed.recorded[0].value.as_deref(),
        "the checksum now is the one recorded"
    );
    assert_eq!(seed.decision.as_ref().unwrap().decision, Decision::Reuse);
    assert_eq!(seed.file.as_deref(), Some("seeds/raw_orders.csv"));
    assert_eq!(seed.href.as_deref(), Some("catalog/seed.shop.raw_orders"));

    assert_eq!(
        view.summary,
        "1 of 2 sources lack evidence good enough to reuse on: their readers build. 1 of 1 seed match their last build and are reused."
    );
}

#[test]
fn without_a_store_nothing_is_compared_and_unknown_stays_unknown() {
    let view = view(&no_store());
    let seed = input(&view, SEED);
    assert_eq!(seed.evidence, None, "nothing to compare the file with");
    assert_eq!(seed.recorded.len(), 0);
    assert_eq!(seed.decision, None);
    // A source's version is known whether or not anything was built from it.
    let events = input(&view, EVENTS);
    assert_eq!(events.evidence.as_ref().unwrap().grade, Grade::Exact);
    assert_eq!(events.recorded.len(), 0);
    assert_eq!(
        input(&view, CLICKS).evidence.as_ref().unwrap().grade,
        Grade::Unknown
    );
    assert!(view.summary.contains("Without a plan"), "{}", view.summary);
}

#[test]
fn a_version_only_runs_read_is_named_but_not_used() {
    // The warehouse keeps a history runs read versions from; the server never connects,
    // so its plan has no version now, and the screen says why.
    let dashboard = recorded().with_freshness(FreshnessInput::new(vec![
        SourceInput::new(CLICKS, "app.clicks").read_by_runs(Some("table version".into())),
    ]));
    let view = view(&dashboard);
    let evidence = input(&view, CLICKS).evidence.clone().unwrap();
    assert_eq!(evidence.method, "table version (read when a run starts)");
    assert_eq!(evidence.grade, Grade::Unknown, "nothing read here");
    assert!(
        evidence
            .notes
            .iter()
            .any(|n| n.starts_with("not read here: this screen doesn't connect")),
        "{:?}",
        evidence.notes
    );
}

#[test]
fn beyond_loopback_no_file_path_is_shown() {
    let view = recorded().freshness_at(&lineage().document, false, at("2026-09-29T12:00:00Z"));
    assert_eq!(input(&view, SEED).file, None);
    assert_eq!(
        input(&view, SEED).relation.as_deref(),
        Some("db.raw_orders")
    );
}

#[test]
fn only_exact_and_semantic_grades_reuse() {
    let view = view(&recorded());
    let reuse: Vec<(&str, bool)> = view
        .grades
        .iter()
        .map(|g| (g.grade.key(), g.allows_reuse))
        .collect();
    assert_eq!(
        reuse,
        [
            ("exact", true),
            ("semantic", true),
            ("proxy", false),
            ("inferred", false),
            ("unknown", false),
        ]
    );
}

fn start(dashboard: Dashboard) -> SocketAddr {
    let app = router(
        lineage().with_dashboard(dashboard),
        &ServeOptions::new(([127, 0, 0, 1], 0).into()),
    );
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

fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n",
        addr.port()
    )
    .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let response = String::from_utf8_lossy(&bytes);
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (
        head.split(' ').nth(1).unwrap().parse().unwrap(),
        body.to_owned(),
    )
}

#[test]
fn the_api_serves_the_view() {
    let addr = start(recorded());
    let (status, body) = get(addr, "/api/catalog/sources");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["schema_version"].as_u64().is_some());
    let ids: Vec<&str> = json["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [CLICKS, EVENTS, SEED]);
    assert_eq!(json["inputs"][1]["evidence"]["grade"], "exact");
    assert_eq!(json["inputs"][0]["evidence"]["grade"], "unknown");
    assert_eq!(json["measured_by"], "sources.json");
}

#[test]
fn the_page_lists_inputs_links_readers_and_escapes_names() {
    let addr = start(recorded());
    let (status, page) = get(addr, "/catalog/sources");
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("<title>Freshness evidence"), "{page}");
    for id in [CLICKS, EVENTS, SEED] {
        assert!(page.contains(&format!(r#"data-input="{id}""#)), "{id}");
    }
    assert!(page.contains(r#"<td data-grade="exact">"#));
    assert!(page.contains(r#"<td data-grade="unknown">"#));
    // From `catalog/sources`, every link climbs one level.
    assert!(page.contains(r#"href="../catalog/seed.shop.raw_orders""#));
    assert!(page.contains(r#"href="../state/plan?node=model.shop.orders""#));
    assert!(page.contains(r#"href="../state/plan""#));
    assert!(page.contains(r#"href="../api/catalog/sources""#));
    // A downstream node's name never reaches the page raw.
    assert!(!page.contains(HOSTILE));
    assert!(page.contains("&lt;/script&gt;"));
    // Sources are declared, so no note says otherwise.
    assert!(!page.contains(r#"data-note="no-sources""#));
    // The navigation links the screen, and marks the Catalog as where it is.
    let (_, catalog) = get(addr, "/catalog");
    assert!(
        catalog.contains(r#"href="catalog/sources""#),
        "the nav links it"
    );
}

#[test]
fn a_project_without_sources_says_so() {
    let dashboard = recorded().with_freshness(FreshnessInput::default());
    let addr = start(dashboard);
    let (status, page) = get(addr, "/catalog/sources");
    assert_eq!(status, 200);
    assert!(page.contains(r#"data-note="no-sources""#));
    assert!(page.contains(r#"data-input="seed.shop.raw_orders""#));
}
