//! The Catalog and the model pages (#313): their JSON API, their HTML, and the facts
//! they're built from.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};

use ods_core::FreshnessPolicy;
use ods_core::state::{
    ExecutionPlan, PlanAction, PlanEntry, Reason, ReasonCode, SnapshotId, Timestamp,
};
use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, RelationName};
use ods_lineage::{GraphFilter, LineageNode, LineageProject, MemoryCache, NodeKind, build};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLink, RelationLinkFields};
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage};
use ods_web::catalog::{
    CatalogColumn, CatalogInput, CatalogNode, CatalogQuery, CatalogTest, ColumnSource, LastBuild,
    TestKind, TypeSource,
};
use ods_web::dashboard::{Recorded, RunRecord, StateInput};
use ods_web::{Dashboard, ServeOptions, Snapshot, router};

const RUN_1: &str = "e6f54fe3-0000-4000-8000-000000000001";
const RUN_2: &str = "9ea38bd5-0000-4000-8000-000000000002";

/// Code with a closing script tag, which must never reach the page raw.
const HOSTILE_SQL: &str = "select '</script><script>alert(1)</script>' as x";
const HOSTILE_NAME: &str = "</script><img src=x onerror=alert(1)>";

fn rel(name: &str) -> RelationName {
    RelationName::new(["db", name]).unwrap()
}

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

/// The lineage graph: `orders` is parsed from `raw_orders`; `customers`' lineage is
/// only inferred.
fn lineage() -> Snapshot {
    let inferred = QueryLineage::new(
        vec![OutputColumn::new(
            "total",
            [(
                ColumnRef::new(rel("orders"), "id"),
                EdgeKind::Direct(DirectKind::Identity),
            )]
            .into(),
            "id",
            Confidence::Inferred,
        )],
        std::collections::BTreeSet::default(),
        [rel("orders")].into(),
        "rows",
        vec![],
    );
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
    let analyzer = FakeSqlLineageAnalyzer::new()
        .with("orders.sql", lineage)
        .with("customers.sql", inferred);
    let project = LineageProject::new(vec![
        LineageNode::new("seed.shop.raw_orders", rel("raw_orders"), NodeKind::Seed)
            .with_columns(["id"]),
        LineageNode::new("model.shop.orders", rel("orders"), NodeKind::Model)
            .with_sql("orders.sql")
            .with_depends_on(["seed.shop.raw_orders"]),
        LineageNode::new("model.shop.customers", rel("customers"), NodeKind::Model)
            .with_sql("customers.sql")
            .with_depends_on(["model.shop.orders"]),
    ]);
    let (graph, _) = build(&project, &analyzer, &MemoryCache::default()).unwrap();
    let name = |id: &str| id.rsplit('.').next().unwrap_or(id).to_owned();
    let document = graph.document(&name, &GraphFilter::default());
    Snapshot::new(document, graph, "fixture")
}

fn nodes() -> Vec<CatalogNode> {
    let mut raw = CatalogNode::new("seed.shop.raw_orders", "raw_orders", "seed");
    raw.materialization = Some("seed".into());
    raw.columns = vec![CatalogColumn::new("id")];

    let mut orders = CatalogNode::new("model.shop.orders", "orders", "model");
    orders.language = Some("sql".into());
    orders.layer = Some("staging".into());
    orders.materialization = Some("view".into());
    orders.tags = vec!["core".into()];
    orders.description = Some("One row per order.".into());
    orders.relation = Some("db.orders".into());
    orders.file = Some("models/staging/orders.sql".into());
    orders.depends_on = vec![
        "seed.shop.raw_orders".into(),
        "source.shop.app.events".into(),
    ];
    orders.code = Some("select id from {{ ref('raw_orders') }}".into());
    let mut id = CatalogColumn::new("id");
    id.data_type = Some(("BIGINT".into(), TypeSource::Warehouse));
    id.description = Some("The order.".into());
    id.constraints = vec!["not_null".into()];
    id.listed_by = vec![ColumnSource::Declared, ColumnSource::WarehouseCatalog];
    let mut status = CatalogColumn::new("status");
    status.data_type = Some(("varchar".into(), TypeSource::Declared));
    status.listed_by = vec![ColumnSource::Declared];
    // Only the warehouse catalog lists it, and the code's lineage doesn't have it.
    let mut legacy = CatalogColumn::new("legacy");
    legacy.data_type = Some(("TEXT".into(), TypeSource::Warehouse));
    legacy.listed_by = vec![ColumnSource::WarehouseCatalog];
    orders.columns = vec![id, status, CatalogColumn::new("amount"), legacy];
    orders.tests = vec![
        CatalogTest::new(
            "test.shop.unique_orders_id.1",
            "unique",
            Some("ID".into()),
            TestKind::Data,
        )
        .covered(true),
        CatalogTest::new(
            "unit_test.shop.orders.totals",
            "totals",
            None,
            TestKind::Unit,
        )
        .covered(true),
        // Attached here, but not one of the checks the record vouches for.
        CatalogTest::new("test.shop.elsewhere.3", "elsewhere", None, TestKind::Data),
    ];

    let mut customers = CatalogNode::new("model.shop.customers", HOSTILE_NAME, "model");
    customers.language = Some("python".into());
    customers.layer = Some("marts".into());
    customers.materialization = Some("table".into());
    customers.depends_on = vec!["model.shop.orders".into()];
    customers.code = Some(HOSTILE_SQL.into());
    let mut total = CatalogColumn::new("total");
    total.listed_by = vec![ColumnSource::Declared];
    customers.columns = vec![total];

    vec![customers, raw, orders]
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

fn plan() -> ExecutionPlan {
    ExecutionPlan::new(
        Some(SnapshotId(2)),
        at("2026-09-29T12:00:00Z"),
        vec![
            entry(
                "seed.shop.raw_orders",
                PlanAction::Reuse,
                ReasonCode::Unchanged,
                &format!("code and inputs unchanged since run {RUN_1}"),
                0,
            ),
            {
                let mut orders = entry(
                    "model.shop.orders",
                    PlanAction::Build,
                    ReasonCode::CodeChanged,
                    &format!("code changed since run {RUN_2}: sql"),
                    1,
                );
                orders.changed_components = vec!["sql".into()];
                orders
            },
            entry(
                "model.shop.customers",
                PlanAction::Build,
                ReasonCode::NeverBuilt,
                "no successful build recorded",
                2,
            ),
        ],
    )
}

fn catalog_input() -> CatalogInput {
    CatalogInput::new(nodes())
        .with_layer_source("the folder under the model paths")
        .with_names(BTreeMap::from([(
            "source.shop.app.events".to_owned(),
            "app.events".to_owned(),
        )]))
        .with_warehouse_as_of(Some("2026-09-28T08:00:00Z".into()))
        .with_last_builds(BTreeMap::from([
            (
                "seed.shop.raw_orders".to_owned(),
                LastBuild::new(Some(1), RUN_1, at("2026-09-27T09:00:00Z")),
            ),
            (
                "model.shop.orders".to_owned(),
                LastBuild::new(Some(2), RUN_2, at("2026-09-28T09:00:00Z")).with_tested(
                    RUN_2,
                    at("2026-09-28T09:05:00Z"),
                    Some("digest".into()),
                    true,
                ),
            ),
        ]))
}

fn recorded() -> Dashboard {
    let runs = vec![
        RunRecord::new(
            2,
            RUN_2,
            at("2026-09-28T09:00:00Z"),
            vec!["model.shop.orders".into()],
            1,
        ),
        RunRecord::new(
            1,
            RUN_1,
            at("2026-09-27T09:00:00Z"),
            vec!["seed.shop.raw_orders".into()],
            0,
        ),
    ];
    Dashboard::new("shop", "dev")
        .with_state(StateInput::Recorded(Box::new(Recorded::new(
            ".ods/state.db",
            runs,
            2,
            Ok(plan()),
        ))))
        .with_catalog(catalog_input())
}

fn no_store() -> Dashboard {
    Dashboard::new("shop", "default")
        .with_state(StateInput::NoStore {
            store: ".ods/state.db".into(),
        })
        .with_catalog(
            CatalogInput::new(nodes()).with_layer_source("the folder under the model paths"),
        )
}

fn start_with(dashboard: Dashboard, options: &ServeOptions) -> SocketAddr {
    let app = router(lineage().with_dashboard(dashboard), options);
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

fn start(dashboard: Dashboard) -> SocketAddr {
    start_with(dashboard, &ServeOptions::new(([127, 0, 0, 1], 0).into()))
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
    let response = String::from_utf8_lossy(&bytes);
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head.to_lowercase(), body.to_owned())
}

fn get(addr: SocketAddr, path: &str) -> (u16, String, String) {
    request(addr, "GET", path)
}

fn json(addr: SocketAddr, path: &str) -> serde_json::Value {
    let (status, _, body) = get(addr, path);
    assert_eq!(status, 200, "{path}: {body}");
    serde_json::from_str(&body).unwrap()
}

/// Pretty JSON with keys sorted, whatever `serde_json`'s map order.
fn pretty(value: serde_json::Value) -> String {
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
    serde_json::to_string_pretty(&sorted(value)).unwrap()
}

fn facet(view: &serde_json::Value, key: &str) -> BTreeMap<String, u64> {
    view["facets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == key)
        .unwrap_or_else(|| panic!("no facet {key}"))["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["value"].as_str().unwrap().to_owned(),
                v["count"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn names(view: &serde_json::Value) -> Vec<String> {
    view["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn the_catalog_api_lists_every_node_with_its_decision_and_last_build() {
    let addr = start(recorded());
    let (status, head, body) = get(addr, "/api/catalog");
    assert_eq!(status, 200);
    assert!(head.contains("content-type: application/json"), "{head}");
    let view: serde_json::Value = serde_json::from_str(&body).unwrap();
    insta::assert_snapshot!("catalog_recorded", pretty(view));
}

#[test]
fn without_a_state_store_every_node_is_never_built() {
    let addr = start(no_store());
    let view = json(addr, "/api/catalog");
    insta::assert_snapshot!("catalog_no_store", pretty(view.clone()));
    for row in view["rows"].as_array().unwrap() {
        assert_eq!(row["decision"]["decision"], "never_built", "{row}");
        assert!(row["last_build"].is_null(), "{row}");
    }
    assert_eq!(facet(&view, "decision")["never_built"], 3);

    let (status, _, page) = get(addr, "/catalog");
    assert_eq!(status, 200);
    assert!(page.contains(r#"<span class="pill never_built""#), "{page}");
    assert!(page.contains(r#"<span class="never">never built</span>"#));
    assert!(page.contains("no state store: every node is never built"));

    let (status, _, page) = get(addr, "/catalog/model.shop.orders?tab=state");
    assert_eq!(status, 200);
    assert!(
        page.contains("Never: no successful build is recorded."),
        "{page}"
    );
    assert!(page.contains(r#"Last successful build <span class="fg">never</span>"#));
}

#[test]
fn facet_counts_match_the_nodes_and_the_plan() {
    let dashboard = recorded();
    let snapshot = lineage();
    let view = dashboard.catalog(&snapshot.document, &CatalogQuery::default(), true);
    let view = serde_json::to_value(view).unwrap();
    let input = catalog_input();
    let plan = plan();

    // Every node once, whatever the filters shown.
    assert_eq!(view["total"], input.nodes.len());
    let mut types: BTreeMap<String, u64> = BTreeMap::new();
    let mut materialized: BTreeMap<String, u64> = BTreeMap::new();
    let mut layers: BTreeMap<String, u64> = BTreeMap::new();
    for node in &input.nodes {
        *types.entry(node.resource_type.clone()).or_default() += 1;
        if let Some(m) = &node.materialization {
            *materialized.entry(m.clone()).or_default() += 1;
        }
        if let Some(l) = &node.layer {
            *layers.entry(l.clone()).or_default() += 1;
        }
    }
    assert_eq!(facet(&view, "type"), types);
    assert_eq!(facet(&view, "materialized"), materialized);
    assert_eq!(facet(&view, "layer"), layers);
    assert_eq!(
        facet(&view, "tag"),
        BTreeMap::from([("core".to_owned(), 1)])
    );

    // Decisions add up to the plan: its builds are Build or Never built, its reuses
    // Reuse, and every node has exactly one.
    let decisions = facet(&view, "decision");
    let builds = plan.with_action(PlanAction::Build).count() as u64;
    let reuses = plan.with_action(PlanAction::Reuse).count() as u64;
    assert_eq!(decisions["build"] + decisions["never_built"], builds);
    assert_eq!(decisions["reuse"], reuses);
    assert_eq!(decisions["unknown"], 0);
    assert_eq!(decisions.values().sum::<u64>(), input.nodes.len() as u64);

    // Lineage confidence from the graph: one parsed, one inferred, the seed has none;
    // every confidence is listed, zeros too.
    assert_eq!(
        facet(&view, "lineage"),
        BTreeMap::from([
            ("parsed".to_owned(), 1),
            ("inferred".to_owned(), 1),
            ("observed".to_owned(), 0),
            ("unknown".to_owned(), 0),
            ("opaque".to_owned(), 0),
            ("n/a".to_owned(), 1),
        ])
    );
}

#[test]
fn facets_filter_and_are_kept_in_the_url() {
    let addr = start(recorded());
    let view = json(addr, "/api/catalog?decision=build&decision=never_built");
    assert_eq!(
        names(&view),
        ["model.shop.customers", "model.shop.orders"],
        "any value of a facet"
    );
    let view = json(
        addr,
        "/api/catalog?decision=build&decision=never_built&layer=staging",
    );
    assert_eq!(names(&view), ["model.shop.orders"], "every facet");
    assert_eq!(view["total"], 3);
    let view = json(addr, "/api/catalog?type=seed");
    assert_eq!(names(&view), ["seed.shop.raw_orders"]);
    assert_eq!(
        facet(&view, "type")["model"],
        2,
        "counts are over every node"
    );
    let view = json(addr, "/api/catalog?lineage=inferred");
    assert_eq!(names(&view), ["model.shop.customers"]);
    let view = json(addr, "/api/catalog?tag=core");
    assert_eq!(names(&view), ["model.shop.orders"]);
    let view = json(addr, "/api/catalog?q=RAW");
    assert_eq!(
        names(&view),
        ["seed.shop.raw_orders"],
        "search ignores case"
    );
    let view = json(addr, "/api/catalog?sort=last_built&desc=1");
    assert_eq!(
        names(&view),
        [
            "model.shop.orders",
            "seed.shop.raw_orders",
            "model.shop.customers"
        ],
        "newest first, never built last"
    );
    let view = json(addr, "/api/catalog?layer=gone");
    assert!(names(&view).is_empty(), "{:?}", names(&view));
    assert!(
        view["facets"][1]["values"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["value"] == "gone" && v["selected"] == true && v["count"] == 0),
        "a selected value stays, to be cleared"
    );

    let (status, _, page) = get(addr, "/catalog?layer=staging&sort=type");
    assert_eq!(status, 200);
    assert!(page.contains(r#"name="layer" value="staging" checked>"#));
    assert!(page.contains(r#"name="layer" value="marts">"#));
    assert!(page.contains(r#"data-shown="1">1 of 3 nodes"#), "{page}");
    // Sorting keeps the filters, and the filters keep the sort.
    assert!(
        page.contains(r#"<a href="catalog?layer=staging" aria-label="Sort by name">Name"#),
        "name is the default sort"
    );
    assert!(
        page.contains(
            r#"<a href="catalog?layer=staging&amp;sort=type&amp;desc=1" class="on" aria-label="Sort by type">Type"#
        )
    );
    assert!(page.contains(r#"<input type="hidden" name="sort" value="type">"#));
    assert!(page.contains(r#"<form id="facets" method="get" action="catalog">"#));
}

#[test]
fn the_catalog_page_shows_every_node_in_the_shell() {
    let addr = start(recorded());
    let (status, head, page) = get(addr, "/catalog");
    assert_eq!(status, 200);
    assert!(head.contains("content-type: text/html"), "{head}");
    assert!(head.contains("content-security-policy: default-src 'none'"));
    // The nav: Catalog is a link, with Models current and the rest planned.
    assert!(
        page.contains(r#"<a href="catalog" aria-current="page" data-section="catalog">"#),
        "{page}"
    );
    assert!(
        page.contains(r#"<a href="catalog" aria-current="page" data-item="models">Models</a>"#)
    );
    assert!(
        page.contains(
            r#"data-item="freshness">Freshness evidence<span class="chip">Planned</span>"#
        )
    );
    assert!(
        page.contains(r#"data-item="semantic">Semantic layer<span class="chip">Planned</span>"#)
    );
    assert!(
        page.contains(r#"<span class="crumb-here">Models</span>"#),
        "Catalog / Models, as the design: {page}"
    );
    assert!(page.contains(r#"<a href="lineage" data-section="lineage">"#));
    // Rows: decision pills, last build, health placeholder, links to model pages.
    assert!(page.contains(
        r#"<a class="mono" href="catalog/model.shop.orders" title="model.shop.orders">orders</a>"#
    ));
    assert!(
        page.contains(
            r#"<span class="pill build" title="sql changed since run 9ea38bd5">BUILD</span>"#
        ),
        "{page}"
    );
    assert!(page.contains(r#"<span class="pill reuse""#));
    assert!(page.contains(
        r#"<span class="pill never_built" title="no successful build recorded">NEVER BUILT</span>"#
    ));
    assert!(page.contains(
        r#"title="snapshot 2, run 9ea38bd5-0000-4000-8000-000000000002, finished 2026-09-28T09:00:00Z">2 · 9ea38bd5</span>"#
    ));
    assert!(page.contains(r#"<span class="never">never built</span>"#));
    assert!(page.contains(">[n]</span>"), "health waits for #117");
    assert!(page.contains("python model"));
    assert!(page.contains("decisions against snapshot 2"));
    assert!(
        page.contains("the folder under the model paths"),
        "the layer says where it comes from"
    );
    assert!(page.contains(r#"<span class="cdot inferred"></span>inferred"#));
    assert!(page.contains(r#"<th aria-sort="ascending"><a href="catalog?desc=1" class="on" aria-label="Sort by name">Name"#), "{page}");
    assert!(
        page.contains(r#"<th><a href="catalog?sort=type" aria-label="Sort by type">Type"#),
        "only the sorted column has aria-sort"
    );
    assert!(page.contains("Layer*"), "the layer is marked as derived");
    assert!(
        !page.contains(r#"id="search""#),
        "the Catalog has one search, its own"
    );
    assert!(page.contains(r#"<input id="catq" form="facets" name="q""#));
}

#[test]
fn a_model_page_has_a_tab_for_each_part() {
    let addr = start(recorded());
    let view = json(addr, "/api/catalog/model.shop.orders");
    insta::assert_snapshot!("model_orders", pretty(view));

    let path = "/catalog/model.shop.orders";
    let (status, _, page) = get(addr, path);
    assert_eq!(status, 200);
    // One level down: links, fonts and the script's API calls go back up.
    assert!(page.contains(r#"<meta name="ods-root" content="../">"#));
    assert!(page.contains("url('../assets/fonts/"));
    assert!(page.contains(r#"<a href="../lineage" data-section="lineage">"#));
    assert!(page.contains(
        r#"<a href="?tab=overview" aria-current="page" data-tab="overview">Overview</a>"#
    ));
    assert!(
        page.contains(r#"data-tab="relationships">Relationships<span class="chip">Planned</span>"#)
    );
    assert!(page.contains(r#"<span class="pill big build" title="sql changed since run 9ea38bd5">BUILD next run · code changed</span>"#), "{page}");
    assert!(page.contains(r#"<a class="crumb" href="../catalog">Catalog</a><span class="crumb-sep">/</span><a class="crumb" href="../catalog">Models</a><span class="crumb-sep">/</span><span class="crumb-here mono">orders</span>"#), "{page}");
    assert!(
        page.contains(r#"<a href="../catalog" aria-current="page" data-item="models">Models</a>"#),
        "the nav from one level down"
    );
    assert!(page.contains("One row per order."));
    assert!(
        page.contains(
            r#"<a class="btn" href="../lineage?node=model.shop.orders">View lineage</a>"#
        )
    );
    assert!(
        page.contains(r#"<a href="../state/plan?node=model.shop.orders">Why this decision</a>"#),
        "Why links to the Plan page's Why panel: {page}"
    );
    assert!(page.contains("snapshot 2 · run"));
    assert!(
        page.contains(r#"<code class="fg">models/staging/orders.sql</code>"#),
        "paths on loopback"
    );
    assert!(
        page.contains("<span>←&nbsp;raw_orders.id</span>"),
        "column lineage, on its own line"
    );

    let (_, _, page) = get(addr, &format!("{path}?tab=code"));
    assert!(
        page.contains("select id from {{ ref(&#x27;raw_orders&#x27;) }}")
            || page.contains("select id from {{ ref('raw_orders') }}"),
        "{page}"
    );
    assert!(
        page.contains("Not shown: compiled code can contain resolved secrets")
            && page.contains("<code>target/compiled/</code>"),
        "compiled code is never served (AGENTS rule 9): {page}"
    );
    let model = json(addr, "/api/catalog/model.shop.orders");
    assert!(model["code"].get("compiled").is_none(), "{model}");

    let (_, _, page) = get(addr, &format!("{path}?tab=lineage"));
    assert!(page.contains(r#"<a class="mono" href="../catalog/seed.shop.raw_orders" title="seed.shop.raw_orders">raw_orders</a>"#), "{page}");
    assert!(
        page.contains(r#"<span class="mono" title="source.shop.app.events">app.events</span>"#),
        "sources have no page"
    );
    assert!(
        page.contains(r#"href="../catalog/model.shop.customers""#),
        "downstream"
    );
    assert!(page.contains(
        r#"<a href="../lineage?node=model.shop.orders">Open in the lineage explorer</a>"#
    ));

    let (_, _, page) = get(addr, &format!("{path}?tab=state"));
    assert!(
        page.contains(
            r#"<code title="code_changed">code changed</code> code changed since run 9ea38bd5: sql"#
        ),
        "{page}"
    );
    assert!(page.contains("From the plan against snapshot 2"));

    assert!(
        page.contains(
            r#"<time datetime="2026-09-28T09:00:00Z" title="2026-09-28T09:00:00Z" data-relative>"#
        ),
        "relative, exact in the tooltip"
    );

    let (_, _, page) = get(addr, &format!("{path}?tab=nonsense"));
    assert!(
        page.contains(r#"aria-current="page" data-tab="overview""#),
        "unknown tabs show the overview"
    );

    // A seed has no code.
    let (status, _, page) = get(addr, "/catalog/seed.shop.raw_orders?tab=code");
    assert_eq!(status, 200);
    assert!(page.contains("The artifacts carry no code for this node"));
}

#[test]
fn an_unknown_node_is_a_404_not_a_panic() {
    let addr = start(recorded());
    let (status, head, page) = get(addr, "/catalog/model.shop.nope");
    assert_eq!(status, 404);
    assert!(head.contains("content-type: text/html"), "{head}");
    assert!(page.contains("No such node"));
    assert!(page.contains("<code>model.shop.nope</code>"));
    assert!(page.contains(r#"<a href="../catalog">Back to the Catalog</a>"#));
    let (status, _, body) = get(addr, "/api/catalog/model.shop.nope");
    assert_eq!(status, 404);
    assert!(body.contains("no such node"), "{body}");
    // Percent-encoded ids are decoded; odd ones are just unknown.
    let (status, _, _) = get(addr, "/catalog/model%2Eshop%2Eorders");
    assert_eq!(status, 200);
    let (status, _, page) = get(addr, "/catalog/%3Cscript%3E");
    assert_eq!(status, 404);
    assert!(page.contains("<code>&lt;script&gt;</code>"), "{page}");
}

#[test]
fn hostile_names_and_code_are_escaped() {
    let addr = start(recorded());
    let (_, _, page) = get(addr, "/catalog");
    assert!(!page.contains(HOSTILE_NAME), "{page}");
    assert!(page.contains("&lt;/script&gt;&lt;img src=x onerror=alert(1)&gt;"));
    let (_, _, page) = get(addr, "/catalog/model.shop.customers?tab=code");
    assert!(!page.contains(HOSTILE_SQL));
    assert!(!page.contains("</script><script>alert"), "{page}");
    assert!(page.contains("&lt;/script&gt;&lt;script&gt;alert(1)&lt;/script&gt;"));
    // Every other tab too.
    for tab in ["overview", "columns", "lineage", "state", "tests"] {
        let (_, _, page) = get(addr, &format!("/catalog/model.shop.customers?tab={tab}"));
        assert!(!page.contains(HOSTILE_NAME), "{tab}");
    }
    let (_, _, page) = get(addr, "/catalog/model.shop.orders");
    assert!(!page.contains(HOSTILE_NAME), "downstream names too");
    // The search box echoes the query escaped.
    let (_, _, page) = get(addr, "/catalog?q=%22%3E%3Cscript%3E");
    assert!(
        page.contains(r#"value="&quot;&gt;&lt;script&gt;""#),
        "{page}"
    );
}

#[test]
fn only_get_is_served() {
    let addr = start(recorded());
    for path in [
        "/catalog",
        "/catalog/model.shop.orders",
        "/api/catalog",
        "/api/catalog/model.shop.orders",
    ] {
        for method in ["POST", "PUT", "DELETE"] {
            let (status, _, _) = request(addr, method, path);
            assert_eq!(status, 405, "{method} {path}");
        }
    }
}

#[test]
fn beyond_loopback_paths_and_errors_are_left_out() {
    let dashboard = Dashboard::new("shop", "dev")
        .with_state(StateInput::Recorded(Box::new(Recorded::new(
            ".ods/state.db",
            vec![RunRecord::new(
                1,
                RUN_1,
                at("2026-09-27T09:00:00Z"),
                vec![],
                0,
            )],
            1,
            Err("cannot open /home/me/secret/state.db".into()),
        ))))
        .with_catalog(catalog_input());
    let options = ServeOptions::new(([0, 0, 0, 0], 0).into());
    let addr = start_with(dashboard.clone(), &options);
    let view = json(addr, "/api/catalog");
    assert_eq!(
        view["decisions"]["error"],
        "the plan couldn't be made; see the server log"
    );
    assert!(!view.to_string().contains("/home/me"), "{view}");
    for row in view["rows"].as_array().unwrap() {
        assert_eq!(
            row["decision"]["decision"], "unknown",
            "no plan, no guess: {row}"
        );
    }
    let model = json(addr, "/api/catalog/model.shop.orders");
    assert!(model["file"].is_null(), "{model}");
    let (_, _, page) = get(addr, "/catalog/model.shop.orders?tab=state");
    assert!(!page.contains("/home/me"));
    assert!(!page.contains("models/staging/orders.sql"));

    // On loopback, both are shown.
    let addr = start(dashboard);
    let view = json(addr, "/api/catalog");
    assert_eq!(
        view["decisions"]["error"],
        "cannot open /home/me/secret/state.db"
    );
    let model = json(addr, "/api/catalog/model.shop.orders");
    assert_eq!(model["file"], "models/staging/orders.sql");
}

#[test]
fn queries_round_trip_through_the_url() {
    let pairs = |s: &str| -> Vec<(String, String)> {
        s.split('&')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    };
    let query = CatalogQuery::from_pairs(&pairs(
        "tag=b&type=seed&tag=a&sort=layer&desc=1&q=x&bogus=1&sort=nope",
    ));
    assert_eq!(
        query.to_pairs(),
        pairs("type=seed&tag=a&tag=b&q=x&sort=layer&desc=1"),
        "stable order; unknown keys and sorts are dropped"
    );
}

#[test]
fn reuse_is_never_claimed_to_be_checked() {
    let addr = start(recorded());
    let view = json(addr, "/api/catalog");
    assert_eq!(view["decisions"]["relations_checked"], false);
    assert_eq!(
        view["decisions"]["caveats"][0],
        "Reuse is decided offline: a reused node's relation isn't checked by this plan (it is when a run starts)."
    );
    let (_, _, page) = get(addr, "/catalog");
    assert!(!page.contains("relation checked"), "{page}");
    assert!(page.contains(
        r#"<span class="pill reuse" title="code and inputs unchanged since run e6f54fe3; its relation is not checked by this plan; checked when a run starts">REUSE</span>"#
    ), "{page}");
    assert!(
        page.contains(
            "unchanged; its relation isn't checked by this plan (it is when a run starts)"
        )
    );
    let decision_note = view["facets"][4]["note"].as_str().unwrap();
    assert!(
        decision_note.contains("isn't checked by this plan"),
        "{decision_note}"
    );

    let (_, _, page) = get(addr, "/catalog/seed.shop.raw_orders");
    assert!(page.contains(r#"<span class="muted">Relation</span><span>not checked by this plan; checked when a run starts</span>"#), "{page}");
    let (_, _, page) = get(addr, "/catalog/seed.shop.raw_orders?tab=state");
    assert!(page.contains("Reuse is decided offline"), "{page}");
    let (_, _, page) = get(addr, "/catalog/model.shop.orders");
    assert!(
        page.contains(r#"<span class="muted">Relation</span><span>not needed: it builds</span>"#)
    );

    // Nothing reused: no caveat.
    let view = json(start(no_store()), "/api/catalog");
    assert!(
        view["decisions"]["caveats"].as_array().unwrap().is_empty(),
        "{:?}",
        view["decisions"]["caveats"].as_array().unwrap()
    );
}

#[test]
fn odd_node_urls_get_the_shells_404_or_a_redirect() {
    let addr = start(recorded());
    let (status, head, page) = get(addr, "/catalog/%FF%FE");
    assert_eq!(status, 404, "not axum's plain 400: {page}");
    assert!(head.contains("content-type: text/html"), "{head}");
    assert!(page.contains("No such node"));
    let (status, _, _) = get(addr, "/api/catalog/%FF%FE");
    assert_eq!(status, 404);
    let (status, head, _) = get(addr, "/catalog/");
    assert_eq!(status, 308);
    assert!(head.contains("location: /catalog\r\n"), "{head}");
}

#[test]
fn facets_and_sorts_follow_their_meaning_not_the_alphabet() {
    let addr = start(recorded());
    let view = json(addr, "/api/catalog");
    let values = |key: &str| -> Vec<String> {
        view["facets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == key)
            .unwrap()["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["value"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(values("layer"), ["staging", "marts"], "upstream first");
    assert_eq!(
        values("materialized"),
        ["table", "view", "seed"],
        "seeds last"
    );
    assert_eq!(
        values("decision"),
        ["build", "reuse", "never_built", "unknown"]
    );
    assert_eq!(view["layer_source"], "the folder under the model paths");
    // Decisions sort in the facet's order: build, reuse, never built.
    let view = json(addr, "/api/catalog?sort=decision");
    assert_eq!(
        names(&view),
        [
            "model.shop.orders",
            "seed.shop.raw_orders",
            "model.shop.customers"
        ]
    );
}

#[test]
fn the_columns_tab_says_where_types_and_lineage_come_from() {
    let addr = start(recorded());
    let path = "/catalog/model.shop.orders";
    let (_, _, page) = get(addr, &format!("{path}?tab=columns"));
    assert!(page.contains(r#"aria-current="page" data-tab="columns""#));
    assert!(
        page.contains(r#"<span class="mono" title="From the warehouse catalog, as of 2026-09-28T08:00:00Z">BIGINT</span>"#)
    );
    assert!(page.contains("Warehouse types are as of its catalog, written 2026-09-28T08:00:00Z."));
    assert!(
        page.contains(r#"legacy <span class="stale""#),
        "a column only the warehouse catalog lists may have been dropped: {page}"
    );
    assert!(
        !page.contains(r#"id <span class="stale""#),
        "declared columns are current"
    );
    assert!(page.contains(r#"varchar <span class="muted">(declared)</span>"#));
    assert!(
        page.contains(r#"<span class="unknown-type""#),
        "an unknown type is marked, not guessed"
    );
    assert!(
        page.contains("<td>unique</td><td>not_null</td>"),
        "column tests match whatever the case: {page}"
    );
    assert!(
        page.contains("←&nbsp;raw_orders.id</td>"),
        "parsed lineage is plain"
    );
    let (_, _, page) = get(addr, "/catalog/model.shop.customers?tab=columns");
    assert!(
        page.contains(r#"←&nbsp;orders.id <span class="inferred""#),
        "lineage that isn't parsed is marked inferred: {page}"
    );
    let (_, _, page) = get(addr, "/catalog/model.shop.customers");
    assert!(
        page.contains(r#"<span>←&nbsp;orders.id <span class="inferred""#),
        "on the overview too"
    );
}

#[test]
fn the_tests_tab_shows_only_recorded_outcomes() {
    let addr = start(recorded());
    let path = "/catalog/model.shop.orders";
    let (_, _, page) = get(addr, &format!("{path}?tab=tests"));
    assert!(page.contains(r#"title="test.shop.unique_orders_id.1">unique</td>"#));
    assert!(page.contains("<td>unit</td>"));
    assert!(page.contains("The last build's recorded checks passed together in run"));
    assert!(page.contains("Outcomes aren't kept per test yet"));
    // The tests the record vouches for passed; the other isn't recorded.
    assert_eq!(page.matches(r#"<span class="passed""#).count(), 2, "{page}");
    assert!(page.contains(r#"title="test.shop.elsewhere.3">elsewhere</td><td class="mono"></td><td>data</td><td><span class="muted" title="Not among the checks a recorded build passed">not recorded</span>"#), "{page}");
    let model = json(addr, "/api/catalog/model.shop.orders");
    assert_eq!(model["tests"][0]["last_outcome"]["outcome"], "passed");
    assert_eq!(model["tests"][0]["last_outcome"]["run_id"], RUN_2);
    assert!(model["tests"][2]["last_outcome"].is_null());
}

/// A test added or edited since the checks last passed hasn't run: the record no
/// longer vouches for any test, and none reads as passed.
#[test]
fn changed_checks_vouch_for_no_test() {
    let mut input = catalog_input();
    let build = input.last_builds.get_mut("model.shop.orders").unwrap();
    *build = LastBuild::new(Some(2), RUN_2, at("2026-09-28T09:00:00Z")).with_tested(
        RUN_2,
        at("2026-09-28T09:05:00Z"),
        Some("old digest".into()),
        false,
    );
    let addr = start(recorded().with_catalog(input));
    let model = json(addr, "/api/catalog/model.shop.orders");
    assert_eq!(
        model["checks_passed"]["checks_changed_since"], true,
        "{model}"
    );
    for test in model["tests"].as_array().unwrap() {
        assert!(test["last_outcome"].is_null(), "{test}");
    }
    let (_, _, page) = get(addr, "/catalog/model.shop.orders?tab=tests");
    assert!(!page.contains(r#"<span class="passed""#), "{page}");
    assert!(
        page.contains(
            "checks changed since run <span class=\"mono\">9ea38bd5</span>: not recorded"
        ),
        "{page}"
    );
}

/// A warehouse catalog may fold a column's case; its lineage is still found.
#[test]
fn column_lineage_matches_whatever_the_case() {
    let mut input = catalog_input();
    let orders = input
        .nodes
        .iter_mut()
        .find(|n| n.id == "model.shop.orders")
        .unwrap();
    orders.columns[0].name = "ID".into();
    let addr = start(recorded().with_catalog(input));
    let model = json(addr, "/api/catalog/model.shop.orders");
    assert_eq!(model["columns"][0]["name"], "ID");
    assert_eq!(
        model["columns"][0]["upstream"][0], "raw_orders.id",
        "{model}"
    );
}

/// The model page's header, up to its tabs.
fn model_head(page: &str) -> String {
    let start = page.find(r#"<div class="model-head">"#).expect("a header");
    let end = page[start..].find(r#"<nav class="tabs""#).expect("tabs") + start;
    page[start..end].replace("><", ">\n<")
}

/// `recorded()`, with `orders`' warehouse link set to `link`.
fn with_link(link: &Result<RelationLink, NoRelationLink>) -> Dashboard {
    let mut nodes = nodes();
    for node in &mut nodes {
        if node.id == "model.shop.orders" {
            node.relation = Some("`main`.`shop`.`orders`".into());
            node.relation_link = RelationLinkFields::from(link.clone());
        }
    }
    let mut input = catalog_input();
    input.nodes = nodes;
    recorded().with_catalog(input)
}

#[test]
fn a_model_page_opens_its_relation_in_the_warehouse() {
    let url = "https://dbc-1.cloud.databricks.com/explore/data/main/shop/orders";
    let addr = start(with_link(&Ok(RelationLink::new(
        url,
        "Open in Catalog Explorer",
    ))));
    let (_, _, page) = get(addr, "/catalog/model.shop.orders");
    let head = model_head(&page);
    insta::assert_snapshot!("model_head_with_link", head);
    // A new tab that is given nothing of this page (no opener, no referrer).
    assert!(head.contains(&format!(
        r#"<a class="btn" href="{url}" target="_blank" rel="noopener noreferrer""#
    )));
    assert!(head.contains("Open in Catalog Explorer ↗</a>"), "{head}");
    // Where it is expected to be, not proof that it exists (AGENTS rule 3).
    assert!(head.contains("expected location"), "{head}");
    assert!(head.contains("hasn't checked that it exists"), "{head}");
    assert!(!head.contains("no_relation_link"), "{head}");
    let model = json(addr, "/api/catalog/model.shop.orders");
    assert_eq!(model["relation_url"], url);
    assert_eq!(model["relation_url_label"], "Open in Catalog Explorer");
    assert!(model.get("relation_url_unavailable").is_none(), "{model}");
}

#[test]
fn a_model_page_without_a_link_says_why() {
    let addr = start(with_link(&Err(NoRelationLink::Unsupported {
        warehouse: Some("duckdb".into()),
    })));
    let (_, _, page) = get(addr, "/catalog/model.shop.orders");
    let head = model_head(&page);
    insta::assert_snapshot!("model_head_without_link", head);
    assert!(
        !head.contains("data-relation-link"),
        "no guessed link: {head}"
    );
    assert!(!head.contains("expected location"), "{head}");
    assert!(
        head.contains(r#"<span class="nolink" data-state="no_relation_link">No warehouse link for <code>duckdb</code> targets</span>"#),
        "{head}"
    );
    let model = json(addr, "/api/catalog/model.shop.orders");
    assert!(model.get("relation_url").is_none(), "{model}");
    assert_eq!(
        model["relation_url_unavailable"],
        "no warehouse link for `duckdb` targets"
    );

    // A link that isn't https:// is never rendered, whatever the binary handed over.
    let addr = start(with_link(&Ok(RelationLink::new(
        "javascript:alert(1)",
        "Open",
    ))));
    let (_, _, page) = get(addr, "/catalog/model.shop.orders");
    assert!(!page.contains("javascript:alert"), "{page}");
}
