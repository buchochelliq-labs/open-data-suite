use super::*;
use ods_core::state::Timestamp;

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

/// A record of the last run in which nothing failed.
fn clean() -> LastFailures {
    failures(&[], &[])
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
    let badge = HealthSettings::default().evaluate(&node, Some(&clean()));
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
    let badge = HealthSettings::default().evaluate(&node, Some(&clean()));
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
    assert_eq!(health(&s, &untested, Some(&clean())), Health::Warning);

    let mut tested = model("model.b", 2);
    tested.build = Some(built());
    let badge = s.evaluate(&tested, Some(&clean()));
    assert_eq!(badge.health, Health::Warning);
    assert!(badge.reasons[0].contains("haven't been recorded passing"));

    tested.build = Some(built().tested("run-1", at("2026-09-28T09:05:00Z"), false));
    assert!(s.evaluate(&tested, Some(&clean())).reasons[0].contains("changed since"));
}

#[test]
fn a_seed_without_tests_is_healthy_once_built() {
    let mut seed = NodeFacts::new("seed.s", "s", "seed");
    seed.build = Some(built());
    let badge = HealthSettings::default().evaluate(&seed, Some(&clean()));
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

#[test]
fn without_the_last_runs_record_the_run_checks_cant_decide() {
    // Built and tested, but nothing says whether a later run failed it: unknown, never
    // healthy (AGENTS rule 3); or a warning, if so configured.
    let mut node = model("model.a", 1);
    node.build = Some(passed());
    let badge = HealthSettings::default().evaluate(&node, None);
    assert_eq!(badge.health, Health::Unknown);
    assert!(
        badge.reasons[0].contains("failures aren't measured"),
        "{:?}",
        badge.reasons
    );
    let warning = settings("unknown_counts_as = \"warning\"");
    assert_eq!(health(&warning, &node, None), Health::Warning);
    // With both run checks off, the rest decide.
    let off = settings(
        "[builtin.last_run_failed]\nseverity = \"off\"\n[builtin.last_run_skipped]\nseverity = \"off\"",
    );
    assert_eq!(health(&off, &node, None), Health::Healthy);
}

// ---------------------------------------------------------------- configuration

#[test]
fn a_check_turned_off_never_decides() {
    let s = settings("[builtin.tests_required]\nseverity = \"off\"");
    let mut node = model("model.a", 0);
    node.build = Some(built());
    let badge = s.evaluate(&node, Some(&clean()));
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
    assert_eq!(health(&error, &node, Some(&clean())), Health::Failing);
    let info = settings("[builtin.tests_required]\nseverity = \"info\"");
    let badge = info.evaluate(&node, Some(&clean()));
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
        health(&s, &mart, Some(&clean())),
        Health::Warning,
        "a mart needs tests"
    );

    let mut staging = mart.clone();
    staging.path = Some("models/staging/stg_orders.sql".into());
    assert_eq!(
        health(&s, &staging, Some(&clean())),
        Health::Healthy,
        "not selected"
    );

    let mut experimental = mart.clone();
    experimental.tags = vec!["experimental".into()];
    assert_eq!(
        health(&s, &experimental, Some(&clean())),
        Health::Healthy,
        "excluded"
    );

    // A node without a known path never matches a path selector.
    let mut unknown_path = mart.clone();
    unknown_path.path = None;
    assert_eq!(health(&s, &unknown_path, Some(&clean())), Health::Healthy);

    let by_name = settings("[builtin.tests_required]\nselect = { name = [\"orders\"] }");
    assert_eq!(health(&by_name, &mart, Some(&clean())), Health::Warning);
    // Selecting replaces the default scope: seeds can be required to have tests too.
    let seeds = settings("[builtin.tests_required]\nselect = { resource_type = [\"seed\"] }");
    let mut seed = NodeFacts::new("seed.s", "s", "seed");
    seed.build = Some(built());
    assert_eq!(health(&seeds, &seed, Some(&clean())), Health::Warning);
}

#[test]
fn unknown_can_count_as_a_warning_never_as_healthy() {
    let s = settings("unknown_counts_as = \"warning\"");
    let badge = s.evaluate(&model("model.a", 0), Some(&clean()));
    assert_eq!(badge.health, Health::Warning);
    assert!(badge.reasons.iter().any(|r| r.starts_with("never built")));
    assert_eq!(s.unknown_counts_as(), Health::Warning);
}

#[test]
fn with_built_off_a_never_built_node_is_judged_on_the_rest() {
    let s = settings("[builtin.built]\nseverity = \"off\"");
    let node = model("model.a", 1);
    // Never built: its tests can't have passed on a build, but it has tests.
    assert_eq!(health(&s, &node, Some(&clean())), Health::Healthy);
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
    let badge = s.evaluate(&node, Some(&clean()));
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
    assert!(how.contains("last-run checks can't decide"), "{how}");
    assert!(!s.how(true).contains("last-run checks can't decide"));
}

#[test]
fn counts_list_every_health() {
    let s = HealthSettings::default();
    let badges = [s.evaluate(&model("model.a", 0), Some(&clean()))];
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
        prop_assert_ne!(health(&s, &node, Some(&clean())), Health::Healthy);
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
        let checks: Vec<&str> = badge.findings.iter().map(|f| f.check.as_str()).collect();
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

// ---------------------------------------------------------------- registered checks

mod registered {
    use std::sync::Arc;
    use std::time::Duration;

    use ods_provider_fake::{FakeHealthCheck, Misbehaviour};

    use super::*;

    fn scope() -> CheckScope {
        let mut a = model("model.p.a", 1);
        a.build = Some(passed());
        let mut b = model("model.p.b", 1);
        b.build = Some(passed());
        CheckScope::new(vec![a, b], Some(clean()))
    }

    fn with(check: FakeHealthCheck) -> HealthSettings {
        HealthSettings::default()
            .with_check(Arc::new(check))
            .unwrap()
    }

    async fn run(settings: &HealthSettings) -> HealthReport {
        settings.run(&scope(), CHECK_TIMEOUT).await
    }

    fn plugin_finding<'a>(report: &'a HealthReport, node: &str) -> &'a Finding {
        report.badges[node]
            .findings
            .iter()
            .find(|f| f.source == CheckSource::Plugin)
            .unwrap()
    }

    #[tokio::test]
    async fn a_passing_check_keeps_nodes_healthy() {
        let report = run(&with(FakeHealthCheck::new("owner", Severity::Error))).await;
        assert_eq!(report.badges["model.p.a"].health, Health::Healthy);
        assert_eq!(plugin_finding(&report, "model.p.a").status, Status::Pass);
        assert_eq!(report.checks.last().unwrap().id, "owner");
        assert_eq!(report.checks.last().unwrap().source, CheckSource::Plugin);
        assert!(!report.fails(true));
    }

    #[tokio::test]
    async fn a_failure_counts_at_the_checks_severity() {
        let error = run(&with(
            FakeHealthCheck::new("owner", Severity::Error).failing("model.p.a"),
        ))
        .await;
        assert_eq!(error.badges["model.p.a"].health, Health::Failing);
        assert_eq!(error.badges["model.p.b"].health, Health::Healthy);
        assert!(error.fails(false));

        let warn = run(&with(
            FakeHealthCheck::new("owner", Severity::Warn).failing("model.p.a"),
        ))
        .await;
        assert_eq!(warn.badges["model.p.a"].health, Health::Warning);
        assert!(!warn.fails(true));

        let info = run(&with(
            FakeHealthCheck::new("owner", Severity::Info).failing("model.p.a"),
        ))
        .await;
        assert_eq!(info.badges["model.p.a"].health, Health::Healthy);
    }

    #[tokio::test]
    async fn an_undecided_node_is_unknown_and_fails_only_when_strict() {
        let report = run(&with(
            FakeHealthCheck::new("owner", Severity::Error).unknown("model.p.a"),
        ))
        .await;
        assert_eq!(report.badges["model.p.a"].health, Health::Unknown);
        assert!(!report.fails(false));
        assert!(report.fails(true));
    }

    #[tokio::test]
    async fn a_check_that_errs_decides_nothing() {
        let report = run(&with(
            FakeHealthCheck::new("owner", Severity::Error).misbehaving(Misbehaviour::Fails),
        ))
        .await;
        for node in ["model.p.a", "model.p.b"] {
            let finding = plugin_finding(&report, node);
            assert_eq!(finding.status, Status::Unknown);
            assert!(finding.reason.starts_with("the check couldn't run"));
            assert_eq!(report.badges[node].health, Health::Unknown);
        }
    }

    #[tokio::test]
    async fn a_skipped_or_repeated_answer_is_unknown() {
        let skipped = run(&with(
            FakeHealthCheck::new("owner", Severity::Error).misbehaving(Misbehaviour::SkipsANode),
        ))
        .await;
        assert_eq!(
            plugin_finding(&skipped, "model.p.a").status,
            Status::Unknown
        );
        assert_eq!(plugin_finding(&skipped, "model.p.b").status, Status::Pass);

        let twice = run(&with(
            FakeHealthCheck::new("owner", Severity::Error).misbehaving(Misbehaviour::AnswersTwice),
        ))
        .await;
        let finding = plugin_finding(&twice, "model.p.a");
        assert_eq!(finding.status, Status::Unknown);
        assert!(finding.reason.contains("more than once"));
    }

    #[tokio::test]
    async fn an_answer_about_a_stranger_voids_the_whole_check() {
        let report = run(&with(
            FakeHealthCheck::new("owner", Severity::Error)
                .failing("model.p.b")
                .misbehaving(Misbehaviour::AnswersAStranger),
        ))
        .await;
        assert_eq!(
            report.badges.keys().collect::<Vec<_>>(),
            ["model.p.a", "model.p.b"]
        );
        for node in ["model.p.a", "model.p.b"] {
            let finding = plugin_finding(&report, node);
            assert_eq!(finding.status, Status::Unknown, "{node}");
            assert!(
                finding.reason.contains("model.stranger"),
                "{}",
                finding.reason
            );
        }
        assert!(!report.fails(false));
        assert!(report.fails(true));
    }

    #[tokio::test]
    async fn a_checks_evidence_is_kept() {
        let report = run(&with(
            FakeHealthCheck::new("owner", Severity::Warn).failing("model.p.a"),
        ))
        .await;
        let finding = plugin_finding(&report, "model.p.a");
        assert_eq!(finding.evidence["told_to"], "fail");
        assert!(plugin_finding(&report, "model.p.b").evidence.is_empty());
        let built = report.badges["model.p.a"]
            .findings
            .iter()
            .find(|f| f.check == "built")
            .unwrap();
        assert_eq!(built.evidence["run_id"], "run-1");
        assert_eq!(built.evidence["built_at"], "2026-09-28T09:00:00Z");
    }

    #[tokio::test(start_paused = true)]
    async fn a_check_that_hangs_times_out_as_unknown() {
        let settings =
            with(FakeHealthCheck::new("owner", Severity::Error).misbehaving(Misbehaviour::Hangs));
        let report = settings.run(&scope(), Duration::from_secs(5)).await;
        let finding = plugin_finding(&report, "model.p.a");
        assert_eq!(finding.status, Status::Unknown);
        assert!(finding.reason.contains("within 5s"));
    }

    #[test]
    fn ids_must_be_valid_and_unique() {
        let bad = HealthSettings::default()
            .with_check(Arc::new(FakeHealthCheck::new("Owner!", Severity::Warn)));
        assert!(bad.unwrap_err().to_string().contains("isn't valid"));

        let builtin = HealthSettings::default()
            .with_check(Arc::new(FakeHealthCheck::new("built", Severity::Warn)));
        assert!(
            builtin
                .unwrap_err()
                .to_string()
                .contains("two health checks")
        );

        let twice = with(FakeHealthCheck::new("owner", Severity::Warn))
            .with_check(Arc::new(FakeHealthCheck::new("owner", Severity::Warn)));
        assert!(twice.unwrap_err().to_string().contains("two health checks"));
    }

    /// What `ods serve` does with a record: the built-ins live, and the registered
    /// checks' findings from the record (ADR-0030 §6).
    fn recorded(report: HealthReport) -> crate::record::Recorded {
        let record = crate::record::HealthRecord::new("p/dev", at("2026-10-06T09:00:00Z"), report);
        crate::record::Recorded::of(&record)
    }

    #[tokio::test]
    async fn a_recorded_failure_reaches_the_badge_beside_the_live_builtins() {
        let report = run(&with(
            FakeHealthCheck::new("owner", Severity::Error).failing("model.p.a"),
        ))
        .await;
        let recorded = recorded(report);
        assert_eq!(
            recorded.checks.len(),
            1,
            "only the registered check is kept"
        );
        let settings = HealthSettings::default();
        let node = &scope().nodes[0];
        let badge = settings.evaluate_with(node, Some(&clean()), Some(&recorded));
        assert_eq!(badge.health, Health::Failing);
        let owner: Vec<&Finding> = badge
            .findings
            .iter()
            .filter(|f| f.check == "owner")
            .collect();
        assert_eq!(owner.len(), 1);
        assert_eq!(owner[0].source, CheckSource::Plugin);
        // The built-ins are the live ones, once each, not the record's as well.
        let built = badge.findings.iter().filter(|f| f.check == "built").count();
        assert_eq!(built, 1);
        // Without the record the same node is healthy: the record is what fails it.
        assert_eq!(
            settings.evaluate(node, Some(&clean())).health,
            Health::Healthy
        );
    }

    #[tokio::test]
    async fn a_node_newer_than_the_record_is_unknown_for_its_checks_never_passed() {
        let recorded = recorded(run(&with(FakeHealthCheck::new("owner", Severity::Warn))).await);
        let mut newer = model("model.p.new", 1);
        newer.build = Some(passed());
        let badge =
            HealthSettings::default().evaluate_with(&newer, Some(&clean()), Some(&recorded));
        assert_eq!(badge.health, Health::Unknown);
        let owner = badge.findings.iter().find(|f| f.check == "owner").unwrap();
        assert_eq!(owner.status, Status::Unknown);
        assert!(
            owner.reason.contains("2026-10-06T09:00:00Z"),
            "{}",
            owner.reason
        );
    }

    #[tokio::test]
    async fn a_record_of_only_builtins_adds_nothing() {
        let report = HealthSettings::default().run(&scope(), CHECK_TIMEOUT).await;
        let recorded = recorded(report);
        assert!(recorded.is_empty());
        let node = &scope().nodes[0];
        let settings = HealthSettings::default();
        assert_eq!(
            settings.evaluate_with(node, Some(&clean()), Some(&recorded)),
            settings.evaluate(node, Some(&clean()))
        );
    }

    #[tokio::test]
    async fn builtins_still_run_beside_registered_checks() {
        let mut scope = scope();
        scope.nodes[0].build = None;
        let report = with(FakeHealthCheck::new("owner", Severity::Warn))
            .run(&scope, CHECK_TIMEOUT)
            .await;
        assert_eq!(report.badges["model.p.a"].health, Health::Unknown);
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.id == "built" && c.source == CheckSource::Builtin)
        );
    }
}

/// Times are kept to the second: a build in the same second as the failed run's record
/// isn't taken as a later one, so the failure stands.
#[test]
fn a_build_in_the_same_second_as_the_failure_does_not_clear_it() {
    let mut node = model("model.p.a", 1);
    node.build = Some(BuildFacts::new("run-0", at("2026-09-29T09:00:00Z")));
    let badge = HealthSettings::default().evaluate(&node, Some(&failures(&["model.p.a"], &[])));
    assert_eq!(badge.health, Health::Failing);

    node.build = Some(BuildFacts::new("run-2", at("2026-09-29T09:00:01Z")));
    let badge = HealthSettings::default().evaluate(&node, Some(&failures(&["model.p.a"], &[])));
    assert_ne!(badge.health, Health::Failing);
}

// ---------------------------------------------------------------- declared checks

/// A mart, built and tested, with `facts` applied.
fn mart(id: &str, facts: impl FnOnce(&mut NodeFacts)) -> NodeFacts {
    let mut node = model(id, 1);
    node.path = Some(format!("models/marts/{}.sql", node.name));
    node.build = Some(passed());
    facts(&mut node);
    node
}

const MARTS_DOCUMENTED: &str = r#"
[[checks]]
id = "marts.documented"
select = { path = ["models/marts/**"] }
require = ["description", "test:unique"]
"#;

fn declared_finding(badge: &HealthBadge, id: &str) -> Finding {
    badge
        .findings
        .iter()
        .find(|f| f.check == id)
        .unwrap_or_else(|| panic!("no {id}: {badge:?}"))
        .clone()
}

#[test]
fn a_declared_check_passes_a_node_with_everything_it_requires() {
    let s = settings(MARTS_DOCUMENTED);
    let node = mart("model.p.orders", |n| {
        n.described = true;
        n.test_types = ["unique".to_owned(), "not_null".to_owned()].into();
    });
    let badge = s.evaluate(&node, Some(&clean()));
    assert_eq!(badge.health, Health::Healthy, "{badge:?}");
    let finding = declared_finding(&badge, "marts.documented");
    assert_eq!(finding.status, Status::Pass);
    assert_eq!(finding.source, CheckSource::Declarative);
    assert_eq!(finding.severity, Severity::Warn, "warn unless configured");
    assert_eq!(finding.evidence["require"], "description, test:unique");
}

#[test]
fn a_declared_check_names_what_is_missing() {
    let s = settings(MARTS_DOCUMENTED);
    let node = mart("model.p.orders", |n| {
        n.test_types = ["not_null".to_owned()].into();
    });
    let badge = s.evaluate(&node, Some(&clean()));
    assert_eq!(badge.health, Health::Warning);
    let finding = declared_finding(&badge, "marts.documented");
    assert_eq!(finding.status, Status::Fail);
    assert_eq!(
        finding.reason,
        "marts.documented: no description; no `unique` test"
    );
    assert_eq!(finding.evidence["missing"], "description, test:unique");
    assert!(badge.reasons.contains(&finding.reason), "{badge:?}");
}

#[test]
fn a_declared_check_at_error_fails_the_node_and_counts_without_the_runs_record() {
    let s = settings(
        r#"
[[checks]]
id = "owned"
require = ["tag:owned", "tests", "constraints"]
severity = "error"
"#,
    );
    assert!(s.errs_without_run_failures());
    let node = mart("model.p.orders", |n| n.tags = vec!["owned".into()]);
    let badge = s.evaluate(&node, Some(&clean()));
    assert_eq!(badge.health, Health::Failing, "{badge:?}");
    assert_eq!(
        declared_finding(&badge, "owned").reason,
        "owned: no constraints"
    );
    assert!(!settings("").errs_without_run_failures(), "the defaults");
}

#[test]
fn a_declared_check_skips_what_it_doesnt_select_or_excludes() {
    let s = settings(
        r#"
[[checks]]
id = "marts.documented"
select = { path = ["models/marts/**"] }
exclude = { tags = ["experimental"] }
require = ["description"]
"#,
    );
    let mut staging = model("model.p.stg_orders", 1);
    staging.path = Some("models/staging/stg_orders.sql".into());
    staging.build = Some(passed());
    let finding = declared_finding(&s.evaluate(&staging, Some(&clean())), "marts.documented");
    assert_eq!(finding.status, Status::Skipped);
    assert!(finding.evidence.is_empty());

    let excluded = mart("model.p.orders", |n| n.tags = vec!["experimental".into()]);
    let badge = s.evaluate(&excluded, Some(&clean()));
    assert_eq!(
        declared_finding(&badge, "marts.documented").status,
        Status::Skipped
    );
    assert_eq!(badge.health, Health::Healthy, "skipped never fails");
}

#[test]
fn a_declared_check_turned_off_says_nothing() {
    let s = settings(
        r#"
[[checks]]
id = "marts.documented"
require = ["description"]
severity = "off"
"#,
    );
    let badge = s.evaluate(&mart("model.p.orders", |_| {}), Some(&clean()));
    assert!(badge.findings.iter().all(|f| f.check != "marts.documented"));
    assert!(s.how(true).contains("marts.documented (off, declared)"));
}

#[test]
fn a_misdeclared_check_is_a_configuration_error() {
    let error = |toml: &str| {
        let config: HealthConfig = toml::from_str(toml).unwrap();
        HealthSettings::from_config(&config)
            .unwrap_err()
            .to_string()
    };
    let one = |fields: &str| ["[[checks]]\n", fields, "\n"].concat();
    let cases = [
        (
            one("id = \"Bad Id\"\nrequire = [\"tests\"]"),
            "health.checks[0].id",
        ),
        (
            one("id = \"built\"\nrequire = [\"tests\"]"),
            "[health.builtin.built]",
        ),
        (
            [
                one("id = \"a\"\nrequire = [\"tests\"]"),
                one("id = \"a\"\nrequire = [\"tests\"]"),
            ]
            .concat(),
            "two checks in [[health.checks]] are called `a`",
        ),
        (
            one("id = \"a\"\nkind = \"script\"\nrequire = [\"tests\"]"),
            "aren't supported yet",
        ),
        (
            one("id = \"a\"\nkind = \"magic\"\nrequire = [\"tests\"]"),
            "isn't a kind of check",
        ),
        (one("id = \"a\""), "health.checks[0].require: say what"),
        (
            one("id = \"a\"\nrequire = [\"owner\"]"),
            "`owner` isn't something",
        ),
        (
            one("id = \"a\"\nrequire = [\"test:\"]"),
            "`test:` isn't something",
        ),
        (
            one("id = \"a\"\nrequire = [\"tests\"]\nselect = { path = [\"models/[\"] }"),
            "health.checks[0].select.path",
        ),
    ];
    for (toml, expected) in cases {
        let error = error(&toml);
        assert!(error.contains(expected), "{toml}\n=> {error}");
    }
    // Unknown keys are refused by the configuration itself (ADR-0005).
    assert!(toml::from_str::<HealthConfig>("[[checks]]\nid = \"a\"\nrequires = []").is_err());
}

#[tokio::test]
async fn declared_checks_run_live_and_are_never_read_back_from_a_record() {
    let s = settings(MARTS_DOCUMENTED);
    let node = mart("model.p.orders", |_| {});
    let report = s
        .run(
            &CheckScope::new(vec![node.clone()], Some(clean())),
            CHECK_TIMEOUT,
        )
        .await;
    let run = report
        .checks
        .iter()
        .find(|c| c.id == "marts.documented")
        .unwrap();
    assert_eq!(run.source, CheckSource::Declarative);
    assert_eq!(run.about, "requires description, test:unique");
    assert_eq!(
        declared_finding(&report.badges["model.p.orders"], "marts.documented").status,
        Status::Fail
    );
    // The dashboard works them out itself: a record of them adds nothing to read back.
    let record = crate::record::HealthRecord::new("p/dev", at("2026-10-06T09:00:00Z"), report);
    assert!(crate::record::Recorded::of(&record).is_empty());
}

#[test]
fn a_plugin_cant_take_a_declared_checks_id() {
    let clash = settings(MARTS_DOCUMENTED).with_check(std::sync::Arc::new(
        ods_provider_fake::FakeHealthCheck::new("marts.documented", Severity::Warn),
    ));
    assert!(clash.is_err());
}

// ---------------------------------------------------------------- coverage targets

fn measured(key: &str, covered: Option<usize>, total: usize) -> Measured {
    Measured::new(key, covered, total)
}

#[test]
fn a_coverage_target_passes_when_reached_and_fails_below() {
    let s = settings("[coverage.tests]\ntarget = 0.8\n");
    let at_target = s.coverage(&[measured("tests", Some(8), 10)]);
    assert_eq!(at_target.len(), 1);
    assert_eq!(at_target[0].status, Status::Pass, "80% meets 80%");
    assert_eq!(
        at_target[0].severity,
        Severity::Warn,
        "warn unless configured"
    );
    assert_eq!(
        at_target[0].reason,
        "8 of 10 models with tests (80%), meeting the 80% target"
    );

    let below = s.coverage(&[measured("tests", Some(7), 10)]);
    assert_eq!(below[0].status, Status::Fail);
    assert_eq!(
        below[0].reason,
        "7 of 10 models with tests (70%), below the 80% target"
    );
    assert_eq!((below[0].covered, below[0].total), (Some(7), 10));
    assert!(
        s.how(true).contains("tests ≥ 80% (warn)"),
        "{}",
        s.how(true)
    );
}

#[test]
fn coverage_with_nothing_to_measure_is_unknown_never_reached() {
    let s = settings("[coverage.source_freshness]\ntarget = 0.5\nseverity = \"error\"\n");
    for measured in [
        vec![measured("source_freshness", None, 0)],
        vec![measured("source_freshness", Some(0), 0)],
        vec![],
    ] {
        let found = s.coverage(&measured);
        assert_eq!(found[0].status, Status::Unknown, "{measured:?}");
        assert!(
            found[0].reason.contains("nothing to measure"),
            "{}",
            found[0].reason
        );
    }
}

#[test]
fn a_missed_target_at_error_fails_the_gate_and_unknown_only_when_strict() {
    let s = settings("[coverage.descriptions]\ntarget = 1.0\nseverity = \"error\"\n");
    let report = |covered: Option<usize>| {
        HealthReport {
            badges: BTreeMap::new(),
            checks: Vec::new(),
            coverage: Vec::new(),
            elevated_login: None,
        }
        .with_coverage(s.coverage(&[measured("descriptions", covered, 4)]))
    };
    assert!(report(Some(3)).fails(false));
    assert!(!report(Some(4)).fails(true));
    assert!(!report(None).fails(false));
    assert!(report(None).fails(true));
    // At warn, a miss never fails the gate.
    let warn = settings("[coverage.descriptions]\ntarget = 1.0\n");
    let report = HealthReport {
        badges: BTreeMap::new(),
        checks: Vec::new(),
        coverage: Vec::new(),
        elevated_login: None,
    }
    .with_coverage(warn.coverage(&[measured("descriptions", Some(0), 4)]));
    assert!(!report.fails(true));
}

#[test]
fn a_target_turned_off_says_nothing() {
    let s = settings("[coverage.tests]\ntarget = 0.8\nseverity = \"off\"\n");
    assert_eq!(s.coverage(&[measured("tests", Some(0), 10)]), Vec::new());
}

#[test]
fn a_misconfigured_target_is_a_configuration_error() {
    let error = |toml: &str| {
        let config: HealthConfig = toml::from_str(toml).unwrap();
        HealthSettings::from_config(&config)
            .unwrap_err()
            .to_string()
    };
    let unknown = error("[coverage.docs]\ntarget = 0.8\n");
    assert!(unknown.contains("health.coverage.docs"), "{unknown}");
    assert!(
        unknown.contains("descriptions"),
        "lists the measures: {unknown}"
    );
    for bad in ["1.5", "-0.1", "nan", "0.8751"] {
        let error = error(&format!("[coverage.tests]\ntarget = {bad}\n"));
        assert!(
            error.contains("health.coverage.tests.target"),
            "{bad}: {error}"
        );
    }
    assert!(toml::from_str::<HealthConfig>("[coverage.tests]\ntarget = 0.8\nlevel = 1\n").is_err());
}

#[test]
fn a_share_keeps_thousandths_and_round_trips() {
    let s = settings("[coverage.tests]\ntarget = 0.875\n");
    let found = s.coverage(&[measured("tests", Some(7), 8)]);
    assert_eq!(found[0].status, Status::Pass, "7/8 is exactly 87.5%");
    assert_eq!(
        found[0].reason, "7 of 8 models with tests (87.5%), meeting the 87.5% target",
        "the share is said as precisely as the target"
    );
    let below =
        settings("[coverage.tests]\ntarget = 0.9\n").coverage(&[measured("tests", Some(2), 3)]);
    assert_eq!(
        below[0].reason, "2 of 3 models with tests (66.6%), below the 90% target",
        "to the thousandth below, never rounded up past a target"
    );
    let json = serde_json::to_value(&found[0]).unwrap();
    assert_eq!(json["target"], 0.875);
    let back: CoverageFinding = serde_json::from_value(json).unwrap();
    assert_eq!(back, found[0]);
}

// ---------------------------------------------------------------- probe checks

const ORDERS_HAS_ROWS: &str = r#"
[[checks]]
id = "orders.has_rows"
kind = "probe"
select = { name = ["orders"] }
sql = "select count(*) as n from {relation}"
pass = "n > 0"
severity = "error"
"#;

fn orders() -> NodeFacts {
    let mut node = model("model.p.orders", 1);
    node.build = Some(passed());
    node
}

fn probe_finding(s: &HealthSettings, node: &NodeFacts) -> Finding {
    let scope = CheckScope::new(vec![node.clone()], Some(clean()));
    let report = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(s.run(&scope, CHECK_TIMEOUT));
    report.badges[&node.id]
        .findings
        .iter()
        .find(|f| f.source == CheckSource::Probe)
        .unwrap_or_else(|| panic!("no probe finding: {report:?}"))
        .clone()
}

#[test]
fn a_probe_runs_only_once_every_guard_holds_and_never_passes_before() {
    let mut s = settings(ORDERS_HAS_ROWS);
    let unchecked = probe_finding(&s, &orders());
    assert_eq!(unchecked.status, Status::Unknown);
    assert!(
        unchecked
            .reason
            .contains("wasn't checked as one read-only query"),
        "{}",
        unchecked.reason
    );

    s.check_probe_sql(&|_| Ok(())).unwrap();
    let untrusted = probe_finding(&s, &orders());
    assert_eq!(untrusted.status, Status::Unknown);
    assert!(
        untrusted.reason.contains("not trusted for this project"),
        "{}",
        untrusted.reason
    );
    assert!(untrusted.reason.contains("ods health trust"));
    assert_eq!(untrusted.evidence["trusted"], "false");

    s.trust_probes(&BTreeSet::from(["orders.has_rows".to_owned()]));
    let trusted = probe_finding(&s, &orders());
    assert_eq!(trusted.status, Status::Unknown, "nothing to run it through");
    assert!(
        trusted.reason.contains("no warehouse connection"),
        "{}",
        trusted.reason
    );
    assert_eq!(trusted.severity, Severity::Error);

    // A node it doesn't select is skipped; the dashboard never runs a probe.
    let other = model("model.p.customers", 1);
    assert_eq!(probe_finding(&s, &other).status, Status::Skipped);
    assert!(
        s.evaluate(&orders(), Some(&clean()))
            .findings
            .iter()
            .all(|f| f.source != CheckSource::Probe)
    );
    assert!(
        s.how(true).contains("orders.has_rows (error, probe)"),
        "{}",
        s.how(true)
    );
}

#[test]
fn a_probe_that_isnt_read_only_is_a_configuration_error_naming_why() {
    let mut s = settings(ORDERS_HAS_ROWS);
    let seen = std::cell::RefCell::new(String::new());
    let error = s
        .check_probe_sql(&|sql| {
            seen.replace(sql.to_owned());
            Err("it contains a DELETE statement".to_owned())
        })
        .unwrap_err()
        .to_string();
    assert_eq!(
        error,
        "health.checks[0].sql: probe `orders.has_rows` must be one read-only query, but it contains a DELETE statement"
    );
    assert_eq!(
        *seen.borrow(),
        "select count(*) as n from ods_probe_relation",
        "{{relation}} reads as a plain name when checked"
    );
}

#[test]
fn a_misconfigured_probe_is_a_configuration_error() {
    let error = |fields: &str| {
        let config: HealthConfig =
            toml::from_str(&["[[checks]]\nid = \"p\"\nkind = \"probe\"\n", fields].concat())
                .unwrap();
        HealthSettings::from_config(&config)
            .unwrap_err()
            .to_string()
    };
    let select = "select = { name = [\"orders\"] }\n";
    let sql = "sql = \"select count(*) as n from {relation}\"\n";
    let pass = "pass = \"n > 0\"\n";
    for (fields, expected) in [
        (
            [sql, pass].concat(),
            "health.checks[0].select: a probe names the nodes",
        ),
        (
            [select, pass].concat(),
            "health.checks[0].sql: a probe needs one read-only query",
        ),
        (
            [select, sql].concat(),
            "health.checks[0].pass: a probe needs a condition",
        ),
        (
            [select, sql, "pass = \"n >\"\n"].concat(),
            "health.checks[0].pass: `n >`",
        ),
        (
            [select, sql, pass, "require = [\"tests\"]\n"].concat(),
            "not `require`",
        ),
        (
            [select, "sql = \"select count(*) as n from orders\"\n", pass].concat(),
            "health.checks[0].sql",
        ),
    ] {
        let error = error(&fields);
        assert!(error.contains(expected), "{fields}\n=> {error}");
    }
    let declarative = "[[checks]]\nid = \"d\"\nrequire = [\"tests\"]\nsql = \"select 1\"\n";
    let config: HealthConfig = toml::from_str(declarative).unwrap();
    let error = HealthSettings::from_config(&config)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("only a `kind = \"probe\"` check has `sql`"),
        "{error}"
    );
}

#[test]
fn a_probes_digest_pins_what_runs_and_where_not_its_verdict() {
    let digest = |toml: &str| settings(toml).probe_definitions()[0].digest.clone();
    let base = digest(ORDERS_HAS_ROWS);
    assert!(base.starts_with("sha256:"), "{base}");
    assert_eq!(base, digest(ORDERS_HAS_ROWS), "stable");
    assert_ne!(
        base,
        digest(&ORDERS_HAS_ROWS.replace("count(*)", "count(1)")),
        "the SQL"
    );
    assert_ne!(
        base,
        digest(&ORDERS_HAS_ROWS.replace("\"orders\"]", "\"orders\", \"payments\"]")),
        "what it reads"
    );
    assert_eq!(
        base,
        digest(&ORDERS_HAS_ROWS.replace("n > 0", "n > 10")),
        "not the condition"
    );
    assert_eq!(
        base,
        digest(&ORDERS_HAS_ROWS.replace("\"error\"", "\"warn\"")),
        "not the severity"
    );
}

#[tokio::test]
async fn probe_findings_are_recorded_for_the_dashboard_to_read() {
    let s = settings(ORDERS_HAS_ROWS);
    let report = s
        .run(
            &CheckScope::new(vec![orders()], Some(clean())),
            CHECK_TIMEOUT,
        )
        .await;
    let run = report
        .checks
        .iter()
        .find(|c| c.id == "orders.has_rows")
        .unwrap();
    assert_eq!(run.source, CheckSource::Probe);
    let record = crate::record::HealthRecord::new("p/dev", at("2026-10-06T09:00:00Z"), report);
    let recorded = crate::record::Recorded::of(&record);
    assert_eq!(
        recorded.checks.len(),
        1,
        "probes aren't live: the dashboard reads them back"
    );
}

// ------------------------------------------------- running probes (ADR-0030 §4c)

mod running {
    use std::sync::Arc;

    use ods_provider_fake::{FakeRelationPrivileges, FakeRelationProbe};

    use super::*;

    const SQL: &str = "select count(*) as n from {relation}";

    /// One warehouse connection: it probes, and says what its login may do.
    struct Both {
        probe: FakeRelationProbe,
        privileges: FakeRelationPrivileges,
    }

    impl ods_sdk::Provider for Both {
        fn info(&self) -> ods_sdk::ProviderInfo {
            self.probe.info()
        }
    }

    #[async_trait::async_trait]
    impl ods_sdk::contracts::probe::RelationProbe for Both {
        async fn probe(
            &self,
            request: &ods_sdk::contracts::probe::ProbeRequest,
            targets: &[ods_sdk::contracts::probe::ProbeTarget],
        ) -> Result<ods_sdk::contracts::probe::ProbeReport, ods_sdk::ProviderError> {
            self.probe.probe(request, targets).await
        }
    }

    #[async_trait::async_trait]
    impl ods_sdk::contracts::privileges::RelationPrivileges for Both {
        async fn privileges(
            &self,
            targets: &[ods_sdk::contracts::probe::ProbeTarget],
        ) -> Result<ods_sdk::contracts::privileges::PrivilegeReport, ods_sdk::ProviderError>
        {
            self.privileges.privileges(targets).await
        }
    }

    fn connect(
        probe: FakeRelationProbe,
        privileges: Arc<FakeRelationPrivileges>,
    ) -> ProbeConnection {
        ProbeConnection::new(Arc::new(Both {
            probe,
            privileges: Arc::unwrap_or_clone(privileges),
        }))
    }

    /// `ORDERS_HAS_ROWS`, checked and trusted, selecting `orders` and `payments`.
    fn ready() -> HealthSettings {
        let mut s = settings(&ORDERS_HAS_ROWS.replace(
            r#"select = { name = ["orders"] }"#,
            r#"select = { name = ["orders", "payments"] }"#,
        ));
        s.check_probe_sql(&|_| Ok(())).unwrap();
        s.trust_probes(&BTreeSet::from(["orders.has_rows".to_owned()]));
        s
    }

    /// A warehouse where `orders` has `n` rows and `payments` none.
    fn warehouse(n: &str) -> FakeRelationProbe {
        FakeRelationProbe::new()
            .with_relation("model.p.orders", "table", None)
            .with_row("model.p.orders", SQL, [("n", n)])
            .with_relation("model.p.payments", "view", None)
            .with_row("model.p.payments", SQL, [("n", "0")])
    }

    fn nodes() -> Vec<NodeFacts> {
        vec![
            orders(),
            model("model.p.payments", 1),
            model("model.p.customers", 1),
        ]
    }

    async fn findings(s: &HealthSettings) -> (BTreeMap<String, Finding>, HealthReport) {
        let report = s
            .run(&CheckScope::new(nodes(), Some(clean())), CHECK_TIMEOUT)
            .await;
        let found = report
            .badges
            .iter()
            .filter_map(|(id, badge)| {
                badge
                    .findings
                    .iter()
                    .find(|f| f.check == "orders.has_rows")
                    .map(|f| (id.clone(), f.clone()))
            })
            .collect();
        (found, report)
    }

    #[tokio::test]
    async fn under_a_read_only_login_a_probe_runs_and_judges_each_row() {
        let probe = warehouse("12");
        let s = ready().with_probe_connection(connect(
            probe.clone(),
            Arc::new(
                FakeRelationPrivileges::new()
                    .with_login("health_reader")
                    .read_only("model.p.orders")
                    .read_only("model.p.payments"),
            ),
        ));
        let (found, report) = findings(&s).await;
        let orders = &found["model.p.orders"];
        assert_eq!(orders.status, Status::Pass, "{orders:?}");
        assert_eq!(orders.evidence["login_check"], "read_only");
        assert_eq!(orders.evidence["login"], "health_reader");
        assert_eq!(orders.evidence["row.n"], "12");
        let payments = &found["model.p.payments"];
        assert_eq!(payments.status, Status::Fail, "{payments:?}");
        assert!(
            payments.reason.contains("`n` is 0, not > 0"),
            "{payments:?}"
        );
        assert_eq!(found["model.p.customers"].status, Status::Skipped);
        assert_eq!(
            probe.probed(),
            ["model.p.orders", "model.p.payments"],
            "only the selected nodes' relations"
        );
        assert_eq!(report.elevated_login, None);
        assert_eq!(report.badges["model.p.payments"].health, Health::Failing);
    }

    #[tokio::test]
    async fn a_login_that_can_do_more_than_read_is_refused_and_nothing_runs() {
        let probe = warehouse("12");
        let s = ready().with_probe_connection(connect(
            probe.clone(),
            Arc::new(
                FakeRelationPrivileges::new()
                    .with_login("builder")
                    .elevated(
                        "model.p.orders",
                        ["MODIFY on schema p", "owner of table orders"],
                    )
                    .unknown("model.p.payments", "no grants visible"),
            ),
        ));
        let (found, report) = findings(&s).await;
        let orders = &found["model.p.orders"];
        assert_eq!(orders.status, Status::Unknown, "never a pass: {orders:?}");
        for part in [
            "refused",
            "read-only login",
            "the login `builder` can do more than read: MODIFY on schema p, owner of table orders",
            "--allow-elevated-login",
        ] {
            assert!(orders.reason.contains(part), "{part}: {}", orders.reason);
        }
        assert_eq!(orders.evidence["login_check"], "refused");
        let payments = &found["model.p.payments"];
        assert_eq!(payments.status, Status::Unknown);
        assert!(
            payments
                .reason
                .contains("the login `builder` couldn't be shown to only read: no grants visible"),
            "{}",
            payments.reason
        );
        assert!(probe.probed().is_empty(), "no query was sent");
        assert_eq!(report.elevated_login, None);
    }

    #[tokio::test]
    async fn without_a_privileges_report_or_with_a_failing_one_nothing_runs() {
        for connection in [
            ProbeConnection::without_privileges(Arc::new(warehouse("12"))),
            connect(
                warehouse("12"),
                Arc::new(FakeRelationPrivileges::new().failing()),
            ),
        ] {
            let s = ready().with_probe_connection(connection);
            let (found, _) = findings(&s).await;
            let orders = &found["model.p.orders"];
            assert_eq!(orders.status, Status::Unknown, "{orders:?}");
            assert!(orders.reason.contains("refused"), "{}", orders.reason);
        }
    }

    #[tokio::test]
    async fn allowing_an_elevated_login_runs_and_says_so_everywhere() {
        let probe = warehouse("12");
        let s = ready().with_probe_connection(
            connect(
                probe.clone(),
                Arc::new(
                    FakeRelationPrivileges::new()
                        .with_login("builder")
                        .elevated("model.p.orders", ["MODIFY on schema p"])
                        .read_only("model.p.payments"),
                ),
            )
            .allowing_elevated_login(true),
        );
        let (found, report) = findings(&s).await;
        let orders = &found["model.p.orders"];
        assert_eq!(orders.status, Status::Pass, "{orders:?}");
        assert_eq!(orders.evidence["login_check"], "overridden");
        assert_eq!(
            orders.evidence["login_found"],
            "can do more than read: MODIFY on schema p"
        );
        assert_eq!(
            found["model.p.payments"].evidence["login_check"],
            "read_only"
        );
        let elevated = report.elevated_login.clone().expect("the override is kept");
        assert_eq!(elevated.login.as_deref(), Some("builder"));
        assert_eq!(
            elevated.found,
            BTreeMap::from([(
                "model.p.orders".to_owned(),
                "can do more than read: MODIFY on schema p".to_owned()
            )])
        );
        // The record keeps it, so it is never hidden afterwards.
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["elevated_login"]["login"], "builder");
        assert_eq!(
            serde_json::from_value::<HealthReport>(json).unwrap(),
            report
        );
        assert_eq!(probe.probed(), ["model.p.orders", "model.p.payments"]);
    }

    #[tokio::test]
    async fn the_override_never_skips_the_sql_or_trust_guards() {
        let mut s = settings(ORDERS_HAS_ROWS).with_probe_connection(
            ProbeConnection::without_privileges(Arc::new(warehouse("12")))
                .allowing_elevated_login(true),
        );
        s.check_probe_sql(&|_| Ok(())).unwrap();
        let (found, _) = findings(&s).await;
        assert!(
            found["model.p.orders"].reason.contains("not trusted"),
            "{:?}",
            found["model.p.orders"]
        );
    }

    #[tokio::test]
    async fn what_a_probe_cant_tell_is_unknown_never_a_pass() {
        let privileges = || {
            Arc::new(
                FakeRelationPrivileges::new()
                    .read_only("model.p.orders")
                    .read_only("model.p.payments"),
            )
        };
        // Not a number where `n > 0` needs one.
        let s = ready().with_probe_connection(connect(warehouse("many"), privileges()));
        let (found, _) = findings(&s).await;
        assert_eq!(found["model.p.orders"].status, Status::Unknown);
        // The warehouse can't be reached.
        let s = ready().with_probe_connection(connect(warehouse("12").failing(), privileges()));
        let (found, _) = findings(&s).await;
        let orders = &found["model.p.orders"];
        assert_eq!(orders.status, Status::Unknown);
        assert!(orders.reason.contains("couldn't run"), "{}", orders.reason);
        // A relation of a kind a probe doesn't read is skipped; none is unknown.
        let s = ready().with_probe_connection(connect(
            FakeRelationProbe::new().with_relation("model.p.orders", "cte", None),
            privileges(),
        ));
        let (found, _) = findings(&s).await;
        assert_eq!(found["model.p.orders"].status, Status::Skipped);
        assert_eq!(found["model.p.payments"].status, Status::Unknown);
    }

    /// A connection that reads only, but answers about a relation it wasn't asked about.
    struct Stray;

    impl ods_sdk::Provider for Stray {
        fn info(&self) -> ods_sdk::ProviderInfo {
            FakeRelationProbe::new().info()
        }
    }

    #[async_trait::async_trait]
    impl ods_sdk::contracts::probe::RelationProbe for Stray {
        async fn probe(
            &self,
            _: &ods_sdk::contracts::probe::ProbeRequest,
            targets: &[ods_sdk::contracts::probe::ProbeTarget],
        ) -> Result<ods_sdk::contracts::probe::ProbeReport, ods_sdk::ProviderError> {
            let rows = || {
                ods_sdk::contracts::probe::ProbeAnswer::Rows(vec![BTreeMap::from([(
                    "n".to_owned(),
                    "5".to_owned(),
                )])])
            };
            let mut answers: Vec<_> = targets.iter().map(|t| (t.id.clone(), rows())).collect();
            answers.push(("model.p.customers".to_owned(), rows()));
            Ok(ods_sdk::contracts::probe::ProbeReport::new(answers))
        }
    }

    #[async_trait::async_trait]
    impl ods_sdk::contracts::privileges::RelationPrivileges for Stray {
        async fn privileges(
            &self,
            targets: &[ods_sdk::contracts::probe::ProbeTarget],
        ) -> Result<ods_sdk::contracts::privileges::PrivilegeReport, ods_sdk::ProviderError>
        {
            let mut fake = FakeRelationPrivileges::new();
            for target in targets {
                fake = fake.read_only(target.id.clone());
            }
            fake.privileges(targets).await
        }
    }

    #[tokio::test]
    async fn a_probe_that_answers_about_another_relation_counts_for_nothing() {
        let s = ready().with_probe_connection(ProbeConnection::new(Arc::new(Stray)));
        let (found, _) = findings(&s).await;
        let orders = &found["model.p.orders"];
        assert_eq!(orders.status, Status::Unknown, "never a pass: {orders:?}");
        assert!(
            orders.reason.contains("which it wasn't asked about"),
            "{}",
            orders.reason
        );
    }
}
