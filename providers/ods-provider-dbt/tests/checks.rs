//! The digest of a node's checks (#220): it changes with their definitions, and not
//! with what dbt adds to a test's macros at run time.

use std::path::Path;

use ods_provider_dbt::Manifest;
use ods_provider_dbt::fingerprint::checks_digest;

const ORDERS: &str = "model.jaffle_ods.orders";

fn manifest() -> Manifest {
    Manifest::read(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-build/manifest.json"),
    )
    .unwrap()
}

fn test_on_orders(m: &mut Manifest) -> &mut ods_provider_dbt::ManifestNode {
    m.nodes
        .iter_mut()
        .find(|n| {
            n.unique_id
                .starts_with("test.jaffle_ods.unique_orders_order_id")
        })
        .unwrap()
}

#[test]
fn a_nodes_checks_digest_follows_their_definitions_only() {
    let base = manifest();
    let digest = checks_digest(&base, ORDERS).expect("orders has tests");
    assert_eq!(checks_digest(&base, "seed.jaffle_ods.raw_orders"), None);

    // `dbt test` lists macros it resolved at run time; `dbt compile` doesn't.
    let mut at_run_time = base.clone();
    test_on_orders(&mut at_run_time)
        .depends_on_macros
        .push("macro.dbt.get_limit_subquery".to_owned());
    assert_eq!(
        checks_digest(&at_run_time, ORDERS).as_deref(),
        Some(digest.as_str())
    );

    let mut edited = base.clone();
    let test = test_on_orders(&mut edited).test.as_mut().unwrap();
    test.arguments["where"] = serde_json::json!("order_id > 0");
    assert_ne!(
        checks_digest(&edited, ORDERS).as_deref(),
        Some(digest.as_str())
    );

    let mut removed = base.clone();
    removed.nodes.retain(|n| {
        !n.unique_id
            .starts_with("test.jaffle_ods.unique_orders_order_id")
    });
    assert_ne!(
        checks_digest(&removed, ORDERS).as_deref(),
        Some(digest.as_str())
    );
}
