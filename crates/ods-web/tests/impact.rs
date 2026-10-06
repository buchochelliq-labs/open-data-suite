//! The Impact simulator (#347): its view model, its JSON API and its page.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};

use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, IndirectKind, RelationName};
use ods_lineage::{
    ColumnGraph, GraphFilter, LineageNode, LineageProject, MemoryCache, NodeKind, build,
};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage};
use ods_web::catalog::{
    CatalogColumn, CatalogInput, CatalogNode, CatalogTest, TestKind, TypeSource,
};
use ods_web::impact::{
    ChangeKind, ImpactQuery, ImpactView, LineageStatus, MAX_CHANGES, MAX_TRAIL, Verdict,
    impact_view,
};
use ods_web::{Dashboard, ServeOptions, Snapshot, router};

/// A model name that must never reach the page raw.
const HOSTILE: &str = "</script><img src=x onerror=alert(1)>";

fn rel(name: &str) -> RelationName {
    RelationName::new(["db", name]).unwrap()
}

fn col(relation: &str, column: &str) -> ColumnRef {
    ColumnRef::new(rel(relation), column)
}

const ID: EdgeKind = EdgeKind::Direct(DirectKind::Identity);
const AGG: EdgeKind = EdgeKind::Direct(DirectKind::Aggregation);

fn out(name: &str, inputs: &[(ColumnRef, EdgeKind)]) -> OutputColumn {
    OutputColumn::new(
        name,
        inputs.iter().cloned().collect(),
        format!("digest:{name}"),
        Confidence::Exact,
    )
}

fn query(
    outputs: Vec<OutputColumn>,
    rows: &[(ColumnRef, IndirectKind)],
    reads: &[&str],
) -> QueryLineage {
    QueryLineage::new(
        outputs,
        rows.iter().cloned().collect(),
        reads.iter().map(|r| rel(r)).collect(),
        "rows",
        vec![],
    )
}

/// `raw_orders` → `orders` → {`customers`, `rank`, `py` (opaque)}; `all_customers`
/// selects `*` from `customers`; `summary` reads the opaque `py`; `events` reads
/// `raw_orders` only; `report` reads `orders` but none of the columns changed here.
#[allow(
    clippy::too_many_lines,
    reason = "one fixture, easiest to read top to bottom"
)]
fn graph() -> ColumnGraph {
    let orders = query(
        vec![
            out("order_id", &[(col("raw_orders", "id"), ID)]),
            out("customer_id", &[(col("raw_orders", "customer_id"), ID)]),
            out("status", &[(col("raw_orders", "status"), ID)]),
            out("amount", &[(col("raw_orders", "amount"), ID)]),
        ],
        &[],
        &["raw_orders"],
    );
    let customers = query(
        vec![
            out("customer_id", &[(col("orders", "customer_id"), ID)]),
            out("lifetime_value", &[(col("orders", "amount"), AGG)]),
        ],
        &[
            (col("orders", "status"), IndirectKind::Filter),
            (col("orders", "customer_id"), IndirectKind::GroupBy),
        ],
        &["orders"],
    );
    let rank = query(
        vec![
            out("order_id", &[(col("orders", "order_id"), ID)]),
            out("amount", &[(col("orders", "amount"), ID)]),
        ],
        &[],
        &["orders"],
    );
    let report = query(
        vec![out("order_id", &[(col("orders", "order_id"), ID)])],
        &[],
        &["orders"],
    );
    let all_customers = query(
        vec![
            out("customer_id", &[(col("customers", "customer_id"), ID)]),
            out(
                "lifetime_value",
                &[(col("customers", "lifetime_value"), ID)],
            ),
        ],
        &[],
        &["customers"],
    )
    .with_wildcards([rel("customers")].into());
    let summary = query(
        vec![out("segment", &[(col("py", "segment"), ID)])],
        &[],
        &["py"],
    );
    let events = query(
        vec![out("order_id", &[(col("raw_orders", "id"), ID)])],
        &[],
        &["raw_orders"],
    );
    // Reads the opaque `py` and, by a parsed path, `orders`.
    let mixed = query(
        vec![
            out("order_id", &[(col("orders", "order_id"), ID)]),
            out("segment", &[(col("py", "segment"), ID)]),
        ],
        &[],
        &["orders", "py"],
    );
    // Counts the rows of `ext`, whose columns were never known: no column use recorded.
    let ext_count = query(vec![out("n", &[])], &[], &["ext"]);
    let analyzer = FakeSqlLineageAnalyzer::new()
        .with("orders.sql", orders)
        .with("customers.sql", customers)
        .with("rank.sql", rank)
        .with("report.sql", report)
        .with("all_customers.sql", all_customers)
        .with("summary.sql", summary)
        .with("events.sql", events)
        .with("mixed.sql", mixed)
        .with("ext_count.sql", ext_count);
    let model = |name: &str, deps: &[&str]| {
        LineageNode::new(format!("model.shop.{name}"), rel(name), NodeKind::Model)
            .with_sql(format!("{name}.sql"))
            .with_depends_on(deps.iter().map(|d| format!("model.shop.{d}")))
    };
    let mut py = LineageNode::new("model.shop.py", rel("py"), NodeKind::Model)
        .with_depends_on(["model.shop.orders"]);
    py.columns = Some(vec!["segment".into()]);
    let project =
        LineageProject::new(vec![
            LineageNode::new("seed.shop.raw_orders", rel("raw_orders"), NodeKind::Seed)
                .with_columns(["id", "customer_id", "status", "amount"]),
            LineageNode::new("model.shop.orders", rel("orders"), NodeKind::Model)
                .with_sql("orders.sql")
                .with_depends_on(["seed.shop.raw_orders"]),
            model("customers", &["orders"]),
            model("rank", &["orders"]),
            model("report", &["orders"]),
            model("all_customers", &["customers"]),
            py,
            model("summary", &["py"]),
            LineageNode::new("model.shop.events", rel("events"), NodeKind::Model)
                .with_sql("events.sql")
                .with_depends_on(["seed.shop.raw_orders"]),
            model("mixed", &["orders", "py"]),
            LineageNode::new("seed.shop.ext", rel("ext"), NodeKind::Seed),
            LineageNode::new("model.shop.ext_count", rel("ext_count"), NodeKind::Model)
                .with_sql("ext_count.sql")
                .with_depends_on(["seed.shop.ext"]),
        ]);
    build(&project, &analyzer, &MemoryCache::default())
        .unwrap()
        .0
}

fn names(graph: &ColumnGraph) -> BTreeMap<String, String> {
    graph
        .nodes()
        .map(|n| {
            let name = n.id.rsplit('.').next().unwrap_or(&n.id).to_owned();
            (n.id.clone(), name)
        })
        .collect()
}

fn catalog() -> CatalogInput {
    let mut orders = CatalogNode::new("model.shop.orders", "orders", "model");
    let mut amount = CatalogColumn::new("amount");
    amount.data_type = Some(("double".into(), TypeSource::Declared));
    orders.columns = vec![amount];
    let mut customers = CatalogNode::new("model.shop.customers", "customers", "model");
    customers.tests = vec![CatalogTest::new(
        "test.shop.unique_customers_customer_id",
        "unique",
        Some("customer_id".into()),
        TestKind::Data,
    )];
    CatalogInput::new(vec![orders, customers])
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn view(items: &[(&str, &str)]) -> ImpactView {
    let graph = graph();
    impact_view(
        &graph,
        &names(&graph),
        &catalog(),
        &ImpactQuery::from_pairs(&pairs(items)),
    )
}

fn verdicts(view: &ImpactView) -> BTreeMap<String, Verdict> {
    view.result
        .as_ref()
        .expect("a result")
        .must_run
        .iter()
        .map(|m| (m.name.clone(), m.verdict))
        .collect()
}

#[test]
fn a_dropped_column_breaks_the_sql_that_names_it_and_unknown_lineage_stays_unknown() {
    let view = view(&[("column", "orders.amount"), ("change", "drop")]);
    let verdicts = verdicts(&view);
    assert_eq!(
        verdicts,
        BTreeMap::from([
            ("orders".to_owned(), Verdict::Changed),
            ("customers".to_owned(), Verdict::Breaks),
            ("rank".to_owned(), Verdict::Breaks),
            // Opaque: may name it or not.
            ("py".to_owned(), Verdict::Unknown),
            // Reads the opaque model: can't be told either (AGENTS rule 3).
            ("summary".to_owned(), Verdict::Unknown),
            // Its input's values change, but it names nothing that goes away.
            ("all_customers".to_owned(), Verdict::Affected),
            // A parsed path from `orders` doesn't make it safe: it also reads `py`.
            ("mixed".to_owned(), Verdict::Unknown),
        ])
    );
    let result = view.result.as_ref().unwrap();
    let py = result.must_run.iter().find(|m| m.name == "py").unwrap();
    assert_eq!(py.lineage, LineageStatus::Opaque);
    assert_eq!(py.columns, None, "an opaque node's columns are unknown");
    let summary = result
        .must_run
        .iter()
        .find(|m| m.name == "summary")
        .unwrap();
    assert_eq!(summary.lineage, LineageStatus::Inferred);
    // The ones that break come first, after the changed model.
    assert_eq!(result.must_run[0].verdict, Verdict::Changed);
    assert_eq!(result.must_run[1].verdict, Verdict::Breaks);
    // `report` reads `orders` but not `amount`; `events` isn't downstream at all.
    assert_eq!(
        result
            .skipped
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["report"]
    );
    assert!(
        result.skipped[0].reason.contains("`amount`"),
        "{:?}",
        result.skipped
    );
    assert_eq!(
        result.not_downstream, 4,
        "raw_orders, events, ext, ext_count"
    );
    assert_eq!(
        result.selector,
        "ods state build -s all_customers -s customers -s mixed -s orders -s py -s rank -s summary"
    );
    assert!(result.selector_exact);
    let mixed = result.must_run.iter().find(|m| m.name == "mixed").unwrap();
    assert!(
        mixed.reasons[0].contains("It reads `py`"),
        "{:?}",
        mixed.reasons
    );
    assert_eq!(view.changes[0].current_type.as_deref(), Some("double"));
    assert_eq!(
        result
            .tests
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["unique"]
    );
}

#[test]
fn what_must_run_is_what_the_engine_says_for_every_change_kind() {
    let graph = graph();
    for items in [
        vec![("column", "orders.amount"), ("change", "drop")],
        vec![
            ("column", "orders.amount"),
            ("change", "retype"),
            ("to", "decimal(18,2)"),
        ],
        vec![
            ("column", "orders.amount"),
            ("change", "rename"),
            ("to", "total"),
        ],
        vec![("column", "customers.lifetime_value"), ("change", "drop")],
    ] {
        let view = impact_view(
            &graph,
            &names(&graph),
            &catalog(),
            &ImpactQuery::from_pairs(&pairs(&items)),
        );
        let result = view.result.unwrap();
        let engine: Vec<String> = graph
            .impact(&result.engine_changes)
            .node_ids()
            .map(str::to_owned)
            .collect();
        assert_eq!(result.reached, engine, "{items:?}");
        let shown: BTreeSet<&str> = result
            .must_run
            .iter()
            .filter(|m| m.verdict != Verdict::Changed)
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(
            shown,
            engine.iter().map(String::as_str).collect(),
            "every reached node is listed: {items:?}"
        );
    }
}

#[test]
fn a_type_change_removes_nothing_and_a_rename_breaks_like_a_drop() {
    let retype = view(&[
        ("column", "orders.amount"),
        ("change", "retype"),
        ("to", "decimal(18,2)"),
    ]);
    let retyped = verdicts(&retype);
    assert!(
        retyped
            .values()
            .all(|v| matches!(v, Verdict::Changed | Verdict::Affected | Verdict::Unknown)),
        "{retyped:?}"
    );
    assert_eq!(retyped["summary"], Verdict::Affected, "nothing goes away");
    let rename = view(&[
        ("column", "orders.amount"),
        ("change", "rename"),
        ("to", "total"),
    ]);
    let renamed = verdicts(&rename);
    assert_eq!(renamed["customers"], Verdict::Breaks);
    let customers = rename
        .result
        .as_ref()
        .unwrap()
        .must_run
        .iter()
        .find(|m| m.name == "customers")
        .unwrap();
    assert!(
        customers.reasons[0].contains("renamed to `total`"),
        "{:?}",
        customers.reasons
    );
}

#[test]
fn dropping_a_column_of_a_node_whose_columns_are_unknown_leaves_its_readers_unknown() {
    let dropped = view(&[("column", "ext.amount"), ("change", "drop")]);
    assert!(
        dropped.changes[0].note.is_some(),
        "says it's followed as all rows"
    );
    // Its SQL records no use of `amount`, but `ext`'s columns were never known.
    assert_eq!(verdicts(&dropped)["ext_count"], Verdict::Unknown);
    let retyped = view(&[("column", "ext.amount"), ("change", "retype")]);
    assert_eq!(verdicts(&retyped)["ext_count"], Verdict::Affected);
}

#[test]
fn a_column_passed_through_select_star_is_lost_downstream() {
    let view = view(&[("column", "customers.lifetime_value"), ("change", "drop")]);
    assert_eq!(verdicts(&view)["all_customers"], Verdict::LosesColumn);
}

#[test]
fn what_cant_be_simulated_says_why_and_shows_no_result() {
    for (items, problem) in [
        (
            vec![("column", "nope.amount"), ("change", "drop")],
            "no model `nope`",
        ),
        (
            vec![("column", "orders.nope"), ("change", "drop")],
            "`orders` has no column `nope`",
        ),
        (
            vec![("column", "orders"), ("change", "drop")],
            "is not MODEL.COLUMN",
        ),
        (
            vec![("column", "orders.amount"), ("change", "rename")],
            "give `amount` a new name",
        ),
        (
            vec![
                ("column", "orders.amount"),
                ("change", "rename"),
                ("to", "status"),
            ],
            "already has a column `status`",
        ),
        (
            vec![("column", "orders.amount"), ("change", "explode")],
            "unknown change `explode`",
        ),
    ] {
        let view = view(&items);
        assert!(view.result.is_none(), "{items:?}");
        let found = view.changes[0].problem.as_deref().unwrap_or_default();
        assert!(found.contains(problem), "{items:?}: {found}");
    }
    // A column without a change yet (from a column's link) waits for one, quietly.
    let view = view(&[("column", "orders.amount")]);
    assert!(view.result.is_none());
    assert_eq!(view.changes[0].problem, None);
    assert_eq!(view.changes[0].column.as_deref(), Some("amount"));
}

#[test]
fn the_form_adds_removes_and_caps_rows() {
    let query = ImpactQuery::from_pairs(&pairs(&[
        ("column", "orders.amount"),
        ("change-0", "drop"),
        ("to", ""),
        ("column", "orders.status"),
        ("change-1", "retype"),
        ("to", "int"),
        ("column", ""),
        ("to", ""),
        ("remove", "0"),
    ]));
    let graph = graph();
    let view = impact_view(&graph, &names(&graph), &catalog(), &query);
    assert_eq!(view.changes.len(), 1, "removed one, ignored the empty one");
    assert_eq!(view.changes[0].column.as_deref(), Some("status"));
    assert_eq!(view.changes[0].to.as_deref(), Some("int"));
    assert!(view.result.is_some());
    assert!(ImpactQuery::from_pairs(&pairs(&[("add", "1")])).add);
    let many: Vec<(&str, &str)> = (0..=MAX_CHANGES)
        .flat_map(|_| [("column", "orders.amount"), ("change", "retype")])
        .collect();
    let view = impact_view(
        &graph,
        &names(&graph),
        &catalog(),
        &ImpactQuery::from_pairs(&pairs(&many)),
    );
    assert!(view.cut);
    assert_eq!(view.changes.len(), MAX_CHANGES);
}

// ---------------------------------------------------------------------- served

fn snapshot(hostile: bool) -> Snapshot {
    let graph = graph();
    let name = |id: &str| {
        if hostile && id == "model.shop.rank" {
            HOSTILE.to_owned()
        } else {
            id.rsplit('.').next().unwrap_or(id).to_owned()
        }
    };
    let document = graph.document(&name, &GraphFilter::default());
    Snapshot::new(document, graph, "fixture")
        .with_dashboard(Dashboard::new("shop", "dev").with_catalog(catalog()))
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
fn the_page_shows_what_must_run_and_what_breaks_in_the_shell() {
    let addr = start(snapshot(false));
    let (status, page) = request(
        addr,
        "GET",
        "/lineage/impact?column=orders.amount&change-0=drop",
    );
    assert_eq!(status, 200);
    assert!(page.contains(r#"aria-current="page" data-section="lineage""#));
    assert!(
        page.contains(r#"aria-current="page" data-item="impact""#),
        "the Lineage section's page"
    );
    assert!(page.contains(r#"<tr data-node="model.shop.customers" data-verdict="breaks">"#));
    assert!(page.contains(r#"<tr data-node="model.shop.py" data-verdict="unknown">"#));
    assert!(
        page.contains(r#"value="drop" checked"#),
        "the form keeps the change"
    );
    assert!(page.contains(
        "ods state build -s all_customers -s customers -s mixed -s orders -s py -s rank -s summary"
    ));
    assert!(page.contains("2 known to break · 3 unknown"));
    assert!(page.contains(r#"href="../catalog/model.shop.customers""#));
    // Nothing asked: the form and what it does.
    let (status, page) = request(addr, "GET", "/lineage/impact");
    assert_eq!(status, 200);
    assert!(page.contains("nothing is run or written"));
    assert!(!page.contains("MUST RUN"));
    // A problem is shown on the form, and nothing is simulated.
    let (_, page) = request(
        addr,
        "GET",
        "/lineage/impact?column=orders.nope&change-0=drop",
    );
    assert!(page.contains(r#"role="alert""#) && page.contains("has no column"));
    assert!(!page.contains("MUST RUN"));
    // Read-only.
    assert_eq!(request(addr, "POST", "/lineage/impact").0, 405);
}

#[test]
fn the_api_is_the_view_model_versioned() {
    let addr = start(snapshot(false));
    let (status, body) = request(
        addr,
        "GET",
        "/api/lineage/impact?column=orders.amount&change=drop",
    );
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["schema_version"], 3);
    assert_eq!(json["changes"][0]["change"], "drop");
    let verdicts: BTreeMap<String, String> = json["result"]["must_run"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap().to_owned(),
                m["verdict"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(verdicts["customers"], "breaks");
    assert_eq!(verdicts["all_customers"], "affected");
    assert!(
        json.get("column_options").is_none(),
        "the picker's list stays in the page"
    );
}

#[test]
fn hostile_names_are_escaped() {
    let addr = start(snapshot(true));
    let (status, page) = request(
        addr,
        "GET",
        "/lineage/impact?column=orders.amount&change-0=drop",
    );
    assert_eq!(status, 200);
    assert!(!page.contains(HOSTILE), "never raw");
    assert!(page.contains("&lt;/script&gt;&lt;img src=x onerror=alert(1)&gt;"));
    let (_, page) = request(
        addr,
        "GET",
        "/lineage/impact?column=%3C%2Fscript%3E.x&change-0=drop",
    );
    assert!(!page.contains("</script>.x"), "the input is escaped too");
}

#[test]
fn indexed_changes_bind_to_their_row_whatever_the_order() {
    let view = view(&[
        ("column", "orders.amount"),
        ("column", "orders.status"),
        ("change-1", "retype"),
        ("change-0", "drop"),
    ]);
    assert_eq!(view.changes[0].change, Some(ChangeKind::Drop));
    assert_eq!(view.changes[1].change, Some(ChangeKind::Retype));
    assert!(view.result.is_some());
}

#[test]
fn shared_names_are_listed_and_linked_by_id() {
    let graph = graph();
    let mut names = names(&graph);
    // `rank` is also called `customers`, as a model of another package could be.
    names.insert("model.shop.rank".into(), "customers".into());
    let query = ImpactQuery::from_pairs(&pairs(&[("column", "customers.lifetime_value")]));
    let shared = impact_view(&graph, &names, &catalog(), &query);
    assert!(
        shared.changes[0]
            .problem
            .as_deref()
            .unwrap_or_default()
            .contains("names 2 nodes")
    );
    assert!(
        shared
            .column_options
            .contains(&"model.shop.customers.lifetime_value".to_owned())
    );
    assert!(
        shared
            .column_options
            .contains(&"model.shop.rank.amount".to_owned())
    );
    assert!(
        !shared
            .column_options
            .iter()
            .any(|o| o.starts_with("customers."))
    );
    // An id always works; with a unique name, the form shows the name instead.
    let by_id = impact_view(
        &graph,
        &names,
        &catalog(),
        &ImpactQuery::from_pairs(&pairs(&[("column", "model.shop.customers.lifetime_value")])),
    );
    assert_eq!(by_id.changes[0].problem, None);
    assert_eq!(
        by_id.changes[0].input,
        "model.shop.customers.lifetime_value"
    );
    let unique = view(&[("column", "model.shop.orders.amount")]);
    assert_eq!(unique.changes[0].input, "orders.amount");
}

#[test]
fn the_trail_is_capped_even_within_one_columns_uses() {
    // One column read by more readers than the trail shows.
    let readers = MAX_TRAIL + 5;
    let mut analyzer = FakeSqlLineageAnalyzer::new();
    let mut nodes =
        vec![LineageNode::new("seed.shop.wide", rel("wide"), NodeKind::Seed).with_columns(["x"])];
    for i in 0..readers {
        let name = format!("r{i:03}");
        analyzer = analyzer.with(
            format!("{name}.sql"),
            query(vec![out("x", &[(col("wide", "x"), ID)])], &[], &["wide"]),
        );
        nodes.push(
            LineageNode::new(format!("model.shop.{name}"), rel(&name), NodeKind::Model)
                .with_sql(format!("{name}.sql"))
                .with_depends_on(["seed.shop.wide"]),
        );
    }
    let graph = build(
        &LineageProject::new(nodes),
        &analyzer,
        &MemoryCache::default(),
    )
    .unwrap()
    .0;
    let view = impact_view(
        &graph,
        &names(&graph),
        &CatalogInput::default(),
        &ImpactQuery::from_pairs(&pairs(&[("column", "wide.x"), ("change", "retype")])),
    );
    let result = view.result.unwrap();
    assert_eq!(result.trail.len(), MAX_TRAIL);
    assert!(result.trail_cut);
    assert_eq!(
        result.reached.len(),
        readers,
        "the cap is on the trail only"
    );
}
