use super::*;

use proptest::prelude::*;

fn at(t: &str) -> Timestamp {
    Timestamp::parse(t).unwrap()
}

fn model(id: &str, tests: usize) -> NodeFacts {
    let mut n = NodeFacts::new(id, id.rsplit('.').next().unwrap(), "model");
    n.tests = tests;
    n
}

fn built() -> BuildFacts {
    BuildFacts::new("run-1", at("2026-09-28T09:00:00Z"))
}

fn passed() -> BuildFacts {
    built().tested("run-1", at("2026-09-28T09:05:00Z"), true)
}

fn failures(failed: &[&str], skipped: &[&str]) -> LastFailures {
    LastFailures::new(
        at("2026-09-29T09:00:00Z"),
        "ods state build",
        failed.iter().map(|s| (*s).to_owned()),
        skipped.iter().map(|s| (*s).to_owned()),
    )
}

/// `[builtin.<id>]` with `severity`, as TOML.
fn severity_line(id: &str, severity: &str) -> String {
    ["[builtin.", id, "]\nseverity = \"", severity, "\"\n"].concat()
}

fn settings(toml: &str) -> HealthSettings {
    let config: HealthConfig = toml::from_str(toml).unwrap();
    HealthSettings::from_config(&config).unwrap()
}

fn health(settings: &HealthSettings, node: &NodeFacts, f: Option<&LastFailures>) -> Health {
    settings.evaluate(node, f).health
}

// ---------------------------------------------------------------- the defaults

#[test]
fn a_tested_build_is_healthy_and_says_why() {
    let mut node = model("model.a", 1);
    node.build = Some(passed());
    let badge = HealthSettings::default().evaluate(&node, None);
    assert_eq!(badge.health, Health::Healthy);
    assert_eq!(
        badge.reasons,
        [
            "built in run run-1 at 2026-09-28T09:00:00Z",
            "its tests passed in run run-1 at 2026-09-28T09:05:00Z"
        ]
    );
    assert_eq!(badge.findings.len(), BUILTINS.len(), "every check reports");
}

#[test]
fn never_built_is_unknown_never_healthy() {
    let node = model("model.a", 0);
    let badge = HealthSettings::default().evaluate(&node, None);
    assert_eq!(badge.health, Health::Unknown);
    assert_eq!(
        badge.reasons,
        ["never built by ODS: nothing to judge it on"]
    );
}

#[test]
fn untested_unpassed_or_changed_tests_warn() {
    let s = HealthSettings::default();
    let mut untested = model("model.a", 0);
    untested.build = Some(built());
    assert_eq!(health(&s, &untested, None), Health::Warning);

    let mut tested = model("model.b", 2);
    tested.build = Some(built());
    let badge = s.evaluate(&tested, None);
    assert_eq!(badge.health, Health::Warning);
    assert!(badge.reasons[0].contains("haven't been recorded passing"));

    tested.build = Some(built().tested("run-1", at("2026-09-28T09:05:00Z"), false));
    assert!(s.evaluate(&tested, None).reasons[0].contains("changed since"));
}

#[test]
fn a_seed_without_tests_is_healthy_once_built() {
    let mut seed = NodeFacts::new("seed.s", "s", "seed");
    seed.build = Some(built());
    let badge = HealthSettings::default().evaluate(&seed, None);
    assert_eq!(badge.health, Health::Healthy);
    assert_eq!(badge.reasons[1], "a seed has no tests to run");
}

#[test]
fn a_failure_counts_until_a_later_build_replaces_it() {
    let s = HealthSettings::default();
    let f = failures(&["model.a"], &[]);
    let mut node = model("model.a", 1);
    node.build = Some(passed());
    assert_eq!(health(&s, &node, Some(&f)), Health::Failing);
    // Never built and failed: failing, not unknown.
    node.build = None;
    assert_eq!(health(&s, &node, Some(&f)), Health::Failing);
    // Built after the failed run started: that build stands.
    node.build = Some(BuildFacts::new("run-2", at("2026-09-29T10:00:00Z")).tested(
        "run-2",
        at("2026-09-29T10:05:00Z"),
        true,
    ));
    assert_eq!(health(&s, &node, Some(&f)), Health::Healthy);
    // Skipped: a warning.
    node.build = Some(passed());
    let badge = s.evaluate(&node, Some(&failures(&[], &["model.a"])));
    assert_eq!(badge.health, Health::Warning);
    assert!(badge.reasons[0].starts_with("skipped in the last run"));
}

// ---------------------------------------------------------------- configuration

#[test]
fn a_check_turned_off_never_decides() {
    let s = settings("[builtin.tests_required]\nseverity = \"off\"");
    let mut node = model("model.a", 0);
    node.build = Some(built());
    let badge = s.evaluate(&node, None);
    assert_eq!(
        badge.health,
        Health::Healthy,
        "no tests is no longer checked"
    );
    assert!(badge.findings.iter().all(|f| f.check != "tests_required"));
    assert!(!s.enabled(Builtin::TestsRequired));
}

#[test]
fn severity_decides_what_a_failure_does() {
    let mut node = model("model.a", 0);
    node.build = Some(built());
    let error = settings("[builtin.tests_required]\nseverity = \"error\"");
    assert_eq!(health(&error, &node, None), Health::Failing);
    let info = settings("[builtin.tests_required]\nseverity = \"info\"");
    let badge = info.evaluate(&node, None);
    assert_eq!(
        badge.health,
        Health::Healthy,
        "info is shown, and changes nothing"
    );
    assert!(badge.findings.iter().any(|f| f.check == "tests_required"
        && f.status == Status::Fail
        && f.severity == Severity::Info));
}

#[test]
fn selectors_scope_a_check_by_type_tag_path_and_name() {
    let s = settings(
        r#"
        [builtin.tests_required]
        select = { path = ["models/marts/**"] }
        exclude = { tags = ["experimental"] }
        "#,
    );
    let mut mart = model("model.orders", 0);
    mart.build = Some(built());
    mart.path = Some("models/marts/orders.sql".into());
    assert_eq!(
        health(&s, &mart, None),
        Health::Warning,
        "a mart needs tests"
    );

    let mut staging = mart.clone();
    staging.path = Some("models/staging/stg_orders.sql".into());
    assert_eq!(health(&s, &staging, None), Health::Healthy, "not selected");

    let mut experimental = mart.clone();
    experimental.tags = vec!["experimental".into()];
    assert_eq!(health(&s, &experimental, None), Health::Healthy, "excluded");

    // A node without a known path never matches a path selector.
    let mut unknown_path = mart.clone();
    unknown_path.path = None;
    assert_eq!(health(&s, &unknown_path, None), Health::Healthy);

    let by_name = settings("[builtin.tests_required]\nselect = { name = [\"orders\"] }");
    assert_eq!(health(&by_name, &mart, None), Health::Warning);
    // Selecting replaces the default scope: seeds can be required to have tests too.
    let seeds = settings("[builtin.tests_required]\nselect = { resource_type = [\"seed\"] }");
    let mut seed = NodeFacts::new("seed.s", "s", "seed");
    seed.build = Some(built());
    assert_eq!(health(&seeds, &seed, None), Health::Warning);
}

#[test]
fn unknown_can_count_as_a_warning_never_as_healthy() {
    let s = settings("unknown_counts_as = \"warning\"");
    let badge = s.evaluate(&model("model.a", 0), None);
    assert_eq!(badge.health, Health::Warning);
    assert!(badge.reasons.iter().any(|r| r.starts_with("never built")));
    assert_eq!(s.unknown_counts_as(), Health::Warning);
}

#[test]
fn with_built_off_a_never_built_node_is_judged_on_the_rest() {
    let s = settings("[builtin.built]\nseverity = \"off\"");
    let node = model("model.a", 1);
    // Never built: its tests can't have passed on a build, but it has tests.
    assert_eq!(health(&s, &node, None), Health::Healthy);
    assert!(s.how(true).contains("built (off)"));
}

#[test]
fn with_every_check_off_nothing_is_healthy() {
    let all_off: String = BUILTINS
        .iter()
        .map(|b| severity_line(b.id(), "off"))
        .collect();
    let s = settings(&all_off);
    let mut node = model("model.a", 1);
    node.build = Some(passed());
    let badge = s.evaluate(&node, None);
    assert_eq!(badge.health, Health::Unknown);
    assert_eq!(badge.reasons, ["no enabled check applies to it"]);
}

#[test]
fn a_misnamed_check_or_a_bad_glob_is_a_configuration_error() {
    let config: HealthConfig =
        toml::from_str("[builtin.test_required]\nseverity = \"off\"").unwrap();
    let error = HealthSettings::from_config(&config)
        .unwrap_err()
        .to_string();
    assert!(error.contains("health.builtin.test_required"), "{error}");
    assert!(
        error.contains("tests_required"),
        "lists the real ones: {error}"
    );

    let config: HealthConfig =
        toml::from_str("[builtin.built]\nselect = { path = [\"models/[\"] }").unwrap();
    let error = HealthSettings::from_config(&config)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("health.builtin.built.select.path"),
        "{error}"
    );

    // Unknown keys are refused by the configuration itself (ADR-0005).
    assert!(toml::from_str::<HealthConfig>("[builtin.built]\nseverty = \"off\"").is_err());
    assert!(toml::from_str::<HealthConfig>("[builtin.built]\nseverity = \"fatal\"").is_err());
}

#[test]
fn how_describes_the_configuration() {
    let s = settings("[builtin.tests_required]\nseverity = \"off\"");
    let how = s.how(false);
    assert!(how.contains("tests_required (off)"), "{how}");
    assert!(how.contains("last_run_failed (error)"), "{how}");
    assert!(how.contains("built (on)"), "it never fails: {how}");
    assert!(how.contains("failures aren't measured"), "{how}");
    assert!(!s.how(true).contains("failures aren't measured"));
}

#[test]
fn counts_list_every_health() {
    let s = HealthSettings::default();
    let badges = [s.evaluate(&model("model.a", 0), None)];
    let counts = counts(&badges);
    assert_eq!(counts.len(), 4);
    assert_eq!(counts[&Health::Unknown], 1);
    assert_eq!(counts[&Health::Healthy], 0);
}

// ---------------------------------------------------------------- properties

fn severity() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just("error"), Just("warn"), Just("info"), Just("off")]
}

proptest! {
    /// Whatever else is configured, while the `built` check is on, a node ODS never built
    /// is never healthy: nothing vouches for it (AGENTS rule 3). Turning `built` off is
    /// the explicit choice to judge nodes on the other checks alone.
    #[test]
    fn a_node_nothing_vouches_for_is_never_healthy(
        severities in proptest::collection::vec(severity(), BUILTINS.len()),
        tests in 0usize..3,
        kind in prop_oneof![Just("model"), Just("seed"), Just("snapshot")],
        warning in any::<bool>(),
    ) {
        let mut config = String::new();
        if warning {
            config.push_str("unknown_counts_as = \"warning\"\n");
        }
        for (b, s) in BUILTINS.iter().zip(&severities) {
            // `built` on, at any severity: it can't fail, only be unknown.
            let s = if *b == Builtin::Built && *s == "off" { "warn" } else { s };
            config.push_str(&severity_line(b.id(), s));
        }
        let s = settings(&config);
        let mut node = NodeFacts::new("x.y", "y", kind);
        node.tests = tests;
        prop_assert_ne!(health(&s, &node, None), Health::Healthy);
    }

    /// A badge's reasons always come from its findings, and its findings are only the
    /// enabled checks, in order.
    #[test]
    fn a_badge_explains_itself_from_its_findings(
        severities in proptest::collection::vec(severity(), BUILTINS.len()),
        tests in 0usize..3,
        built in any::<bool>(),
        failed in any::<bool>(),
    ) {
        let mut config = String::new();
        for (b, s) in BUILTINS.iter().zip(&severities) {
            config.push_str(&severity_line(b.id(), s));
        }
        let s = settings(&config);
        let mut node = model("model.a", tests);
        if built {
            node.build = Some(passed());
        }
        let f = failures(if failed { &["model.a"] } else { &[] }, &[]);
        let badge = s.evaluate(&node, Some(&f));
        let enabled: Vec<&str> = BUILTINS
            .iter()
            .zip(&severities)
            .filter(|(_, s)| **s != "off")
            .map(|(b, _)| b.id())
            .collect();
        let checks: Vec<&str> = badge.findings.iter().map(|f| f.check).collect();
        prop_assert_eq!(checks, enabled);
        for reason in &badge.reasons {
            prop_assert!(
                badge.findings.iter().any(|f| &f.reason == reason)
                    || reason == "no enabled check applies to it",
                "{} isn't a finding's", reason
            );
        }
    }
}
