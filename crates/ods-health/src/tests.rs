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
