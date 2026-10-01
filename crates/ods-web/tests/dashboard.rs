//! The dashboard (#310): its JSON API, the served Home page, and the facts they're
//! built from.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};

use ods_core::FreshnessPolicy;
use ods_core::state::{
    ExecutionPlan, Fingerprint, NodeState, PlanAction, PlanEntry, Reason, ReasonCode, SnapshotId,
    StateSnapshot, Timestamp,
};
use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, RelationName};
use ods_lineage::{GraphFilter, LineageNode, LineageProject, MemoryCache, NodeKind, build};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage};
use ods_web::dashboard::{
    AttentionKind, ModuleState, ModuleStatus, OpaqueNode, Planner, Recorded, RunRecord, StateInput,
    StateStatus, Target,
};
use ods_web::{Dashboard, ServeOptions, Snapshot, router};

fn rel(name: &str) -> RelationName {
    RelationName::new(["db", name]).unwrap()
}

/// A tiny lineage graph: the dashboard doesn't depend on it, but the server needs one.
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
        LineageNode::new("seed.raw_orders", rel("raw_orders"), NodeKind::Seed).with_columns(["id"]),
        LineageNode::new("model.orders", rel("orders"), NodeKind::Model)
            .with_sql("orders.sql")
            .with_depends_on(["seed.raw_orders"]),
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

/// The design's demo: three runs, then a change to `customers`.
fn recorded() -> Dashboard {
    let plan = ExecutionPlan::new(
        Some(SnapshotId(3)),
        at("2026-09-29T12:00:00Z"),
        vec![
            entry(
                "seed.raw_customers",
                PlanAction::Reuse,
                ReasonCode::Unchanged,
                "unchanged",
                0,
            ),
            entry(
                "seed.raw_orders",
                PlanAction::Build,
                ReasonCode::MissingDataEvidence,
                "no freshness evidence: will build next run",
                0,
            ),
            entry(
                "model.orders",
                PlanAction::Reuse,
                ReasonCode::Unchanged,
                "unchanged",
                1,
            ),
            entry(
                "model.customers",
                PlanAction::Build,
                ReasonCode::CodeChanged,
                &format!("code changed since run {RUN_2}: sql"),
                2,
            ),
            entry(
                "model.customer_segments",
                PlanAction::Build,
                ReasonCode::UpstreamCodeChanged,
                "upstream code changed: customers will be rebuilt",
                3,
            ),
        ],
    );
    let runs = vec![
        RunRecord::new(
            3,
            RUN_3,
            at("2026-09-29T11:56:00Z"),
            vec!["model.customer_segments".into(), "model.customers".into()],
            9,
        ),
        RunRecord::new(
            2,
            RUN_2,
            at("2026-09-28T09:00:00Z"),
            (0..8).map(|i| format!("model.m{i}")).collect(),
            3,
        ),
        RunRecord::new(
            1,
            RUN_1,
            at("2026-09-27T09:00:00Z"),
            vec![
                "seed.raw_customers".into(),
                "seed.raw_orders".into(),
                "seed.raw_payments".into(),
            ],
            0,
        ),
    ];
    Dashboard::new("jaffle_ods", "dev")
        .with_target(Some(Target::new("dev", Some("warehouse".into()))))
        .with_node_kinds(BTreeMap::from([("model".into(), 8), ("seed".into(), 3)]))
        .with_opaque(vec![OpaqueNode::new(
            "model.customer_segments",
            "customer_segments",
            "Python model: column lineage unknown",
        )])
        .with_state(StateInput::Recorded(Box::new(
            Recorded::new(".ods/state.db", runs, 3, Ok(plan))
                .with_warnings(vec!["no source freshness results".into()]),
        )))
        .with_modules(vec![
            ModuleStatus::new("Lineage (column-level)", ModuleState::Ready, None),
            ModuleStatus::new("State", ModuleState::Ready, None),
            ModuleStatus::new("Usage", ModuleState::Planned, None),
        ])
}

fn no_store() -> Dashboard {
    Dashboard::new("jaffle_ods", "default")
        .with_node_kinds(BTreeMap::from([("model".into(), 8), ("seed".into(), 3)]))
        .with_state(StateInput::NoStore {
            store: ".ods/state.db".into(),
        })
        .with_modules(vec![ModuleStatus::new(
            "State",
            ModuleState::NotSetUp,
            Some("record a first run".into()),
        )])
}

fn start(dashboard: Dashboard) -> SocketAddr {
    let options = ServeOptions::new(([127, 0, 0, 1], 0).into());
    let app = router(lineage().with_dashboard(dashboard), &options);
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

/// A minimal HTTP/1.1 request: (status, lowercased headers, body).
fn request(addr: SocketAddr, method: &str, path: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost:{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        addr.port()
    )
    .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    // Fonts are binary: only the text is compared.
    let response = String::from_utf8_lossy(&bytes);
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head.to_lowercase(), body.to_owned())
}

fn get(addr: SocketAddr, path: &str) -> (u16, String, String) {
    request(addr, "GET", path)
}

/// Pretty JSON with keys sorted, whether or not `serde_json` keeps insertion order (its
/// `preserve_order` feature is on in some builds of the workspace).
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
fn the_api_serves_home_and_the_shell_as_view_models() {
    let addr = start(recorded());
    let (status, head, body) = get(addr, "/api/home");
    assert_eq!(status, 200);
    assert!(head.contains("content-type: application/json"), "{head}");
    insta::assert_snapshot!("home_recorded", pretty(&body));
    let (status, _, body) = get(addr, "/api/shell");
    assert_eq!(status, 200);
    insta::assert_snapshot!("shell_recorded", pretty(&body));
}

#[test]
fn without_a_state_store_home_explains_how_to_record_a_first_run() {
    let addr = start(no_store());
    let (status, _, body) = get(addr, "/api/home");
    assert_eq!(status, 200, "no store is a state, not an error");
    insta::assert_snapshot!("home_no_store", pretty(&body));
    let (status, _, body) = get(addr, "/api/shell");
    assert_eq!(status, 200);
    insta::assert_snapshot!("shell_no_store", pretty(&body));

    let (status, _, page) = get(addr, "/");
    assert_eq!(status, 200);
    assert!(page.contains("No runs recorded yet"), "{page}");
    assert!(page.contains("<code>ods state build</code>"));
    assert!(page.contains(r#"Store: <code title=".ods/state.db">.ods/state.db</code>"#));
    assert!(page.contains(r#"data-state="no_store""#));
    assert!(page.contains("no snapshot yet"));
    assert!(!page.contains("Recent runs"), "no empty table");
}

#[test]
fn home_is_served_with_the_shell_and_every_panel() {
    let addr = start(recorded());
    let (status, head, page) = get(addr, "/");
    assert_eq!(status, 200);
    assert!(head.contains("content-type: text/html"), "{head}");
    assert!(
        head.contains("content-security-policy: default-src 'none'"),
        "{head}"
    );
    // The shell: every section of the design, the pickers, the header, the badge.
    for label in [
        "Home",
        "Catalog",
        "Lineage",
        "State",
        "ERD",
        "Usage",
        "CI · Impact",
        "Agent",
        "Settings",
    ] {
        assert!(
            page.contains(&format!(r#"</svg><span class="label">{label}</span>"#)),
            "{label}"
        );
    }
    assert!(page.contains(r#"<a href="./" aria-current="page" data-section="home">"#));
    assert!(page.contains(r#"<a href="lineage" data-section="lineage">"#));
    assert!(page.contains(r#"<a href="catalog" data-section="catalog">"#));
    assert!(
        page.contains(
            r#"<span class="planned" title="Planned: not built yet" data-section="usage">"#
        ),
        "planned sections link nowhere"
    );
    assert!(page.contains("dev · warehouse"));
    assert!(page.contains(r#"<span class="crumb">jaffle_ods</span>"#));
    assert!(page.contains(r#"id="search""#));
    assert!(page.contains(r#"snapshot 3 · <time datetime="2026-09-29T11:56:00Z" data-relative>"#));
    assert!(page.contains("Local · read-only"));
    assert!(page.contains("'IBM Plex Sans'"));
    assert!(page.contains("url('assets/fonts/IBMPlexSans-Regular-Latin1.woff2')"));
    assert!(page.contains("the ERD module works from the CLI"));
    assert!(page.contains(r#"<a href="state/plan" data-section="state">"#));
    assert!(page.contains("<table class=\"runs\">"));
    assert!(page.contains(r#"<meta name="ods-generation" content="1">"#));
    // Home.
    assert!(page.contains(r#"data-tile="nodes""#));
    assert!(page.contains("8 models · 3 seeds"));
    assert!(page.contains("2 built · 9 kept"));
    assert!(page.contains("Kept earlier build"));
    assert!(
        !page.contains("9 reused") && !page.contains("Reused"),
        "a kept build isn't claimed as reuse"
    );
    assert!(!page.contains("✓"), "a recorded run may have failed nodes");
    assert!(!page.contains("% of the plan skipped"));
    assert!(
        page.contains(r#"data-build="3""#),
        "the plan's builds are counted"
    );
    assert!(
        !page.contains("Nothing:"),
        "no all-clear while the plan builds"
    );
    assert!(page.contains("3 · 4c0b5c8f"));
    assert!(page.contains(r#"data-kind="changed""#));
    assert!(page.contains(r#"data-kind="unknown""#));
    assert!(page.contains(r#"data-kind="opaque""#));
    assert!(page.contains(r#"href="lineage#node=model%2Ecustomers""#));
    assert!(page.contains("Code changed since run 9ea38bd5: sql"));
    assert!(page.contains("[tested] / 8"));
    assert!(page.contains("[n]"));
    assert!(page.contains("Lineage (column-level)"));
    // No external fetches: the page must work offline and under the CSP.
    assert!(!page.contains("https://"), "no CDN or web fonts");
    assert!(!page.contains("<link"), "no external stylesheets");
}

#[test]
fn the_fonts_are_served_by_the_server_itself() {
    let addr = start(recorded());
    let (status, head, _) = get(addr, "/assets/fonts/IBMPlexMono-Medium-Latin1.woff2");
    assert_eq!(status, 200);
    assert!(head.contains("content-type: font/woff2"), "{head}");
    assert!(head.contains("font-src 'self'"), "{head}");
    assert_eq!(get(addr, "/assets/fonts/../../Cargo.toml").0, 404);
    assert_eq!(get(addr, "/assets/fonts/other.woff2").0, 404);
    // Encoded traversal: decoded, it still names no vendored font.
    for path in [
        "/assets/fonts/%2e%2e%2f%2e%2e%2fCargo.toml",
        "/assets/fonts/..%2fsrc%2flib.rs",
        "/assets/fonts/%2e%2e%2fdashboard.css",
    ] {
        assert_eq!(get(addr, path).0, 404, "{path}");
    }
    assert!(
        head.contains("cache-control: public, max-age=31536000, immutable"),
        "{head}"
    );
    let (_, head, _) = get(addr, "/api/home");
    assert!(
        head.contains("cache-control: no-store"),
        "data is never cached"
    );
}

#[test]
fn the_explorer_moves_to_lineage_next_to_home() {
    let addr = start(recorded());
    let (status, _, page) = get(addr, "/lineage");
    assert_eq!(status, 200);
    assert!(page.contains(r#"content="api""#));
    assert!(page.contains("model.orders"));
}

#[test]
fn nothing_writes() {
    let addr = start(recorded());
    for path in [
        "/",
        "/lineage",
        "/api/home",
        "/api/shell",
        "/api/version",
        "/api/graph",
    ] {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let (status, _, _) = request(addr, method, path);
            assert_eq!(status, 405, "{method} {path}");
        }
    }
}

#[test]
fn hostile_names_are_escaped() {
    let evil = "<script>alert(1)</script>";
    let dashboard = Dashboard::new(evil, "dev")
        .with_opaque(vec![OpaqueNode::new("model.x", evil, evil)])
        .with_state(StateInput::Recorded(Box::new(Recorded::new(
            "db",
            vec![RunRecord::new(
                1,
                evil,
                at("2026-09-29T11:56:00Z"),
                vec![],
                1,
            )],
            1,
            Err(evil.into()),
        ))));
    let addr = start(dashboard);
    let (_, _, page) = get(addr, "/");
    assert!(!page.contains(evil), "{page}");
    assert!(page.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
}

#[test]
fn a_run_built_the_nodes_whose_last_build_is_its_own() {
    let fingerprint = Fingerprint::from_content([("sql", "select 1")]);
    let node = |run: &str| {
        NodeState::new(
            fingerprint.clone(),
            at("2026-09-29T11:00:00Z"),
            run,
            BTreeMap::new(),
        )
    };
    let snapshot = StateSnapshot::new(
        Some(SnapshotId(1)),
        at("2026-09-29T11:56:00Z"),
        RUN_2,
        BTreeMap::from([
            ("model.a".into(), node(RUN_2)),
            ("model.b".into(), node(RUN_1)),
            ("seed.c".into(), node(RUN_1)),
        ]),
    );
    let run = RunRecord::of(2, &snapshot);
    assert_eq!(run.built, ["model.a"]);
    assert_eq!(run.kept, 2);
    assert_eq!(run.snapshot, 2);
}

#[test]
fn every_kind_of_attention_gets_a_place() {
    let plan = ExecutionPlan::new(
        Some(SnapshotId(1)),
        at("2026-09-29T12:00:00Z"),
        (0..9)
            .map(|i| {
                entry(
                    &format!("model.m{i}"),
                    PlanAction::Build,
                    ReasonCode::CodeChanged,
                    "code changed",
                    0,
                )
            })
            .collect(),
    );
    let dashboard = Dashboard::new("p", "dev")
        .with_opaque(vec![OpaqueNode::new("model.py", "py", "Python model")])
        .with_state(StateInput::Recorded(Box::new(Recorded::new(
            "db",
            vec![RunRecord::new(
                1,
                RUN_1,
                at("2026-09-29T11:56:00Z"),
                vec![],
                1,
            )],
            1,
            Ok(plan),
        ))));
    let home = dashboard.home(true);
    assert_eq!(home.attention.len(), 5);
    assert_eq!(home.attention_more, 5);
    assert_eq!(home.attention[4].kind, AttentionKind::Opaque);
}

#[test]
fn beyond_loopback_paths_and_errors_stay_in_the_log() {
    let dashboard = no_store().with_state(StateInput::Unreadable {
        store: "/home/me/.ods/state.db".into(),
        error: "file is not a database at /home/me/.ods/state.db".into(),
    });
    let local = dashboard.home(true);
    assert_eq!(local.state, StateStatus::Unreadable);
    assert!(local.empty.unwrap().message.contains("/home/me"));
    assert_eq!(local.store.unwrap().full, "/home/me/.ods/state.db");
    let remote = dashboard.home(false);
    assert_eq!(remote.store, None);
    let text = serde_json::to_string(&remote).unwrap();
    assert!(!text.contains("/home/me"), "{text}");
    assert!(text.contains("ods state doctor"));
}

/// One recorded run, and a plan whose builds have reasons the attention list doesn't
/// show.
fn builds_for_unlisted_reasons(entries: Vec<PlanEntry>) -> Dashboard {
    let plan = ExecutionPlan::new(Some(SnapshotId(1)), at("2026-09-29T12:00:00Z"), entries);
    Dashboard::new("p", "dev").with_state(StateInput::Recorded(Box::new(Recorded::new(
        "db",
        vec![RunRecord::new(
            1,
            RUN_1,
            at("2026-09-29T11:56:00Z"),
            vec![],
            2,
        )],
        1,
        Ok(plan),
    ))))
}

#[test]
fn the_all_clear_needs_a_plan_that_builds_nothing() {
    // Another target: everything builds, and none of it is "changed" or "unknown".
    let target_changed = builds_for_unlisted_reasons(vec![
        entry(
            "model.a",
            PlanAction::Build,
            ReasonCode::TargetChanged,
            "last built in another target",
            0,
        ),
        entry(
            "model.b",
            PlanAction::Build,
            ReasonCode::TargetChanged,
            "last built in another target",
            1,
        ),
        entry(
            "model.c",
            PlanAction::Build,
            ReasonCode::NeverBuilt,
            "no successful build recorded",
            1,
        ),
    ]);
    let home = target_changed.home(true);
    assert!(home.attention.is_empty(), "{:?}", home.attention);
    let plan = home.plan.as_ref().unwrap();
    assert_eq!(plan.build, 3);
    let reasons: Vec<(String, usize)> = plan
        .builds_for
        .iter()
        .map(|r| (r.label.clone(), r.count))
        .collect();
    assert_eq!(
        reasons,
        [
            ("target changed".to_owned(), 2),
            ("never built".to_owned(), 1)
        ]
    );
    let addr = start(target_changed);
    let (_, _, page) = get(addr, "/");
    assert!(!page.contains("Nothing"), "{page}");
    assert!(page.contains("The plan builds 3 nodes"), "{page}");
    assert!(page.contains("2 target changed · 1 never built"), "{page}");

    let all_reused = builds_for_unlisted_reasons(vec![entry(
        "model.a",
        PlanAction::Reuse,
        ReasonCode::Unchanged,
        "unchanged",
        0,
    )]);
    let addr = start(all_reused);
    let (_, _, page) = get(addr, "/");
    assert!(
        page.contains("Nothing: the plan reuses every node."),
        "{page}"
    );
    assert!(!page.contains("The plan builds"));
}

#[test]
fn home_plans_again_as_of_each_request() {
    // A lag tolerance that runs out at noon: nothing on disk changes, the plan does.
    let deadline = at("2026-09-29T12:00:00Z");
    let planner: Planner = std::sync::Arc::new(move |now| {
        let (action, code) = if now < deadline {
            (PlanAction::Reuse, ReasonCode::WithinLagTolerance)
        } else {
            (PlanAction::Build, ReasonCode::NewUpstreamData)
        };
        Ok((
            ExecutionPlan::new(
                Some(SnapshotId(1)),
                now,
                vec![entry("model.orders", action, code, "lag tolerance", 0)],
            ),
            Vec::new(),
        ))
    });
    let stale = Err("made at load time; must not be shown".to_owned());
    let dashboard = Dashboard::new("p", "dev").with_state(StateInput::Recorded(Box::new(
        Recorded::new(
            "db",
            vec![RunRecord::new(
                1,
                RUN_1,
                at("2026-09-29T09:00:00Z"),
                vec![],
                1,
            )],
            1,
            stale,
        )
        .with_planner(planner),
    )));
    let before = dashboard.home_at(true, at("2026-09-29T11:59:00Z"));
    let plan = before.plan.unwrap();
    assert_eq!((plan.build, plan.reuse, plan.error), (0, 1, None));
    let after = dashboard.home_at(true, at("2026-09-29T12:01:00Z"));
    let plan = after.plan.unwrap();
    assert_eq!((plan.build, plan.reuse), (1, 0));
    assert_eq!(plan.builds_for[0].label, "new upstream data");
}

#[test]
fn an_unreadable_project_does_not_blame_the_store() {
    let dashboard = Dashboard::new("project", "dev").with_state(StateInput::ProjectUnreadable {
        error: "can't read /home/me/p/target/sources.json: expected value".into(),
        hint: Some("fix or remove the source freshness results".into()),
    });
    let local = dashboard.home(true);
    assert_eq!(local.state, StateStatus::ProjectUnreadable);
    let empty = local.empty.unwrap();
    assert_eq!(empty.title, "The project can't be read");
    assert!(empty.message.contains("sources.json"));
    assert!(
        empty
            .message
            .contains("fix or remove the source freshness results")
    );
    assert!(!empty.message.contains("doctor"));
    assert_eq!(local.tiles[3].value, None, "unknown, not zero");
    let remote = serde_json::to_string(&dashboard.home(false)).unwrap();
    assert!(!remote.contains("/home/me"), "{remote}");
}
