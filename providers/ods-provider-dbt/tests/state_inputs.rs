//! What State reads from dbt: fingerprints, run results and source freshness (ADR-0013).

use std::path::{Path, PathBuf};

use ods_provider_dbt::fingerprint::fingerprint;
use ods_provider_dbt::{Artifacts, Manifest, RunResults, RunStatus, SourceFreshness};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10")
}

fn manifest() -> Manifest {
    Artifacts::load(&fixture()).unwrap().manifest
}

fn node<'m>(m: &'m Manifest, id: &str) -> &'m ods_provider_dbt::ManifestNode {
    m.nodes.iter().find(|n| n.unique_id == id).unwrap()
}

#[test]
fn fingerprints_are_reproducible_and_name_their_components() {
    let m = manifest();
    let orders = fingerprint(&m, node(&m, "model.jaffle_ods.orders")).unwrap();
    let again = fingerprint(&manifest(), node(&manifest(), "model.jaffle_ods.orders")).unwrap();
    assert_eq!(orders, again);
    let names: Vec<&str> = orders.components.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "config", "contract", "engine", "macros", "relation", "scheme", "sql"
        ]
    );
    let seed = fingerprint(&m, node(&m, "seed.jaffle_ods.raw_orders")).unwrap();
    assert!(!seed.components.contains_key("sql"), "seeds have no SQL");
    assert!(seed.components.contains_key("file"), "a seed is its CSV");
    let python = fingerprint(&m, node(&m, "model.jaffle_ods.customer_segments")).unwrap();
    assert!(
        python.components.contains_key("file") && python.components.contains_key("compiled_code"),
        "Python isn't normalised: {:?}",
        python.components.keys()
    );
    assert_eq!(m.project_name.as_deref(), Some("jaffle_ods"));
}

fn with_sql(node: &ods_provider_dbt::ManifestNode, sql: &str) -> ods_provider_dbt::ManifestNode {
    let mut node = node.clone();
    node.compiled_code = Some(sql.to_owned());
    node
}

#[test]
fn formatting_only_edits_keep_the_fingerprint_and_are_recorded_as_cosmetic() {
    let m = manifest();
    let orders = node(&m, "model.jaffle_ods.orders");
    let sql = orders.compiled_code.clone().unwrap();
    let base = fingerprint(&m, orders).unwrap();

    // A comment and reindenting: same fingerprint, different raw text.
    let reformatted = format!(
        "-- orders, one row per order\n{}\n/* end */",
        sql.replace('\n', "\n    ")
    );
    let mut edited = with_sql(orders, &reformatted);
    // dbt's checksum of the file changes with any edit; it isn't part of a SQL model's
    // fingerprint.
    edited.checksum = Some("edited".to_owned());
    let after = fingerprint(&m, &edited).unwrap();
    assert_eq!(after.digest, base.digest);
    assert_eq!(after.cosmetic_changes(&base), ["sql"]);

    // A real change is still a change.
    let changed = fingerprint(&m, &with_sql(orders, &format!("{sql} where 1 = 0"))).unwrap();
    assert_eq!(changed.diff(&base).changed, ["sql"]);

    // Keyword case counts: some dialects let keywords be case-sensitive names.
    let upper = fingerprint(&m, &with_sql(orders, &sql.replace("select", "SELECT"))).unwrap();
    assert_eq!(upper.diff(&base).changed, ["sql"]);

    // Jinja that runs SQL of its own keeps the file in the fingerprint.
    let mut side_effect = orders.clone();
    side_effect.raw_code = Some("{% do run_query('delete from audit') %}\nselect 1".into());
    assert!(
        fingerprint(&m, &side_effect)
            .unwrap()
            .components
            .contains_key("file")
    );

    // SQL that can't be normalised safely is hashed as is.
    let dollar = format!("{sql} -- $");
    let raw = fingerprint(&m, &with_sql(orders, &format!("{sql}\nwhere $1 = 1"))).unwrap();
    let raw_reformatted =
        fingerprint(&m, &with_sql(orders, &format!("{sql}\nwhere  $1 = 1"))).unwrap();
    assert_ne!(raw.digest, raw_reformatted.digest);
    assert_eq!(
        fingerprint(&m, &with_sql(orders, &dollar)).unwrap().digest,
        base.digest,
        "a `$` in a comment is fine"
    );
}

#[test]
fn tags_and_policies_do_not_change_a_fingerprint_but_materialization_does() {
    let m = manifest();
    let base = fingerprint(&m, node(&m, "model.jaffle_ods.orders")).unwrap();
    let mut changed = node(&m, "model.jaffle_ods.orders").clone();
    changed.config.raw["tags"] = serde_json::json!(["nightly"]);
    changed.config.raw["state"] = serde_json::json!({"lag_tolerance": "4h"});
    assert_eq!(fingerprint(&m, &changed).unwrap(), base);
    changed.config.raw["materialized"] = serde_json::json!("incremental");
    let diff = fingerprint(&m, &changed).unwrap().diff(&base);
    assert_eq!(diff.changed, ["config"]);
}

#[test]
fn a_model_without_compiled_sql_cannot_be_fingerprinted() {
    let m = manifest();
    let mut parsed_only = node(&m, "model.jaffle_ods.orders").clone();
    parsed_only.compiled_code = None;
    let why = fingerprint(&m, &parsed_only).unwrap_err();
    assert!(why.contains("dbt compile"), "{why}");
}

#[test]
fn run_results_give_status_and_completion_per_node() {
    let run = RunResults::read(&fixture().join("run_results.json")).unwrap();
    assert!(run.invocation_id.is_some());
    assert_eq!(
        run.command.as_deref(),
        Some("generate"),
        "the fixture's run is `dbt docs generate`"
    );
    assert!(!run.empty);
    assert!(run.started_at.is_some());
    let orders = run
        .results
        .iter()
        .find(|r| r.unique_id == "model.jaffle_ods.orders")
        .unwrap();
    assert_eq!(orders.status, RunStatus::Success);
    assert!(orders.completed_at.is_some());
}

fn temp(name: &str, content: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ods-dbt-{}-{name}", std::process::id()));
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn source_freshness_keeps_measured_sources_and_reports_errors() {
    let path = temp(
        "sources.json",
        r#"{
          "metadata": {"dbt_schema_version": "https://schemas.getdbt.com/dbt/sources/v3.json",
                       "generated_at": "2026-09-25T03:00:00.000000Z", "invocation_id": "x"},
          "results": [
            {"unique_id": "source.p.raw.orders", "status": "pass",
             "max_loaded_at": "2026-09-25T02:59:00+00:00", "snapshotted_at": "2026-09-25T03:00:00+00:00"},
            {"unique_id": "source.p.raw.users", "status": "runtime error", "error": "no such column"}
          ],
          "elapsed_time": 1.0
        }"#,
    );
    let freshness = SourceFreshness::read(&path).unwrap();
    std::fs::remove_file(&path).ok();
    assert_eq!(
        freshness.generated_at.as_deref(),
        Some("2026-09-25T03:00:00.000000Z")
    );
    assert_eq!(
        freshness.max_loaded_at["source.p.raw.orders"],
        "2026-09-25T02:59:00+00:00"
    );
    assert_eq!(freshness.errors["source.p.raw.users"], "runtime error");
}

#[test]
fn unsupported_run_results_versions_are_errors() {
    let path = temp(
        "run_results.json",
        r#"{"metadata": {"dbt_schema_version": "https://schemas.getdbt.com/dbt/run-results/v99.json"}, "results": []}"#,
    );
    let error = RunResults::read(&path).unwrap_err().to_string();
    std::fs::remove_file(&path).ok();
    assert!(error.contains("v99"), "{error}");
}

#[test]
fn content_that_dbt_did_not_hash_cannot_be_fingerprinted() {
    let m = manifest();
    let mut big_seed = node(&m, "seed.jaffle_ods.raw_orders").clone();
    big_seed.checksum = None; // what `{name: "path", …}` reads as
    assert!(fingerprint(&m, &big_seed).unwrap_err().contains("1 MiB"));

    let mut no_config = node(&m, "model.jaffle_ods.orders").clone();
    no_config.config.raw = serde_json::Value::Null;
    assert!(fingerprint(&m, &no_config).is_err());

    let mut unknown_macro = node(&m, "model.jaffle_ods.orders").clone();
    unknown_macro
        .depends_on_macros
        .push("macro.jaffle_ods.not_there".into());
    assert!(
        fingerprint(&m, &unknown_macro)
            .unwrap_err()
            .contains("not_there")
    );
}

#[test]
fn a_materialization_or_naming_macro_change_changes_the_fingerprint() {
    let base_manifest = manifest();
    let orders = node(&base_manifest, "model.jaffle_ods.orders").clone();
    let base = fingerprint(&base_manifest, &orders).unwrap();
    let materialization = base_manifest
        .macros
        .keys()
        .find(|id| id.ends_with("materialization_table_default"))
        .expect("dbt's table materialization is in the manifest")
        .clone();
    let mut edited = manifest();
    edited
        .macros
        .get_mut(&materialization)
        .unwrap()
        .sql
        .push_str("\n-- patched");
    let diff = fingerprint(&edited, &orders).unwrap().diff(&base);
    assert_eq!(diff.changed, ["macros"]);

    let mut renamed = orders.clone();
    renamed.relation_name = Some("\"jaffle_ods\".\"dev\".\"orders\"".into());
    assert_eq!(
        fingerprint(&base_manifest, &renamed)
            .unwrap()
            .diff(&base)
            .changed,
        ["relation"]
    );
}

/// `orders` with these post-hooks.
fn hooked(m: &Manifest, hooks: &[&str]) -> ods_provider_dbt::ManifestNode {
    let mut node = node(m, "model.jaffle_ods.orders").clone();
    node.config.raw["post-hook"] = serde_json::Value::Array(
        hooks
            .iter()
            .map(|sql| serde_json::json!({"sql": sql, "transaction": true, "index": null}))
            .collect(),
    );
    node
}

#[test]
fn hooks_that_read_environment_variables_fingerprint_their_values() {
    use ods_provider_dbt::fingerprint::fingerprint_with_env;
    let m = manifest();
    let env = |role: &'static str| move |name: &str| (name == "HOOK_ROLE").then(|| role.to_owned());

    // `this` alone: no values to track.
    let plain = fingerprint_with_env(
        &m,
        &hooked(&m, &["grant select on {{ this }} to x"]),
        &env("a"),
    )
    .unwrap();
    assert!(!plain.components.contains_key("hook_env"));

    let grant = hooked(
        &m,
        &["grant select on {{ this }} to {{ env_var('HOOK_ROLE') }}"],
    );
    let a = fingerprint_with_env(&m, &grant, &env("analyst")).unwrap();
    let b = fingerprint_with_env(&m, &grant, &env("admin")).unwrap();
    assert!(a.components.contains_key("hook_env"));
    assert_eq!(b.diff(&a).changed, ["hook_env"]);
    // Only a digest of the value is kept.
    assert!(!serde_json::to_string(&a).unwrap().contains("analyst"));
}

#[test]
fn hooks_calling_a_macro_that_reads_the_environment_are_tracked_too() {
    use ods_provider_dbt::fingerprint::fingerprint_with_env;
    let mut m = manifest();
    let mut grant = m.macros.values().next().unwrap().clone();
    grant.sql = "{% macro grant_to_role(rel) %}grant select on {{ rel }} to {{ env_var(\"HOOK_ROLE\", \"x\") }}{% endmacro %}".to_owned();
    grant.depends_on = Vec::new();
    m.macros
        .insert("macro.jaffle_ods.grant_to_role".to_owned(), grant);
    let node = hooked(&m, &["{{ grant_to_role(this) }}"]);
    let a = fingerprint_with_env(&m, &node, &|_| Some("a".to_owned())).unwrap();
    let b = fingerprint_with_env(&m, &node, &|_| None).unwrap();
    assert_eq!(b.diff(&a).changed, ["hook_env"]);
}

#[test]
fn hooks_whose_values_ods_cant_see_always_build() {
    use ods_provider_dbt::fingerprint::fingerprint_with_env;
    let m = manifest();
    let env = |_: &str| Some("v".to_owned());
    for (hook, expect) in [
        (
            "grant select on {{ this }} to {{ var('reporting_role') }}",
            "var(reporting_role)",
        ),
        ("select '{{ env_var('DBT_ENV_SECRET_TOKEN') }}'", "secret"),
        ("select '{{ env_var(name) }}'", "can't name"),
    ] {
        let why = fingerprint_with_env(&m, &hooked(&m, &[hook]), &env).unwrap_err();
        assert!(why.contains(expect), "{hook}: {why}");
    }
    // `env_var` isn't mistaken for `var`, and `var` in the model's SQL is fine: it is
    // rendered into the compiled SQL.
    let mut in_sql = node(&m, "model.jaffle_ods.orders").clone();
    in_sql.raw_code = Some("select {{ var('x', 1) }} as x".to_owned());
    assert!(fingerprint_with_env(&m, &in_sql, &env).is_ok());
}
