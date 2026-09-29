//! `ods doctor` (#181, ADR-0023): can ODS work here? Checks the configuration, the dbt
//! project and its artifacts, dbt itself, the target, the state store and what the
//! wired providers can do, offline by default; `--connect` adds the live checks.
//!
//! The checks are in `doctor_checks`; this module is the command: its arguments, its
//! result model and how that is shown.

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_core::{CheckCategory, CheckResult, CheckStatus, HealthReport, Verdict};
use serde::Serialize;

use super::doctor_checks::{Checks, Options, PROVIDERS, codes};
use super::state_settings::artifact_dir_args;
use crate::exit::{CliError, ExitStatus};
use crate::module::{Context, Module};
use crate::present::{Present, Span, Tone, TreeItem, ViewNode};

/// `ods doctor`.
pub struct Doctor;

impl Module for Doctor {
    fn command(&self) -> Command {
        let command = Command::new("doctor")
            .about("Check that ODS can work here: configuration, dbt project, tools, target, state store and provider capabilities")
            .long_about("Check that ODS can work here: configuration, dbt project and artifacts, dbt and its adapter, the target, the state store and what the providers can do. Offline by default: nothing contacts the warehouse unless --connect is given. Exits 0 when healthy or with warnings only (unless --strict), 5 when a check fails; see docs/cli.md#ods-doctor")
            .arg(
                Arg::new("project")
                    .long("project")
                    .action(ArgAction::SetTrue)
                    .help("Only check the dbt project and its artifacts"),
            )
            .arg(
                Arg::new("provider")
                    .long("provider")
                    .value_name("NAME")
                    .value_parser(PROVIDERS)
                    .help("Only this provider's checks"),
            )
            .arg(
                Arg::new("connect")
                    .long("connect")
                    .action(ArgAction::SetTrue)
                    .help("Also run the live checks through dbt: that relations can be checked, and table versions read"),
            )
            .arg(
                Arg::new("strict")
                    .long("strict")
                    .action(ArgAction::SetTrue)
                    .help("Fail (exit 5) on warnings and unknown checks too"),
            );
        artifact_dir_args(command, "manifest.json")
            .arg(
                Arg::new("dbt")
                    .long("dbt")
                    .value_name("PROGRAM")
                    .help("The dbt executable [default: the configured program, else dbt]"),
            )
            .arg(
                Arg::new("profiles-dir")
                    .long("profiles-dir")
                    .value_name("DIR")
                    .env("DBT_PROFILES_DIR")
                    .hide_env_values(true)
                    .help("dbt's --profiles-dir"),
            )
            .arg(
                Arg::new("dbt-profile")
                    .long("dbt-profile")
                    .value_name("NAME")
                    .env("DBT_PROFILE")
                    .hide_env_values(true)
                    .help("dbt's --profile"),
            )
            .arg(
                Arg::new("target")
                    .long("target")
                    .value_name("NAME")
                    .env("DBT_TARGET")
                    .hide_env_values(true)
                    .help("dbt's --target"),
            )
            .arg(
                Arg::new("state-db")
                    .long("state-db")
                    .value_name("PATH")
                    .help("SQLite state database [config: state.db; default: .ods/state.db]"),
            )
            .arg(
                Arg::new("environment")
                    .long("environment")
                    .value_name("NAME")
                    .help("Whose state is kept [config: state.environment; default: the dbt target, else `default`]"),
            )
    }

    fn diagnoses_config(&self) -> bool {
        true
    }

    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        let options = Options {
            project_only: matches.get_flag("project"),
            provider: matches.get_one::<String>("provider").cloned(),
            connect: matches.get_flag("connect"),
            strict: matches.get_flag("strict"),
        };
        let checks = Checks::new(
            matches,
            ctx.config,
            ctx.config_failure.clone(),
            options.connect,
        )
        .run(&options);
        let report = DoctorReport::new(&options, checks);
        match report.failure() {
            Some(error) => ctx.emit_failed(&report, error),
            None => ctx.emit(&report),
        }
    }
}

/// Which checks ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct Scope {
    /// Only the project checks (`--project`).
    project_only: bool,
    /// Only this provider's checks (`--provider`).
    provider: Option<String>,
    /// Whether the live checks ran (`--connect`).
    connect: bool,
}

/// `ods doctor`'s result model: the scope, then the verdict and every check.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct DoctorReport {
    scope: Scope,
    #[serde(flatten)]
    health: HealthReport,
}

impl DoctorReport {
    pub(super) fn new(options: &Options, checks: Vec<CheckResult>) -> Self {
        Self {
            scope: Scope {
                project_only: options.project_only,
                provider: options.provider.clone(),
                connect: options.connect,
            },
            health: HealthReport::new(checks, options.strict),
        }
    }

    /// The error to exit with, when the verdict is a failure (ADR-0023 §3): exit
    /// status 5, whichever check failed; each finding carries its own code.
    pub(super) fn failure(&self) -> Option<CliError> {
        if self.health.verdict != Verdict::Failed {
            return None;
        }
        let failing: Vec<&str> = self.health.failing().map(|c| c.id.as_str()).collect();
        Some(
            CliError::new(
                ExitStatus::CheckFailed,
                codes::DOCTOR_FAILED,
                format!(
                    "{} check{} failed: {}",
                    failing.len(),
                    if failing.len() == 1 { "" } else { "s" },
                    failing.join(", ")
                ),
            )
            .with_hint(if self.health.strict {
                "each check above says what to do; without --strict, warnings and unknown optional checks don't fail"
            } else {
                "each check above says what to do; docs/cli.md#ods-doctor lists every code"
            }),
        )
    }
}

fn category_title(category: CheckCategory) -> &'static str {
    match category {
        CheckCategory::Config => "Configuration",
        CheckCategory::Project => "Project",
        CheckCategory::Tools => "Tools",
        CheckCategory::Target => "Target",
        CheckCategory::StateStore => "State store",
        CheckCategory::Capabilities => "Capabilities",
        CheckCategory::Connectivity => "Connectivity",
        _ => "Other",
    }
}

fn status_span(check: &CheckResult) -> Span {
    let tone = match check.status {
        CheckStatus::Ok => Tone::Success,
        CheckStatus::Warning | CheckStatus::Unknown => Tone::Warning,
        CheckStatus::Error => Tone::Error,
        _ => Tone::Muted,
    };
    let text = match &check.code {
        Some(code) => format!("[{} {code}]", check.status.name()),
        None => format!("[{}]", check.status.name()),
    };
    Span::toned(text, tone)
}

fn check_item(check: &CheckResult) -> TreeItem {
    let mut children: Vec<TreeItem> = check
        .evidence
        .iter()
        .map(|e| {
            let mut label = vec![
                Span::toned(format!("{}: ", e.key), Tone::Muted),
                Span::plain(e.value.as_str()),
            ];
            if let Some(source) = &e.source {
                label.push(Span::toned(format!(" ({source})"), Tone::Muted));
            }
            TreeItem::leaf(label)
        })
        .collect();
    if let Some(hint) = &check.hint {
        children.push(TreeItem::leaf(vec![
            Span::toned("hint: ", Tone::Emphasis),
            Span::plain(hint.as_str()),
        ]));
    }
    let mut label = vec![
        status_span(check),
        Span::plain(" "),
        Span::toned(check.id.as_str(), Tone::Code),
    ];
    if check.required {
        label.push(Span::toned(" (required)", Tone::Muted));
    }
    label.push(Span::plain(format!(": {}", check.message)));
    TreeItem { label, children }
}

impl Present for DoctorReport {
    const COMMAND: &'static str = "doctor";

    fn view(&self) -> ViewNode {
        let health = &self.health;
        let verdict = match health.verdict {
            Verdict::Healthy => Span::toned("healthy", Tone::Success),
            Verdict::Warnings => Span::toned("healthy, with warnings", Tone::Warning),
            _ => Span::toned("failed", Tone::Error),
        };
        let s = health.summary;
        let mut summary = vec![
            ("verdict".to_owned(), vec![verdict]),
            (
                "checks".to_owned(),
                vec![Span::plain(format!(
                    "{} ok, {} warning, {} error, {} unknown, {} skipped",
                    s.ok, s.warning, s.error, s.unknown, s.skipped
                ))],
            ),
        ];
        let mut scope = Vec::new();
        if self.scope.project_only {
            scope.push("--project".to_owned());
        }
        if let Some(provider) = &self.scope.provider {
            scope.push(format!("--provider {provider}"));
        }
        if self.scope.connect {
            scope.push("--connect".to_owned());
        }
        if health.strict {
            scope.push("--strict".to_owned());
        }
        if !scope.is_empty() {
            summary.push(("options".to_owned(), vec![Span::plain(scope.join(" "))]));
        }
        let mut blocks = vec![
            ViewNode::Heading("ODS doctor".into()),
            ViewNode::KeyValue(summary),
        ];
        let mut categories: Vec<CheckCategory> = health.checks.iter().map(|c| c.category).collect();
        categories.dedup();
        for category in categories {
            blocks.push(ViewNode::Tree(TreeItem {
                label: vec![Span::toned(category_title(category), Tone::Emphasis)],
                children: health
                    .checks
                    .iter()
                    .filter(|c| c.category == category)
                    .map(check_item)
                    .collect(),
            }));
        }
        if health.checks.is_empty() {
            blocks.push(ViewNode::Paragraph(vec![Span::toned(
                "no check matches these options",
                Tone::Muted,
            )]));
        }
        ViewNode::Group(blocks)
    }
}

#[cfg(test)]
mod tests {
    use ods_core::Evidence;

    use super::*;
    use crate::output::{ColorChoice, Mode, OutputSettings};
    use crate::present::emit;

    /// One check of each status, with evidence, sources and hints: every part of the
    /// view.
    fn fixture(strict: bool) -> DoctorReport {
        let checks = vec![
            CheckResult::ok(
                "config.load",
                CheckCategory::Config,
                "1 configuration file(s) loaded",
            )
            .required(true)
            .evidence(Evidence::new("project file", "ods.toml").from_source("loaded")),
            CheckResult::warning(
                "project.freshness",
                CheckCategory::Project,
                codes::STALE_ARTIFACTS,
                "the manifest is older than `models/orders.sql`",
            )
            .provider("dbt")
            .fact("manifest_modified", "2026-01-01T00:00:00Z")
            .hint("parse the project again: `dbt parse`"),
            CheckResult::error(
                "tools.dbt",
                CheckCategory::Tools,
                codes::DBT_MISSING,
                "dbt can't be run",
            )
            .required(true)
            .provider("dbt")
            .hint("install dbt-core"),
            CheckResult::unknown(
                "target.identity",
                CheckCategory::Target,
                codes::BLOCKED,
                "not checked: `tools.dbt` failed",
            )
            .required(true)
            .provider("dbt")
            .fact("depends_on", "tools.dbt"),
            CheckResult::skipped(
                "connectivity.relations",
                CheckCategory::Connectivity,
                "a live check: pass --connect to run it",
            )
            .provider("dbt"),
        ];
        DoctorReport::new(
            &Options {
                strict,
                ..Options::default()
            },
            checks,
        )
    }

    fn render(report: &DoctorReport, mode: Mode, color: ColorChoice) -> String {
        let settings = OutputSettings {
            mode,
            color,
            width: Some(100),
        };
        let mut out = Vec::new();
        emit(report, &settings, &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .replace(env!("CARGO_PKG_VERSION"), "[ods-version]")
    }

    #[test]
    fn a_failing_check_exits_5_with_the_doctor_code() {
        let error = fixture(false).failure().unwrap();
        assert_eq!(error.status, ExitStatus::CheckFailed);
        assert_eq!(error.code, codes::DOCTOR_FAILED);
        assert_eq!(error.message, "2 checks failed: tools.dbt, target.identity");
    }

    #[test]
    fn exit_semantics_are_deterministic() {
        use CheckStatus::*;
        let only = |status: CheckStatus, required: bool, strict: bool| {
            let check = match status {
                CheckStatus::Ok => CheckResult::ok("a.b", CheckCategory::Project, "x"),
                CheckStatus::Warning => {
                    CheckResult::warning("a.b", CheckCategory::Project, "ODS-W0206", "x")
                }
                CheckStatus::Unknown => {
                    CheckResult::unknown("a.b", CheckCategory::Project, "ODS-U0001", "x")
                }
                CheckStatus::Error => {
                    CheckResult::error("a.b", CheckCategory::Project, "ODS-E0204", "x")
                }
                _ => CheckResult::skipped("a.b", CheckCategory::Project, "x"),
            }
            .required(required);
            let options = Options {
                strict,
                ..Options::default()
            };
            DoctorReport::new(&options, vec![check])
                .failure()
                .map_or(0, |e| e.status.code())
        };
        // (status, required, strict) -> exit status, the same every time.
        let table = [
            (Ok, true, false, 0),
            (Skipped, true, true, 0),
            (Warning, false, false, 0),
            (Warning, false, true, 5),
            (Unknown, false, false, 0),
            (Unknown, false, true, 5),
            (Unknown, true, false, 5),
            (Error, false, false, 5),
            (Error, true, true, 5),
        ];
        for (status, required, strict, code) in table {
            for _ in 0..3 {
                assert_eq!(
                    only(status, required, strict),
                    code,
                    "{status:?} {required} {strict}"
                );
            }
        }
    }

    #[test]
    fn every_check_shows_its_status_code_evidence_and_hint() {
        let text = render(&fixture(false), Mode::Plain, ColorChoice::Never);
        for needle in [
            "[ok] config.load (required)",
            "[warning ODS-W0206] project.freshness",
            "[error ODS-E0502] tools.dbt (required)",
            "[unknown ODS-U0001] target.identity",
            "[skipped] connectivity.relations",
            "project file: ods.toml (loaded)",
            "hint: install dbt-core",
            "depends_on: tools.dbt",
        ] {
            assert!(text.contains(needle), "{needle} missing from:\n{text}");
        }
    }

    /// Contract snapshots (ADR-0003 §5).
    mod contract {
        use super::*;

        fn snapshot(name: &str, output: &str) {
            insta::with_settings!({ snapshot_path => "../snapshots" }, {
                insta::assert_snapshot!(name, output);
            });
        }

        #[test]
        fn doctor_json() {
            snapshot(
                "doctor_json",
                &render(&fixture(false), Mode::Json, ColorChoice::Never),
            );
        }

        #[test]
        fn doctor_plain() {
            snapshot(
                "doctor_plain",
                &render(&fixture(false), Mode::Plain, ColorChoice::Never),
            );
        }
    }

    /// Presentation snapshots, apart from the contract (they change with rs-rich).
    mod rich {
        use super::*;

        #[test]
        fn doctor_human() {
            let output = render(&fixture(true), Mode::Human, ColorChoice::Never);
            insta::with_settings!({ snapshot_path => "../snapshots/rich", prepend_module_to_snapshot => false }, {
                insta::assert_snapshot!("doctor_human", output);
            });
        }
    }
}
