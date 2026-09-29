//! The Lineage page and its State overlay (#312): the API, the served page, deep links,
//! and the offline page, which has neither the shell nor the overlay.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use ods_core::FreshnessPolicy;
use ods_core::state::{
    Evidence, Exactness, ExecutionPlan, PlanAction, PlanEntry, Reason, ReasonCode, SnapshotId,
    Timestamp,
};
use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, RelationName};
use ods_lineage::{GraphFilter, LineageNode, LineageProject, MemoryCache, NodeKind, build};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage};
use ods_web::dashboard::{OpaqueNode, Planner, Recorded, RunRecord, StateInput, StateStatus};
use ods_web::lineage::{Decision, RELATION_NOT_CHECKED, TRUSTED_REUSE};
use ods_web::{Dashboard, ServeOptions, Snapshot, router, standalone_page};

fn rel(name: &str) -> RelationName {
    RelationName::new(["db", name]).unwrap()
}

const RUN: &str = "9ea38bd5-0000-4000-8000-000000000002";

/// A source `raw` → seed-less `orders` (SQL) → `customers` (SQL) → `segments` (a Python
/// model: no SQL, so opaque), and a seed `countries` read by `customers`.
fn lineage() -> Snapshot {
    let identity = EdgeKind::Direct(DirectKind::Identity);
    let reads = |name: &str, from: &str| {
        QueryLineage::new(
            vec![OutputColumn::new(
                "id",
                [(ColumnRef::new(rel(from), "id"), identity)].into(),
                format!("{name}:id"),
                Confidence::Exact,
            )],
            std::collections::BTreeSet::default(),
            [rel(from)].into(),
            "rows",
            vec![],
        )
    };
    let analyzer = FakeSqlLineageAnalyzer::new()
        .with("orders.sql", reads("orders", "raw"))
        .with("customers.sql", reads("customers", "orders"));
    let project = LineageProject::new(vec![
        LineageNode::new("source.shop.raw", rel("raw"), NodeKind::Source).with_columns(["id"]),
        LineageNode::new("seed.shop.countries", rel("countries"), NodeKind::Seed)
            .with_columns(["id"]),
        LineageNode::new("model.shop.orders", rel("orders"), NodeKind::Model)
            .with_sql("orders.sql")
            .with_depends_on(["source.shop.raw"]),
        LineageNode::new("model.shop.customers", rel("customers"), NodeKind::Model)
            .with_sql("customers.sql")
            .with_depends_on(["model.shop.orders", "seed.shop.countries"]),
        LineageNode::new("model.shop.segments", rel("segments"), NodeKind::Model)
            .with_depends_on(["model.shop.customers"]),
    ]);
    let (graph, _) = build(&project, &analyzer, &MemoryCache::default()).unwrap();
    let name = |id: &str| id.rsplit('.').next().unwrap_or(id).to_owned();
    let document = graph.document(&name, &GraphFilter::default());
    Snapshot::new(document, graph, "fixture")
}

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

fn entry(id: &str, action: PlanAction, code: ReasonCode, message: &str) -> PlanEntry {
    let name = id.rsplit('.').next().unwrap();
    let kind = id.split('.').next().unwrap();
    let mut entry = PlanEntry::new(
        id,
        name,
        kind,
        action,
        vec![Reason::new(code, message)],
        FreshnessPolicy::conservative(),
        0,
    );
    if code == ReasonCode::CodeChanged {
        entry.changed_components = vec!["sql".into()];
    }
    entry
}

/// The plan after a change to `customers`; `countries` was last built in another target.
fn plan() -> ExecutionPlan {
    ExecutionPlan::new(
        Some(SnapshotId(2)),
        at("2026-09-29T12:00:00Z"),
        vec![
            entry(
                "seed.shop.countries",
                PlanAction::Build,
                ReasonCode::TargetChanged,
                "last built in another target (prod)",
            ),
            entry(
                "model.shop.orders",
                PlanAction::Reuse,
                ReasonCode::Unchanged,
                &format!("code and inputs unchanged since run {RUN}"),
            ),
            entry(
                "model.shop.customers",
                PlanAction::Build,
                ReasonCode::CodeChanged,
                &format!("code changed since run {RUN}: sql"),
            ),
            entry(
                "model.shop.segments",
                PlanAction::Build,
                ReasonCode::UpstreamCodeChanged,
                "upstream code changed: customers will be rebuilt",
            ),
        ],
    )
}

fn recorded() -> Dashboard {
    let mut run = RunRecord::new(
        2,
        RUN,
        at("2026-09-29T11:00:00Z"),
        vec![
            "model.shop.customers".into(),
            "model.shop.orders".into(),
            "model.shop.segments".into(),
        ],
        1,
    );
    run.components = BTreeMap::from([(
        "model.shop.customers".to_owned(),
        vec!["config".to_owned(), "sql".to_owned(), "upstream".to_owned()],
    )]);
    Dashboard::new("shop", "dev")
        .with_opaque(vec![OpaqueNode::new(
            "model.shop.segments",
            "segments",
            "Python model: column lineage unknown",
        )])
        .with_state(StateInput::Recorded(Box::new(
            Recorded::new(".ods/state.db", vec![run], 2, Ok(plan()))
                .with_warnings(vec!["no source freshness results".into()]),
        )))
}

fn no_store() -> Dashboard {
    Dashboard::new("shop", "default").with_state(StateInput::NoStore {
        store: ".ods/state.db".into(),
    })
}

fn start(snapshot: Snapshot) -> SocketAddr {
    let options = ServeOptions::new(([127, 0, 0, 1], 0).into());
    let app = router(snapshot, &options);
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
    let response = String::from_utf8_lossy(&bytes);
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head.to_lowercase(), body.to_owned())
}

fn get(addr: SocketAddr, path: &str) -> (u16, String, String) {
    request(addr, "GET", path)
}

/// Pretty JSON with keys sorted, whatever `serde_json`'s features.
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

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
}

#[test]
fn the_overlay_api_colours_each_node_by_the_plan() {
    let addr = start(lineage().with_dashboard(recorded()));
    let (status, head, body) = get(addr, "/api/lineage/overlay");
    assert_eq!(status, 200);
    assert!(head.contains("content-type: application/json"), "{head}");
    assert!(head.contains("cache-control: no-store"), "{head}");
    insta::assert_snapshot!("overlay_recorded", pretty(&body));
    let overlay = json(&body);
    assert_eq!(overlay["schema_version"], 1);
    let nodes = &overlay["nodes"];
    assert_eq!(nodes["model.shop.orders"]["decision"], "reuse");
    assert_eq!(nodes["model.shop.customers"]["decision"], "build");
    assert_eq!(nodes["model.shop.customers"]["summary"], "code changed");
    // A node whose target changed builds, and says why.
    assert_eq!(nodes["seed.shop.countries"]["decision"], "build");
    assert_eq!(nodes["seed.shop.countries"]["summary"], "target changed");
    // An opaque node carries why.
    assert_eq!(
        nodes["model.shop.segments"]["opaque"],
        "Python model: column lineage unknown"
    );
    // Sources are read, never built.
    assert!(nodes.get("source.shop.raw").is_none());
    assert_eq!(
        nodes["model.shop.customers"]["model_href"],
        "catalog/model.shop.customers"
    );
    assert_eq!(
        nodes["model.shop.customers"]["why_href"],
        "state/plan?node=model.shop.customers"
    );
    assert_eq!(
        nodes["model.shop.customers"]["reasons"][0]["message"],
        "Code changed since run 9ea38bd5: sql",
        "run ids read short"
    );
}

#[test]
fn without_a_state_store_every_node_is_never_built_not_an_error() {
    let addr = start(lineage().with_dashboard(no_store()));
    let (status, _, body) = get(addr, "/api/lineage/overlay");
    assert_eq!(status, 200, "no store is a state, not an error");
    insta::assert_snapshot!("overlay_no_store", pretty(&body));
    let overlay = json(&body);
    assert_eq!(overlay["state"], "no_store");
    for (id, node) in overlay["nodes"].as_object().unwrap() {
        assert_eq!(node["decision"], "never_built", "{id}");
    }
    assert_eq!(overlay["counts"]["never_built"], 4);

    let (status, _, page) = get(addr, "/lineage");
    assert_eq!(status, 200);
    assert!(
        page.contains(r#""state":"no_store""#),
        "the overlay is embedded"
    );

    // A snapshot with no dashboard at all (e.g. an embedder that only has the graph)
    // reads the same way.
    let addr = start(lineage());
    let overlay = json(&get(addr, "/api/lineage/overlay").2);
    assert_eq!(overlay["state"], "no_store");
    assert_eq!(
        overlay["nodes"]["model.shop.orders"]["decision"],
        "never_built"
    );
}

#[test]
fn the_page_sits_in_the_shell_with_the_overlay_embedded() {
    let addr = start(lineage().with_dashboard(recorded()));
    let (status, head, page) = get(addr, "/lineage");
    assert_eq!(status, 200);
    assert!(head.contains("content-type: text/html"), "{head}");
    assert!(
        head.contains("content-security-policy: default-src 'none'"),
        "{head}"
    );
    // The shell: Lineage is the current section; Home links back.
    assert!(page.contains(r#"<a href="lineage" aria-current="page" data-section="lineage">"#));
    assert!(page.contains(r#"<a href="./" data-section="home">"#));
    assert!(page.contains(r#"<span class="crumb-here">Lineage</span>"#));
    assert!(page.contains("<title>Lineage · shop · ODS</title>"));
    assert!(page.contains("Local · read-only"));
    // The explorer, served: the overlay picker and impact.
    assert!(page.contains(r#"content="api""#));
    assert!(page.contains(r#"<select id="lin-overlay""#));
    assert!(page.contains(r#"id="lin-impact""#));
    assert!(page.contains(r#"id="lin-columns""#));
    assert!(page.contains(r#"id="lin-panel""#));
    // The graph and the overlay, embedded as the first paint.
    assert!(page.contains(r#"<script type="application/json" id="ods-graph">{"#));
    assert!(page.contains(r#"<script type="application/json" id="ods-overlay">{"#));
    assert!(page.contains(r#""decision":"build","summary":"code changed""#));
    assert!(page.contains(r#""opaque":"Python model: column lineage unknown""#));
    assert!(page.contains(r#"<script type="application/json" id="ods-selected">null</script>"#));
    // The DAG's edges are between nodes, and never called relationships (rule 6).
    assert!(page.contains(
        r#"{"from":"model.shop.customers","to":"model.shop.segments","via":"declared"}"#
    ));
    let dagre = include_str!("../assets/vendor/dagre.min.js");
    assert!(
        !page
            .replace(dagre, "")
            .to_lowercase()
            .contains("relationship"),
        "the page's own text never calls an edge a relationship"
    );
    // No external fetches.
    assert!(!page.contains("https://"));
    assert!(!page.contains("<link"));
}

#[test]
fn a_deep_link_selects_a_node() {
    let addr = start(lineage().with_dashboard(recorded()));
    let (status, _, page) = get(addr, "/lineage?node=model.shop.customers");
    assert_eq!(status, 200);
    assert!(page.contains(
        r#"<script type="application/json" id="ods-selected">"model.shop.customers"</script>"#
    ));
    // Percent-encoded, as the links the overlay gives are.
    let (_, _, page) = get(addr, "/lineage?node=model%2Eshop%2Ecustomers&column=id");
    assert!(page.contains(r#"id="ods-selected">"model.shop.customers"</script>"#));
    // An unknown node is the page's to say, not an error.
    let (status, _, page) = get(addr, "/lineage?node=model.shop.nope");
    assert_eq!(status, 200);
    assert!(page.contains(r#"id="ods-selected">"model.shop.nope"</script>"#));
}

#[test]
fn nothing_on_the_page_writes() {
    let addr = start(lineage().with_dashboard(recorded()));
    for path in [
        "/lineage",
        "/lineage?node=model.shop.customers",
        "/api/lineage/overlay",
    ] {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            assert_eq!(request(addr, method, path).0, 405, "{method} {path}");
        }
    }
}

#[test]
fn hostile_names_and_links_are_escaped() {
    let evil = "</script><script>alert(1)</script>";
    let mut snapshot = lineage();
    for node in &mut snapshot.document.nodes {
        node.name = evil.to_owned();
    }
    let dashboard = Dashboard::new(evil, "dev")
        .with_opaque(vec![OpaqueNode::new("model.shop.segments", evil, evil)])
        .with_state(StateInput::Recorded(Box::new(Recorded::new(
            "db",
            vec![RunRecord::new(
                1,
                evil,
                at("2026-09-29T11:00:00Z"),
                vec![],
                1,
            )],
            1,
            Ok(ExecutionPlan::new(
                Some(SnapshotId(1)),
                at("2026-09-29T12:00:00Z"),
                vec![entry(
                    "model.shop.orders",
                    PlanAction::Build,
                    ReasonCode::CodeChanged,
                    evil,
                )],
            )),
        ))));
    let addr = start(snapshot.with_dashboard(dashboard));
    let (_, _, page) = get(
        addr,
        "/lineage?node=%3C%2Fscript%3E%3Cscript%3Ealert(1)%3C%2Fscript%3E",
    );
    assert!(!page.contains(evil), "nothing ends a script element early");
    assert!(!page.contains("<script>alert(1)"));
    assert!(page.contains(r"\u003c/script>\u003cscript>alert(1)\u003c/script>"));
    assert!(page.contains("&lt;/script&gt;&lt;script&gt;alert(1)&lt;/script&gt;"));
}

#[test]
fn missing_evidence_and_failed_plans_are_unknown_never_reuse() {
    let document = lineage().document;
    let unreadable = Dashboard::new("shop", "dev").with_state(StateInput::Unreadable {
        store: "/home/me/.ods/state.db".into(),
        error: "file is not a database at /home/me/.ods/state.db".into(),
    });
    let local = unreadable.lineage_overlay(&document, true);
    assert_eq!(local.state, StateStatus::Unreadable);
    assert!(
        local
            .nodes
            .values()
            .all(|n| n.decision == Decision::Unknown)
    );
    assert!(local.error.as_deref().unwrap().contains("/home/me"));
    // Beyond loopback, error text and paths stay in the server log.
    let remote = serde_json::to_string(&unreadable.lineage_overlay(&document, false)).unwrap();
    assert!(!remote.contains("/home/me"), "{remote}");

    let failed =
        Dashboard::new("shop", "dev").with_state(StateInput::Recorded(Box::new(Recorded::new(
            "db",
            vec![],
            0,
            Err("manifest too old at /home/me/p".into()),
        ))));
    let overlay = failed.lineage_overlay(&document, false);
    assert!(
        overlay
            .nodes
            .values()
            .all(|n| n.decision == Decision::Unknown)
    );
    assert!(
        !serde_json::to_string(&overlay)
            .unwrap()
            .contains("/home/me")
    );

    // Missing evidence builds, shown as unknown; a node the plan left out too.
    let partial =
        Dashboard::new("shop", "dev").with_state(StateInput::Recorded(Box::new(Recorded::new(
            "db",
            vec![],
            0,
            Ok(ExecutionPlan::new(
                None,
                at("2026-09-29T12:00:00Z"),
                vec![entry(
                    "model.shop.orders",
                    PlanAction::Build,
                    ReasonCode::MissingDataEvidence,
                    "no freshness evidence for raw",
                )],
            )),
        ))));
    let overlay = partial.lineage_overlay(&document, true);
    assert_eq!(overlay.state, StateStatus::NoRuns);
    assert_eq!(
        overlay.nodes["model.shop.orders"].decision,
        Decision::Unknown
    );
    assert_eq!(
        overlay.nodes["model.shop.customers"].decision,
        Decision::Unknown
    );
    assert_eq!(overlay.nodes["model.shop.customers"].summary, "not planned");
}

#[test]
fn the_overlay_plans_again_as_of_each_request() {
    // A lag tolerance that runs out at noon: nothing on disk changes, the overlay does.
    let deadline = at("2026-09-29T12:00:00Z");
    let planner: Planner = Arc::new(move |now| {
        let (action, code) = if now < deadline {
            (PlanAction::Reuse, ReasonCode::WithinLagTolerance)
        } else {
            (PlanAction::Build, ReasonCode::NewUpstreamData)
        };
        Ok((
            ExecutionPlan::new(
                Some(SnapshotId(1)),
                now,
                vec![entry("model.shop.orders", action, code, "lag tolerance")],
            ),
            Vec::new(),
        ))
    });
    let dashboard = Dashboard::new("shop", "dev").with_state(StateInput::Recorded(Box::new(
        Recorded::new(
            "db",
            vec![RunRecord::new(
                1,
                RUN,
                at("2026-09-29T09:00:00Z"),
                vec![],
                1,
            )],
            1,
            Err("made at load time; must not be shown".into()),
        )
        .with_planner(planner),
    )));
    let document = lineage().document;
    let before = dashboard.lineage_overlay_at(&document, true, at("2026-09-29T11:59:00Z"));
    assert_eq!(before.nodes["model.shop.orders"].decision, Decision::Reuse);
    assert_eq!(before.error, None);
    let after = dashboard.lineage_overlay_at(&document, true, at("2026-09-29T12:01:00Z"));
    assert_eq!(after.nodes["model.shop.orders"].decision, Decision::Build);
    assert_eq!(
        after.nodes["model.shop.orders"].summary,
        "new upstream data"
    );
}

#[test]
fn the_offline_page_has_the_explorer_without_the_shell_or_the_overlay() {
    let page = standalone_page(&lineage().document).unwrap();
    assert!(page.contains(r#"<meta name="ods-source" content="embedded">"#));
    assert!(page.contains(r#"id="lin-panel""#), "the same explorer");
    assert!(page.contains("State overlay and impact need <code>ods serve</code>"));
    assert!(!page.contains(r#"<nav class="side""#), "no shell");
    assert!(!page.contains(r#"id="ods-overlay""#), "no overlay");
    assert!(!page.contains(r#"id="lin-overlay""#), "no overlay picker");
    assert!(
        !page.contains(r#"id="lin-impact""#),
        "impact needs the server"
    );
    assert!(!page.contains("assets/fonts/"), "no font files to fetch");
    assert!(!page.contains("__ODS_"), "every placeholder is replaced");
}

#[test]
fn reuse_never_claims_a_relation_check_the_page_did_not_make() {
    let document = lineage().document;
    let overlay = recorded().lineage_overlay(&document, true);
    let orders = &overlay.nodes["model.shop.orders"];
    assert_eq!(orders.decision, Decision::Reuse);
    assert_eq!(orders.relation.as_deref(), Some(RELATION_NOT_CHECKED));
    assert!(
        overlay.warnings.iter().any(|w| w == TRUSTED_REUSE),
        "{:?}",
        overlay.warnings
    );
    // Nothing the page says about reuse claims the warehouse was checked.
    let addr = start(lineage().with_dashboard(recorded()));
    let (_, _, page) = get(addr, "/lineage");
    let (_, _, api) = get(addr, "/api/lineage/overlay");
    for text in [page.as_str(), api.as_str()] {
        assert!(
            !text.contains("relation checked"),
            "claims a relation check"
        );
        assert!(!text.contains("unchanged, relation"));
    }
    assert!(
        page.contains("relation not checked here"),
        "the legend says so"
    );

    // Where the plan did check, it says what it found, and there is no warning.
    let mut checked = entry(
        "model.shop.orders",
        PlanAction::Reuse,
        ReasonCode::WithinLagTolerance,
        "new upstream data, within its lag tolerance",
    );
    checked.evidence = vec![Evidence::new(
        "relation_exists",
        "model.shop.orders",
        Some("table".into()),
        Exactness::Exact,
    )];
    let dashboard =
        Dashboard::new("shop", "dev").with_state(StateInput::Recorded(Box::new(Recorded::new(
            "db",
            vec![],
            0,
            Ok(ExecutionPlan::new(
                None,
                at("2026-09-29T12:00:00Z"),
                vec![checked],
            )),
        ))));
    let overlay = dashboard.lineage_overlay(&document, true);
    let orders = &overlay.nodes["model.shop.orders"];
    assert_eq!(
        orders.relation.as_deref(),
        Some("checked: in the warehouse (table)")
    );
    // Reuse isn't always "unchanged": the summary gives the reason.
    assert_eq!(orders.summary, "within lag tolerance");
    assert!(!overlay.warnings.iter().any(|w| w == TRUSTED_REUSE));
}

#[test]
fn the_fingerprint_step_lists_changed_and_unchanged_components() {
    let overlay = recorded().lineage_overlay(&lineage().document, true);
    let customers = &overlay.nodes["model.shop.customers"];
    let components: Vec<(&str, bool)> = customers
        .components
        .iter()
        .map(|c| (c.name.as_str(), c.changed))
        .collect();
    assert_eq!(
        components,
        [("sql", true), ("config", false), ("upstream", false)]
    );
    // Nothing recorded to compare with: nothing listed.
    assert!(overlay.nodes["seed.shop.countries"].components.is_empty());
}
