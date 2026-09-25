//! The explorer's three delivery modes and its HTTP API, over a real socket.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};

use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, RelationName};
use ods_lineage::{GraphFilter, LineageNode, LineageProject, MemoryCache, NodeKind, build};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage};
use ods_web::{ServeOptions, Snapshot, export_site, router, search, standalone_page};

fn rel(name: &str) -> RelationName {
    RelationName::new(["db", name]).unwrap()
}

/// `raw_orders` (seed) → `orders` (model, `amount` derived from `raw_orders.total`).
fn snapshot() -> Snapshot {
    let identity = EdgeKind::Direct(DirectKind::Identity);
    let lineage = QueryLineage::new(
        vec![
            OutputColumn::new(
                "id",
                [(ColumnRef::new(rel("raw_orders"), "id"), identity)].into(),
                "id",
                Confidence::Exact,
            ),
            OutputColumn::new(
                "amount",
                [(
                    ColumnRef::new(rel("raw_orders"), "total"),
                    EdgeKind::Direct(DirectKind::Transformation),
                )]
                .into(),
                "amount",
                Confidence::Exact,
            ),
        ],
        std::collections::BTreeSet::default(),
        [rel("raw_orders")].into(),
        "rows",
        vec![],
    );
    let analyzer = FakeSqlLineageAnalyzer::new().with("orders.sql", lineage);
    let project = LineageProject::new(vec![
        LineageNode::new("seed.raw_orders", rel("raw_orders"), NodeKind::Seed)
            .with_columns(["id", "total"]),
        LineageNode::new("model.orders", rel("orders"), NodeKind::Model)
            .with_sql("orders.sql")
            .with_depends_on(["seed.raw_orders"]),
    ]);
    let (graph, _) = build(&project, &analyzer, &MemoryCache::default()).unwrap();
    let name = |id: &str| id.rsplit('.').next().unwrap_or(id).to_owned();
    let document = graph.document(&name, &GraphFilter::default());
    Snapshot::new(document, graph, "fixture")
}

/// Serves `snapshot` under `base` on a free loopback port, on a background thread.
fn start(base: &str) -> SocketAddr {
    start_with(snapshot(), base)
}

fn start_with(snapshot: Snapshot, base: &str) -> SocketAddr {
    let options = ServeOptions::new(([127, 0, 0, 1], 0).into())
        .with_base_path(base)
        .unwrap();
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

/// A minimal HTTP/1.1 GET: (status, headers, body).
fn get(addr: SocketAddr, path: &str) -> (u16, String, String) {
    get_as(addr, &format!("localhost:{}", addr.port()), path)
}

fn get_as(addr: SocketAddr, host: &str, path: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head.to_lowercase(), body.to_owned())
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
}

#[test]
fn the_api_answers_graph_search_node_and_impact_queries() {
    let addr = start("");

    let (status, head, page) = get(addr, "/");
    assert_eq!(status, 200);
    assert!(
        head.contains("content-security-policy: default-src 'none'"),
        "{head}"
    );
    assert!(head.contains("x-content-type-options: nosniff"));
    assert!(
        page.contains(r#"content="api""#),
        "the served page talks to the API"
    );
    assert!(page.contains("model.orders"), "the first paint is embedded");

    let (_, _, body) = get(addr, "/api/version");
    let version = json(&body);
    assert_eq!(version["api"], 1);
    assert_eq!(version["generation"], 1);
    assert_eq!(version["source"], "fixture");

    let (_, _, body) = get(addr, "/api/graph");
    assert_eq!(json(&body)["nodes"].as_array().unwrap().len(), 2);

    let (_, _, body) = get(addr, "/api/search?q=amou");
    let hits = json(&body);
    assert_eq!(hits[0]["label"], "orders.amount");
    assert_eq!(hits[0]["kind"], "column");

    let (status, _, body) = get(addr, "/api/node?id=orders");
    assert_eq!(status, 200);
    assert_eq!(json(&body)["node"]["id"], "model.orders");

    let (status, _, body) = get(addr, "/api/impact?node=raw_orders&column=total");
    assert_eq!(status, 200);
    let impact = json(&body);
    assert!(
        impact["impact"].to_string().contains("model.orders"),
        "a change to raw_orders.total reaches orders: {impact}"
    );

    // A typo must not read as "nothing is affected".
    let (status, _, body) = get(addr, "/api/impact?node=raw_orders&column=totl");
    assert_eq!(status, 400, "{body}");
    let (status, _, body) = get(addr, "/api/impact?node=raw_orders&column=TOTAL");
    assert_eq!(status, 200, "case-insensitive when unambiguous");
    assert!(body.contains("model.orders"), "{body}");
    let (status, _, _) = get(
        addr,
        "/api/impact?node=raw_orders&column=discount&kind=added",
    );
    assert_eq!(status, 200, "added columns don't exist yet");

    let (status, _, _) = get(addr, "/api/node?id=nope");
    assert_eq!(status, 404);
    let (status, _, _) = get(addr, "/api/impact?node=orders&column=id&kind=renamed");
    assert_eq!(status, 400);
}

#[test]
fn it_can_live_under_a_reverse_proxy_prefix() {
    let addr = start("lineage/");
    assert_eq!(get(addr, "/lineage/").0, 200);
    let (status, head, _) = get(addr, "/lineage");
    assert_eq!(status, 308, "the page must load from the slash URL");
    assert!(head.contains("location: /lineage/"), "{head}");
    assert_eq!(get(addr, "/lineage/healthz").2, "ok");
    assert_eq!(get(addr, "/lineage/api/graph").0, 200);
    assert_eq!(get(addr, "/api/graph").0, 404);
}

#[test]
fn loopback_servers_refuse_foreign_host_names() {
    let addr = start("");
    // DNS rebinding: a page on evil.example resolved to 127.0.0.1 still says so.
    assert_eq!(get_as(addr, "evil.example", "/api/graph").0, 421);
    assert_eq!(get_as(addr, "127.0.0.1", "/healthz").0, 200);
    assert_eq!(get_as(addr, "[::1]:9", "/healthz").0, 200);
}

#[test]
fn base_paths_that_are_not_plain_segments_are_refused() {
    let options = ServeOptions::new(([127, 0, 0, 1], 0).into());
    for bad in ["/{x", "/a/{id}", "my lineage", "/a/../b"] {
        assert!(options.clone().with_base_path(bad).is_err(), "{bad}");
    }
}

#[test]
fn hostile_names_cannot_break_out_of_the_embedded_graph() {
    let evil = "</script><script>alert(1)</script>";
    let mut snapshot = snapshot();
    snapshot.document.nodes[0].name = evil.to_owned();
    let page = standalone_page(&snapshot.document).unwrap();
    assert!(!page.contains(evil));
    assert!(page.contains(r"\u003c/script>\u003cscript>alert(1)\u003c/script>"));
    let addr = start_with(snapshot, "");
    let (_, _, served) = get(addr, "/");
    assert!(!served.contains(evil));
}

#[test]
fn standalone_pages_embed_the_graph_and_static_sites_fetch_it() {
    let snapshot = snapshot();
    let page = standalone_page(&snapshot.document).unwrap();
    assert!(page.contains(r#"content="embedded""#));
    assert!(page.contains("model.orders"));

    let dir = std::env::temp_dir().join(format!("ods-web-site-{}", std::process::id()));
    let files = export_site(&snapshot.document, &dir).unwrap();
    assert_eq!(files, [dir.join("index.html"), dir.join("graph.json")]);
    let index = std::fs::read_to_string(&files[0]).unwrap();
    assert!(index.contains(r#"content="graph.json""#));
    assert!(
        !index.contains("model.orders"),
        "the site page carries no data"
    );
    let graph = json(&std::fs::read_to_string(&files[1]).unwrap());
    assert_eq!(graph, serde_json::to_value(&snapshot.document).unwrap());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn search_ranks_prefix_matches_first_and_needs_every_term() {
    let document = snapshot().document;
    let labels = |q: &str| -> Vec<String> {
        search(&document, q, 10)
            .into_iter()
            .map(|h| h.label)
            .collect()
    };
    // Prefix matches first, then shorter labels.
    assert_eq!(
        labels("orders"),
        [
            "orders",
            "orders.id",
            "orders.amount",
            "raw_orders",
            "raw_orders.id",
            "raw_orders.total"
        ]
    );
    assert_eq!(labels("raw id"), ["raw_orders.id"]);
    assert!(labels("  ").is_empty());
    assert_eq!(search(&document, "orders", 1).len(), 1);
}
