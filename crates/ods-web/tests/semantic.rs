//! The Semantic layer page (#352): the semantic models and metrics a project declares,
//! read-only, with the models each is defined on; and an empty state without them.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};

use ods_lineage::{GraphFilter, LineageProject, MemoryCache, build};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_web::catalog::{CatalogInput, CatalogNode};
use ods_web::semantic::{MetricInput, SemanticField, SemanticInput, SemanticModelInput};
use ods_web::{Dashboard, ServeOptions, Snapshot, router};

const HOSTILE: &str = "</script><img src=x onerror=alert(1)>";

fn field(name: &str, kind: &str) -> SemanticField {
    SemanticField::new(name, Some(kind.to_owned()))
}

/// Two semantic models on two models, three metrics: two simple, and a ratio of them.
fn input() -> SemanticInput {
    let mut orders = SemanticModelInput::new("semantic_model.shop.orders", "orders");
    orders.description = Some("One row per order.".into());
    orders.defined_on = vec!["model.shop.orders".into()];
    orders.entities = vec![field("order", "primary")];
    orders.measures = vec![field("order_total", "sum"), field("order_count", "count")];
    orders.dimensions = vec![field("ordered_at", "time"), field(HOSTILE, "categorical")];
    let mut customers = SemanticModelInput::new("semantic_model.shop.customers", "customers");
    // Defined on a model the Catalog doesn't have: named, never linked.
    customers.defined_on = vec!["model.other.customers".into()];
    customers.dimensions = vec![field("first_ordered_at", "time")];
    let mut revenue = MetricInput::new("metric.shop.revenue", "revenue");
    revenue.kind = Some("simple".into());
    revenue.label = Some("Revenue".into());
    revenue.computed_from = Some("order_total".into());
    revenue.reads = vec!["semantic_model.shop.orders".into()];
    let mut customers_metric = MetricInput::new("metric.shop.customers", "customers");
    customers_metric.kind = Some("simple".into());
    customers_metric.reads = vec!["semantic_model.shop.customers".into()];
    let mut per_customer = MetricInput::new("metric.shop.per_customer", "per_customer");
    per_customer.kind = Some("ratio".into());
    per_customer.computed_from = Some("revenue / customers".into());
    per_customer.reads = vec!["metric.shop.revenue".into(), "metric.shop.customers".into()];
    SemanticInput::new(
        "the project's manifest",
        vec![orders, customers],
        vec![revenue, customers_metric, per_customer],
    )
}

fn dashboard(semantic: SemanticInput) -> Dashboard {
    Dashboard::new("shop", "dev")
        .with_catalog(CatalogInput::new(vec![CatalogNode::new(
            "model.shop.orders",
            "orders",
            "model",
        )]))
        .with_semantic(semantic)
}

fn start(dashboard: Dashboard) -> SocketAddr {
    let (graph, _) = build(
        &LineageProject::new(vec![]),
        &FakeSqlLineageAnalyzer::new(),
        &MemoryCache::default(),
    )
    .unwrap();
    let document = graph.document(&|id: &str| id.to_owned(), &GraphFilter::default());
    let snapshot = Snapshot::new(document, graph, "fixture").with_dashboard(dashboard);
    let app = router(snapshot, &ServeOptions::new(([127, 0, 0, 1], 0).into()));
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

/// GET `path`: (status, body).
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
fn each_metric_reaches_its_models_through_the_metrics_it_reads() {
    let view = dashboard(input()).semantic();
    let names: Vec<&str> = view.models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["customers", "orders"], "by id");
    // A model the Catalog has is linked; another is only named.
    let orders = &view.models[1];
    assert_eq!(
        orders.defined_on[0].href.as_deref(),
        Some("catalog/model.shop.orders")
    );
    assert_eq!(view.models[0].defined_on[0].href, None);
    assert_eq!(view.models[0].defined_on[0].name, "model.other.customers");

    let metric = |id: &str| view.metrics.iter().find(|m| m.id == id).unwrap();
    let revenue = metric("metric.shop.revenue");
    assert_eq!(revenue.semantic_models, ["orders"]);
    assert_eq!(revenue.dimensions, [HOSTILE, "ordered_at"]);
    // The ratio reads two metrics, so both semantic models, and both their models.
    let ratio = metric("metric.shop.per_customer");
    assert_eq!(ratio.semantic_models, ["customers", "orders"]);
    assert_eq!(
        ratio.dimensions,
        [HOSTILE, "first_ordered_at", "ordered_at"]
    );
    let models: Vec<&str> = ratio.depends_on.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(models, ["model.other.customers", "model.shop.orders"]);
}

#[test]
fn the_page_lists_the_definitions_and_escapes_them() {
    let addr = start(dashboard(input()));
    let (status, page) = get(addr, "/catalog/semantic");
    assert_eq!(status, 200);
    assert!(!page.contains(HOSTILE), "a name is text, never markup");
    for expected in [
        "Read-only placeholder.",
        r#"<span class="sem-pill">the project's manifest</span>"#,
        r#"<div class="sem-model" data-semantic-model="semantic_model.shop.orders">"#,
        r#"on model <a class="mono" href="../catalog/model.shop.orders" title="model.shop.orders">orders</a>"#,
        r#"<span class="mono" title="model.other.customers">model.other.customers</span>"#,
        r#"<span class="mono">order_total</span> <span class="muted">sum</span>"#,
        r#"<tr data-metric="metric.shop.per_customer">"#,
        r#"<span class="sem-kind">ratio</span>"#,
        "revenue / customers",
        r#"<span class="label">Revenue</span>"#,
        "3 metrics",
        r#"<a class="btn" href="../lineage/impact">Open impact</a>"#,
    ] {
        assert!(page.contains(expected), "{expected}\n{page}");
    }
    assert!(!page.contains("<form"), "nothing on the page queries");
    // Catalog is current, with the Semantic layer its current page.
    assert!(
        page.contains(
            r#"<a href="../catalog/semantic" aria-current="page" data-item="semantic">Semantic layer</a>"#
        ),
        "{page}"
    );
}

#[test]
fn a_project_without_a_semantic_layer_gets_an_empty_state() {
    let empty = SemanticInput::new("the project's manifest", vec![], vec![]);
    let addr = start(dashboard(empty));
    let (status, page) = get(addr, "/catalog/semantic");
    assert_eq!(status, 200);
    assert!(page.contains("No semantic layer"));
    assert!(page.contains("This project declares no semantic models or metrics."));
    assert!(
        !page.contains(r#"<table class="sem-table""#),
        "no table without metrics"
    );
    // Nothing read at all says so.
    let addr = start(dashboard(SemanticInput::default()));
    let (_, page) = get(addr, "/catalog/semantic");
    assert!(page.contains("the project's definitions couldn't be read"));
}

#[test]
fn the_api_serves_the_view() {
    let addr = start(dashboard(input()));
    let (status, body) = get(addr, "/api/catalog/semantic");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        json,
        serde_json::to_value(dashboard(input()).semantic()).unwrap()
    );
    assert_eq!(json["metrics"][1]["id"], "metric.shop.per_customer");
}
