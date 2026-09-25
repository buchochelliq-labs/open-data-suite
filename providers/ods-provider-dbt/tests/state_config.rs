//! dbt State configs read from real dbt 1.10 and dbt v2 output (#168).

use std::path::{Path, PathBuf};

use ods_core::{LoadedAt, PolicyOrigin, Quorum};
use ods_provider_dbt::state_config::{StatePolicies, resolve};
use ods_provider_dbt::{ArtifactPreference, Artifacts};

fn artifacts(version: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dbt/jaffle-ods-state/artifacts")
        .join(version)
}

fn policies(version: &str, preference: ArtifactPreference) -> StatePolicies {
    resolve(
        &Artifacts::load_with(&artifacts(version), preference)
            .unwrap()
            .manifest,
    )
}

const H: u64 = 3_600;

fn lag_and_quorum(p: &StatePolicies, model: &str) -> (u64, Quorum) {
    let policy = &p.nodes[&format!("model.jaffle_ods.{model}")];
    (policy.lag_tolerance_secs, policy.require_fresh_data_from)
}

#[test]
fn every_format_reads_the_projects_dbt_state_configs() {
    for (version, preference) in [
        ("dbt-1.10", ArtifactPreference::Json),
        ("dbt-2.0", ArtifactPreference::Json),
        ("dbt-2.0", ArtifactPreference::InfoSchema),
    ] {
        let p = policies(version, preference);
        let context = format!("{version} {preference:?}");
        assert!(p.uses_dbt_state, "{context}");

        // Project level (`marts/`), inherited unchanged.
        assert_eq!(
            lag_and_quorum(&p, "customer_order_rank"),
            (4 * H, Quorum::All),
            "{context}"
        );
        // Nothing configured, but the project uses dbt State: dbt State's defaults.
        let stg = &p.nodes["model.jaffle_ods.stg_orders"];
        assert_eq!(
            (stg.lag_tolerance_secs, stg.require_fresh_data_from),
            (45 * 60, Quorum::Any)
        );
        assert!(
            matches!(stg.origin, PolicyOrigin::FormatDefault { .. }),
            "{context}"
        );
        assert!(stg.allows_reuse());
        // YAML `state:` plus SAO `build_after.updates_on: all`.
        assert_eq!(
            lag_and_quorum(&p, "customers"),
            (24 * H, Quorum::All),
            "{context}"
        );
        // Settings ODS can't honour yet keep these from ever being reused.
        assert!(
            !p.nodes["model.jaffle_ods.orders"].allows_reuse(),
            "{context}"
        );
        assert!(
            !p.nodes["model.jaffle_ods.order_events"].allows_reuse(),
            "{context}"
        );
        assert_eq!(lag_and_quorum(&p, "orders").0, 2 * H, "{context}");

        assert_eq!(
            p.sources["source.jaffle_ods.landing.orders_feed"],
            LoadedAt::Field("_loaded_at".into())
        );
        assert_eq!(
            p.sources["source.jaffle_ods.landing.events_feed"],
            LoadedAt::Query("select max(received_at) from main.events_feed".into())
        );
        assert_eq!(
            p.sources["source.jaffle_ods.landing.customers_feed"],
            LoadedAt::WarehouseMetadata
        );
    }
}

#[test]
fn dbt_1x_replaces_the_state_block_while_dbt_v2_merges_it() {
    // `orders` sets only `lag_tolerance` (and `evaluate_volatile_sql`) in SQL; the
    // project sets `require_fresh_data_from: all` for `marts/`.
    let v1 = policies("dbt-1.10", ArtifactPreference::Json);
    let v2 = policies("dbt-2.0", ArtifactPreference::Json);
    assert_eq!(lag_and_quorum(&v1, "orders"), (2 * H, Quorum::Any));
    assert_eq!(lag_and_quorum(&v2, "orders"), (2 * H, Quorum::All));
    // `order_events` sets only a hook setting: dbt 1.x loses the project's 4h.
    assert_eq!(lag_and_quorum(&v1, "order_events"), (45 * 60, Quorum::Any));
    assert_eq!(lag_and_quorum(&v2, "order_events"), (4 * H, Quorum::All));
}

#[test]
fn dbt_v2_json_and_parquet_resolve_identically() {
    assert_eq!(
        policies("dbt-2.0", ArtifactPreference::Json),
        policies("dbt-2.0", ArtifactPreference::InfoSchema)
    );
}

#[test]
fn projects_without_dbt_state_get_the_conservative_default() {
    let target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10");
    let p = resolve(&Artifacts::load(&target).unwrap().manifest);
    assert!(!p.uses_dbt_state);
    assert!(
        p.nodes
            .values()
            .all(|n| n.lag_tolerance_secs == 0 && n.origin == PolicyOrigin::ConservativeDefault)
    );
}
