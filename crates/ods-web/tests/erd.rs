//! The ERD page (#64): its view model, its JSON API and its page.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};

use ods_erd::{Basis, BuildOptions, ColumnInput, EntityInput, EntityKind, Fact, build};
use ods_lineage::{GraphFilter, LineageProject, MemoryCache};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_web::erd::{ErdInput, ErdQuery, Suggestion, erd_view};
use ods_web::{Dashboard, ServeOptions, Snapshot, router};

/// An entity name that must never reach the page raw.
const HOSTILE: &str = "</script><img src=x onerror=alert(1)>";

fn cols(names: &[&str]) -> Vec<ColumnInput> {
    names.iter().map(|n| ColumnInput::new(*n, None)).collect()
}

/// `orders` → `customers` is tested; `payments` joins `orders` in SQL; `reviews` only
/// looks like it references `customers`; `audit` relates to nothing.
fn input(audit_name: &str) -> ErdInput {
    let entity = |id: &str, name: &str, columns: &[&str]| {
        EntityInput::new(id, name, EntityKind::Table, cols(columns))
    };
    let entities = vec![
        entity(
            "model.shop.customers",
            "customers",
            &["customer_id", "name"],
        ),
        entity(
            "model.shop.orders",
            "orders",
            &["order_id", "customer_id", "amount"],
        ),
        entity(
            "model.shop.payments",
            "payments",
            &["payment_id", "order_id"],
        ),
        entity(
            "model.shop.reviews",
            "reviews",
            &["review_id", "customer_id"],
        ),
        entity("model.shop.audit", audit_name, &["at"]),
    ];
    let unique = |entity: &str, column: &str| Fact::Unique {
        entity: entity.into(),
        columns: vec![column.into()],
        basis: Basis::Tested,
        evidence: format!("test.unique_{column}"),
    };
    let not_null = |entity: &str, column: &str| Fact::NotNull {
        entity: entity.into(),
        column: column.into(),
        basis: Basis::Tested,
        evidence: format!("test.not_null_{column}"),
    };
    let facts = vec![
        unique("model.shop.customers", "customer_id"),
        not_null("model.shop.customers", "customer_id"),
        unique("model.shop.orders", "order_id"),
        not_null("model.shop.orders", "order_id"),
        Fact::ForeignKey {
            entity: "model.shop.orders".into(),
            columns: vec!["customer_id".into()],
            to: "model.shop.customers".into(),
            to_columns: vec!["customer_id".into()],
            basis: Basis::Tested,
            evidence: "test.relationships_orders_customer_id".into(),
        },
        Fact::Joined {
            left: "model.shop.payments".into(),
            left_columns: vec!["order_id".into()],
            right: "model.shop.orders".into(),
            right_columns: vec!["order_id".into()],
            evidence: "model.shop.revenue".into(),
        },
    ];
    let erd = build(
        &entities,
        &facts,
        BuildOptions::default().with_inference(true),
    );
    let suggestions = vec![Suggestion::new(
        "model.shop.payments",
        vec!["order_id".into()],
        "model.shop.orders",
        vec!["order_id".into()],
        "under payments › columns",
        "- name: order_id\n  tests: [relationships]",
    )];
    ErdInput::new(Ok(erd), suggestions)
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn untested_relationships_are_numbered_with_how_to_test_them() {
    let input = input("audit");
    let view = erd_view(Some(&input), &ErdQuery::default());
    let erd = view.erd.as_ref().unwrap();
    // Only entities with a relationship, by default.
    assert!(!erd.entities.iter().any(|e| e.name == "audit"));
    assert_eq!(view.total_entities, 5);
    let by_basis = |b: Basis| erd.relationships.iter().filter(|r| r.basis == b).count();
    assert_eq!(by_basis(Basis::Tested), 1);
    assert_eq!(by_basis(Basis::Joined), 1);
    assert_eq!(
        by_basis(Basis::Inferred),
        1,
        "reviews.customer_id, from naming"
    );
    // Joined and inferred ones are missing, numbered in order; tested ones are known.
    assert_eq!(view.missing.len(), 2);
    assert_eq!(
        view.missing.iter().map(|m| m.number).collect::<Vec<_>>(),
        [1, 2]
    );
    let joined = view
        .missing
        .iter()
        .find(|m| m.basis == Basis::Joined)
        .unwrap();
    assert!(
        joined.summary.contains("joined in `revenue`")
            || joined.summary.contains("joined in `model.shop.revenue`"),
        "{}",
        joined.summary
    );
    assert!(
        joined.suggestion.is_some(),
        "the binary's suggestion is matched"
    );
    let inferred = view
        .missing
        .iter()
        .find(|m| m.basis == Basis::Inferred)
        .unwrap();
    assert!(inferred.summary.contains("matching column names only"));
    assert!(inferred.suggestion.is_none(), "none given for it");
    assert_eq!(view.known.len(), 1);
    assert_eq!(view.known[0].from, "orders.customer_id");
    // One number per relationship, `Some` exactly for the missing ones.
    assert_eq!(view.numbers.len(), erd.relationships.len());
    assert_eq!(view.numbers.iter().flatten().count(), 2);
}

#[test]
fn the_scope_follows_the_query() {
    let input = input("audit");
    let all = erd_view(Some(&input), &ErdQuery::from_pairs(&pairs(&[("all", "1")])));
    assert_eq!(all.erd.as_ref().unwrap().entities.len(), 5);
    // `payments` and what it relates to, one step away.
    let focused = erd_view(
        Some(&input),
        &ErdQuery::from_pairs(&pairs(&[("select", "payments"), ("depth", "1")])),
    );
    let names: Vec<&str> = focused
        .erd
        .as_ref()
        .unwrap()
        .entities
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, ["orders", "payments"]);
    // Several at once, separated by spaces or commas; unknown names are said.
    let query = ErdQuery::from_pairs(&pairs(&[("select", "payments, nope reviews")]));
    assert_eq!(query.select, ["nope", "payments", "reviews"]);
    let view = erd_view(Some(&input), &query);
    assert_eq!(view.unknown, ["nope"]);
    // Only unknown names: nothing, not everything.
    let none = erd_view(
        Some(&input),
        &ErdQuery::from_pairs(&pairs(&[("select", "nope")])),
    );
    assert_eq!(none.erd.as_ref().unwrap().entities, []);
    assert_eq!(ErdQuery::from_pairs(&pairs(&[("depth", "99")])).depth, 10);
}

#[test]
fn without_an_erd_the_page_says_why() {
    let view = erd_view(None, &ErdQuery::default());
    assert!(view.erd.is_none());
    assert!(view.unavailable.is_some());
    let broken = ErdInput::new(Err("manifest.json is missing".into()), Vec::new());
    let view = erd_view(Some(&broken), &ErdQuery::default());
    assert_eq!(
        view.unavailable.as_deref(),
        Some("manifest.json is missing")
    );
}

// ---------------------------------------------------------------------- served

fn snapshot(input: ErdInput) -> Snapshot {
    let graph = ods_lineage::build(
        &LineageProject::new(Vec::new()),
        &FakeSqlLineageAnalyzer::new(),
        &MemoryCache::default(),
    )
    .unwrap()
    .0;
    let document = graph.document(&|id: &str| id.to_owned(), &GraphFilter::default());
    Snapshot::new(document, graph, "fixture")
        .with_dashboard(Dashboard::new("shop", "dev").with_erd(input))
}

fn start(snapshot: Snapshot) -> SocketAddr {
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
    (
        head.split(' ').nth(1).unwrap().parse().unwrap(),
        body.to_owned(),
    )
}

#[test]
fn the_page_draws_the_erd_in_the_shell_with_what_is_missing() {
    let addr = start(snapshot(input("audit")));
    let (status, page) = request(addr, "GET", "/erd");
    assert_eq!(status, 200);
    assert!(page.contains(r#"aria-current="page" data-section="erd""#));
    assert!(page.contains(r#"<script type="application/json" id="erd-data">"#));
    assert!(page.contains("Missing relationships"));
    assert!(page.contains("Already tested (1)"));
    assert!(page.contains("under payments › columns"));
    assert!(page.contains("Relationships only."), "never lineage");
    assert!(
        page.contains(r#"<tr data-basis="inferred">"#),
        "every relationship, without script"
    );
    assert_eq!(request(addr, "POST", "/erd").0, 405);
    let (_, page) = request(addr, "GET", "/erd?select=nope");
    assert!(page.contains(r#"role="alert""#) && page.contains("<code>nope</code>"));
}

#[test]
fn the_api_is_the_view_model_versioned() {
    let addr = start(snapshot(input("audit")));
    let (status, body) = request(addr, "GET", "/api/erd?select=payments&depth=1");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["schema_version"], 2);
    assert_eq!(json["erd"]["schema_version"], 1);
    assert_eq!(json["erd"]["entities"].as_array().unwrap().len(), 2);
    assert_eq!(json["missing"][0]["basis"], "joined");
}

#[test]
fn hostile_names_are_escaped() {
    let addr = start(snapshot(input(HOSTILE)));
    let (status, page) = request(addr, "GET", "/erd?all=1");
    assert_eq!(status, 200);
    assert!(
        !page.contains(HOSTILE),
        "never raw, in the HTML or the embedded JSON"
    );
    let (_, page) = request(addr, "GET", "/erd?select=%3C%2Fscript%3E");
    assert!(
        page.contains("<code>&lt;/script&gt;</code>")
            && page.contains(r#"value="&lt;/script&gt;""#),
        "the query is escaped too"
    );
    assert!(!page.contains("<code></script>"));
}

#[test]
fn cardinality_is_only_claimed_where_a_key_proves_it() {
    let input = input("audit");
    let view = erd_view(Some(&input), &ErdQuery::default());
    let erd = view.erd.as_ref().unwrap();
    assert_eq!(view.proven.len(), erd.relationships.len());
    for (r, proven) in erd.relationships.iter().zip(&view.proven) {
        match r.basis {
            // A tested reference to a tested key: proven.
            Basis::Tested => assert!(*proven, "{r:?}"),
            // Inferred from names: nothing is proven, whatever was worked out.
            Basis::Inferred => assert!(!*proven, "{r:?}"),
            _ => {}
        }
    }
    let inferred = view
        .missing
        .iter()
        .find(|m| m.basis == Basis::Inferred)
        .unwrap();
    assert!(
        inferred.summary.contains("Cardinality unproven"),
        "{}",
        inferred.summary
    );
}

#[test]
fn a_shared_name_is_ambiguous_not_unknown() {
    let input = input("orders");
    let view = erd_view(
        Some(&input),
        &ErdQuery::from_pairs(&pairs(&[("select", "orders")])),
    );
    assert_eq!(view.ambiguous, ["orders"]);
    assert_eq!(view.unknown, Vec::<String>::new());
}
