//! Exact selection (#211): selectors reach exactly the requested nodes.

use std::path::Path;

use ods_provider_dbt::selection::exact_selectors;
use ods_provider_dbt::{Artifacts, Manifest};

fn manifest() -> Manifest {
    Artifacts::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-build"),
    )
    .unwrap()
    .manifest
}

fn ids(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

/// A copy of `node` under another id, fqn and file.
fn add(m: &mut Manifest, like: &str, id: &str, fqn: &[&str], file: &str) {
    let mut node = m
        .nodes
        .iter()
        .find(|n| n.unique_id == like)
        .unwrap()
        .clone();
    id.clone_into(&mut node.unique_id);
    node.fqn = fqn.iter().map(|p| (*p).to_owned()).collect();
    node.original_file_path = Some(file.to_owned());
    m.nodes.push(node);
}

#[test]
fn one_node_is_selected_by_its_full_fqn_and_type() {
    let m = manifest();
    assert_eq!(
        exact_selectors(&m, &ids(&["model.jaffle_ods.orders"])).unwrap(),
        ["fqn:jaffle_ods.marts.orders,resource_type:model"]
    );
}

#[test]
fn a_fully_requested_folder_is_selected_as_a_whole() {
    let m = manifest();
    let staging = ids(&[
        "model.jaffle_ods.stg_customers",
        "model.jaffle_ods.stg_orders",
        "model.jaffle_ods.stg_payments",
    ]);
    assert_eq!(
        exact_selectors(&m, &staging).unwrap(),
        ["fqn:jaffle_ods.staging,resource_type:model"]
    );
    let everything: Vec<String> = m
        .nodes
        .iter()
        .filter(|n| n.unique_id.starts_with("model.") || n.unique_id.starts_with("seed."))
        .map(|n| n.unique_id.clone())
        .collect();
    assert_eq!(
        exact_selectors(&m, &everything).unwrap(),
        [
            "fqn:jaffle_ods,resource_type:model",
            "fqn:jaffle_ods,resource_type:seed"
        ]
    );
}

#[test]
fn a_folder_named_like_the_model_is_left_out_by_the_file() {
    let mut m = manifest();
    // models/marts/orders/orders_detail.sql: `fqn:jaffle_ods.marts.orders` reaches it.
    add(
        &mut m,
        "model.jaffle_ods.orders",
        "model.jaffle_ods.orders_detail",
        &["jaffle_ods", "marts", "orders", "orders_detail"],
        "models/marts/orders/orders_detail.sql",
    );
    assert_eq!(
        exact_selectors(&m, &ids(&["model.jaffle_ods.orders"])).unwrap(),
        ["path:models/marts/orders.sql,fqn:jaffle_ods.marts.orders,resource_type:model"]
    );
    // A model and a seed sharing a name don't collide: the type tells them apart.
    add(
        &mut m,
        "seed.jaffle_ods.raw_orders",
        "seed.jaffle_ods.orders",
        &["jaffle_ods", "orders"],
        "seeds/orders.csv",
    );
    assert_eq!(
        exact_selectors(&m, &ids(&["seed.jaffle_ods.orders"])).unwrap(),
        ["fqn:jaffle_ods.orders,resource_type:seed"]
    );
}

#[test]
fn a_package_node_that_cant_be_selected_exactly_is_refused() {
    let mut m = manifest();
    add(
        &mut m,
        "model.jaffle_ods.orders",
        "model.pkg.events",
        &["pkg", "events"],
        "models/events.sql",
    );
    add(
        &mut m,
        "model.jaffle_ods.orders",
        "model.pkg.events_daily",
        &["pkg", "events", "events_daily"],
        "models/events/events_daily.sql",
    );
    let err = exact_selectors(&m, &ids(&["model.pkg.events"])).unwrap_err();
    assert!(err.contains("model.pkg.events_daily"), "{err}");
    assert!(exact_selectors(&m, &ids(&["model.jaffle_ods.nope"])).is_err());
}

#[test]
fn a_thousand_nodes_make_a_short_command() {
    let mut m = manifest();
    let mut wanted = Vec::new();
    for i in 0..1000 {
        let id = format!("model.jaffle_ods.gen_{i}");
        add(
            &mut m,
            "model.jaffle_ods.orders",
            &id,
            &["jaffle_ods", "generated", &format!("gen_{i}")],
            &format!("models/generated/gen_{i}.sql"),
        );
        wanted.push(id);
    }
    let selectors = exact_selectors(&m, &wanted).unwrap();
    assert_eq!(selectors, ["fqn:jaffle_ods.generated,resource_type:model"]);
}

/// Architecture review of #211: dbt also matches the fqn without its package.
#[test]
fn a_folder_named_like_a_package_is_not_selected_with_it() {
    let mut m = manifest();
    // A package `stripe`, and the root project's `models/stripe/` folder.
    add(
        &mut m,
        "model.jaffle_ods.orders",
        "model.stripe.stripe_charges",
        &["stripe", "stripe_charges"],
        "models/stripe_charges.sql",
    );
    add(
        &mut m,
        "model.jaffle_ods.orders",
        "model.jaffle_ods.fct_revenue",
        &["jaffle_ods", "stripe", "fct_revenue"],
        "models/stripe/fct_revenue.sql",
    );
    // `fqn:stripe` would reach `fct_revenue` too; the full fqn doesn't.
    assert_eq!(
        exact_selectors(&m, &ids(&["model.stripe.stripe_charges"])).unwrap(),
        ["fqn:stripe.stripe_charges,resource_type:model"]
    );
    // Asked for both, `fqn:stripe` is fine.
    assert_eq!(
        exact_selectors(
            &m,
            &ids(&[
                "model.stripe.stripe_charges",
                "model.jaffle_ods.fct_revenue"
            ])
        )
        .unwrap(),
        ["fqn:stripe,resource_type:model"]
    );

    // The root project's `models/jaffle_ods/marts/orders/extra.sql`.
    let mut m = manifest();
    add(
        &mut m,
        "model.jaffle_ods.orders",
        "model.jaffle_ods.extra",
        &["jaffle_ods", "jaffle_ods", "marts", "orders", "extra"],
        "models/jaffle_ods/marts/orders/extra.sql",
    );
    assert_eq!(
        exact_selectors(&m, &ids(&["model.jaffle_ods.orders"])).unwrap(),
        ["path:models/marts/orders.sql,fqn:jaffle_ods.marts.orders,resource_type:model"]
    );
}

/// dbt splits `--select` on spaces and intersects on commas.
#[test]
fn folder_names_dbt_would_split_are_never_used_in_selectors() {
    let mut m = manifest();
    add(
        &mut m,
        "model.jaffle_ods.orders",
        "model.jaffle_ods.legacy_orders",
        &["jaffle_ods", "old marts", "legacy_orders"],
        "models/old marts/legacy_orders.sql",
    );
    add(
        &mut m,
        "model.jaffle_ods.orders",
        "model.jaffle_ods.ab",
        &["jaffle_ods", "a,b", "ab"],
        "models/a,b/ab.sql",
    );
    for id in ["model.jaffle_ods.legacy_orders", "model.jaffle_ods.ab"] {
        let err = exact_selectors(&m, &ids(&[id])).unwrap_err();
        assert!(err.contains("can be selected safely"), "{err}");
    }
    // Others are unaffected, and never compressed into a split-prone selector.
    assert_eq!(
        exact_selectors(&m, &ids(&["model.jaffle_ods.orders"])).unwrap(),
        ["fqn:jaffle_ods.marts.orders,resource_type:model"]
    );
}

#[test]
fn a_node_without_an_fqn_blocks_selections_that_could_reach_it() {
    let mut m = manifest();
    add(
        &mut m,
        "model.jaffle_ods.orders",
        "model.jaffle_ods.unknown",
        &[],
        "models/unknown.sql",
    );
    // It could be anywhere: the root project's nodes fall back to their file.
    assert_eq!(
        exact_selectors(&m, &ids(&["model.jaffle_ods.orders"])).unwrap(),
        ["path:models/marts/orders.sql,fqn:jaffle_ods.marts.orders,resource_type:model"]
    );
}
