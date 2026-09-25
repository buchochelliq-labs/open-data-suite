//! Keys and relationships from facts, with evidence, and optional inference.

use ods_erd::{
    Basis, BuildOptions, Cardinality, ColumnInput, EntityInput, EntityKind, Fact, build,
};

fn entity(id: &str, columns: &[&str]) -> EntityInput {
    EntityInput::new(
        id,
        id,
        EntityKind::Table,
        columns
            .iter()
            .map(|c| ColumnInput::new(*c, Some("integer".into())))
            .collect(),
    )
}

fn unique(entity: &str, column: &str) -> Fact {
    Fact::Unique {
        entity: entity.into(),
        columns: vec![column.into()],
        basis: Basis::Tested,
        evidence: format!("unique_{entity}_{column}"),
    }
}

fn not_null(entity: &str, column: &str) -> Fact {
    Fact::NotNull {
        entity: entity.into(),
        column: column.into(),
        basis: Basis::Tested,
        evidence: format!("not_null_{entity}_{column}"),
    }
}

fn relationship(from: &str, column: &str, to: &str, to_column: &str) -> Fact {
    Fact::ForeignKey {
        entity: from.into(),
        columns: vec![column.into()],
        to: to.into(),
        to_columns: vec![to_column.into()],
        basis: Basis::Tested,
        evidence: format!("relationships_{from}_{column}"),
    }
}

fn shop() -> Vec<EntityInput> {
    vec![
        entity("customers", &["customer_id", "name"]),
        entity("orders", &["order_id", "customer_id", "amount"]),
        entity("payments", &["payment_id", "order_id", "amount"]),
        entity("shipments", &["id", "order_id"]),
    ]
}

#[test]
fn unique_and_not_null_make_a_tested_primary_key_and_unique_alone_does_not() {
    let erd = build(
        &shop(),
        &[
            unique("customers", "customer_id"),
            not_null("customers", "customer_id"),
            unique("orders", "order_id"),
        ],
        BuildOptions::default(),
    );
    let customers = erd.entity("customers").unwrap();
    let pk = customers.primary_key.as_ref().unwrap();
    assert_eq!(
        (pk.columns.as_slice(), pk.basis),
        (&["customer_id".to_owned()][..], Basis::Tested)
    );
    assert!(
        pk.evidence
            .contains(&"unique_customers_customer_id".to_owned())
    );
    let orders = erd.entity("orders").unwrap();
    assert!(orders.primary_key.is_none(), "unique alone may be null");
    assert_eq!(orders.unique_keys[0].columns, ["order_id"]);
}

#[test]
fn relationships_get_cardinality_optionality_and_evidence() {
    let erd = build(
        &shop(),
        &[
            unique("customers", "customer_id"),
            not_null("customers", "customer_id"),
            relationship("orders", "customer_id", "customers", "customer_id"),
            not_null("orders", "customer_id"),
            // A unique reference is one-to-one.
            relationship("shipments", "order_id", "orders", "order_id"),
            unique("shipments", "order_id"),
        ],
        BuildOptions::default(),
    );
    let to_customers = &erd.relationships[0];
    assert_eq!(
        (to_customers.from.as_str(), to_customers.to.as_str()),
        ("orders", "customers")
    );
    assert_eq!(to_customers.cardinality, Cardinality::ManyToOne);
    assert!(!to_customers.optional, "the reference is tested not null");
    let shipment = erd
        .relationships
        .iter()
        .find(|r| r.from == "shipments")
        .unwrap();
    assert_eq!(shipment.cardinality, Cardinality::OneToOne);
    assert!(shipment.optional);
    assert!(erd.entity("orders").unwrap().columns[1].foreign_key);
}

#[test]
fn declared_keys_win_and_duplicates_merge_their_evidence() {
    let erd = build(
        &shop(),
        &[
            Fact::PrimaryKey {
                entity: "orders".into(),
                columns: vec!["ORDER_ID".into()],
                basis: Basis::Declared,
                evidence: "constraint primary_key".into(),
            },
            relationship("payments", "order_id", "orders", "order_id"),
            Fact::ForeignKey {
                entity: "payments".into(),
                columns: vec!["order_id".into()],
                to: "orders".into(),
                to_columns: vec!["order_id".into()],
                basis: Basis::Declared,
                evidence: "constraint foreign_key".into(),
            },
        ],
        BuildOptions::default(),
    );
    let pk = erd.entity("orders").unwrap().primary_key.clone().unwrap();
    assert_eq!(
        pk.columns,
        ["order_id"],
        "matched case-insensitively, entity spelling kept"
    );
    assert_eq!(erd.relationships.len(), 1);
    assert_eq!(erd.relationships[0].basis, Basis::Declared);
    assert_eq!(erd.relationships[0].evidence.len(), 2);
}

#[test]
fn facts_about_unknown_columns_are_reported_not_dropped_silently() {
    let erd = build(
        &shop(),
        &[
            unique("orders", "nope"),
            relationship("orders", "customer_id", "ghosts", "id"),
        ],
        BuildOptions::default(),
    );
    assert!(erd.relationships.is_empty());
    assert_eq!(erd.diagnostics.len(), 2, "{:?}", erd.diagnostics);
}

#[test]
fn inference_is_off_by_default_and_labelled_when_on() {
    let facts = [
        unique("customers", "customer_id"),
        not_null("customers", "customer_id"),
    ];
    assert!(
        build(&shop(), &facts, BuildOptions::default())
            .relationships
            .is_empty()
    );

    let erd = build(
        &shop(),
        &facts,
        BuildOptions::default().with_inference(true),
    );
    let inferred: Vec<(&str, &str, &str)> = erd
        .relationships
        .iter()
        .map(|r| (r.from.as_str(), r.from_columns[0].as_str(), r.to.as_str()))
        .collect();
    assert_eq!(
        inferred,
        [
            ("orders", "customer_id", "customers"),
            ("payments", "order_id", "orders"),
            ("shipments", "order_id", "orders")
        ]
    );
    assert!(erd.relationships.iter().all(|r| r.basis == Basis::Inferred));
    assert_eq!(
        erd.entity("shipments")
            .unwrap()
            .primary_key
            .as_ref()
            .unwrap()
            .basis,
        Basis::Inferred,
        "`id` looks like the key"
    );
}

#[test]
fn inference_prefers_tested_keys_and_reports_ambiguity() {
    let entities = vec![
        entity("stg_customers", &["customer_id"]),
        entity("customers", &["customer_id"]),
        entity("orders", &["order_id", "customer_id"]),
        entity("legacy_customers", &["customer_id"]),
    ];
    let tested = [
        unique("customers", "customer_id"),
        not_null("customers", "customer_id"),
    ];
    let erd = build(
        &entities,
        &tested,
        BuildOptions::default().with_inference(true),
    );
    let from_orders = erd
        .relationships
        .iter()
        .find(|r| r.from == "orders")
        .unwrap();
    assert_eq!(
        from_orders.to, "customers",
        "the tested key beats guessed ones"
    );

    let erd = build(&entities, &[], BuildOptions::default().with_inference(true));
    assert!(erd.relationships.iter().all(|r| r.from != "orders"));
    assert!(
        erd.diagnostics
            .iter()
            .any(|d| d.contains("several possible targets"))
    );
}

#[test]
fn focus_keeps_neighbours_within_depth() {
    let erd = build(
        &shop(),
        &[
            relationship("orders", "customer_id", "customers", "customer_id"),
            relationship("payments", "order_id", "orders", "order_id"),
        ],
        BuildOptions::default(),
    );
    let names =
        |e: &ods_erd::Erd| -> Vec<String> { e.entities.iter().map(|e| e.id.clone()).collect() };
    assert_eq!(
        names(&erd.focused(&["customers".into()], 1)),
        ["customers", "orders"]
    );
    assert_eq!(
        names(&erd.focused(&["customers".into()], 2)),
        ["customers", "orders", "payments"]
    );
    assert_eq!(
        names(&erd.connected_only()),
        ["customers", "orders", "payments"]
    );
}

#[test]
fn renders_mermaid_dot_and_json() {
    let erd = build(
        &shop(),
        &[
            unique("customers", "customer_id"),
            not_null("customers", "customer_id"),
            relationship("orders", "customer_id", "customers", "customer_id"),
        ],
        BuildOptions::default().with_inference(true),
    );
    let mermaid = erd.to_mermaid();
    assert!(mermaid.starts_with("erDiagram\n"));
    assert!(mermaid.contains("    customers {\n        integer customer_id PK \"tested key\"\n"));
    assert!(
        mermaid.contains("    orders }o--o| customers : \"customer_id\"\n"),
        "{mermaid}"
    );
    assert!(
        mermaid.contains("payments }o..o| orders : \"order_id (inferred)\""),
        "{mermaid}"
    );
    let dot = erd.to_dot();
    assert!(dot.starts_with("digraph erd {"));
    assert!(dot.contains("style=dashed"));
    let json: serde_json::Value = serde_json::from_str(&erd.to_json().unwrap()).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["relationships"][0]["basis"], "tested");
}
