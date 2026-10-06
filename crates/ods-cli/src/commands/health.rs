//! `ods health check` (#392, ADR-0030 §6): runs every enabled health check, built-in and
//! registered, on the project's nodes, records the findings beside the state store, and
//! exits 5 when a check at severity `error` fails, so CI can gate on it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_config::Loaded;
use ods_core::state::Timestamp;
use ods_health::record::{self, HealthRecord};
use ods_health::{
    CheckRun, CheckSource, CoverageFinding, Finding, Health, HealthBadge, HealthReport, Severity,
};
use ods_health::{HEALTHS, LastFailures, Status};
use ods_sdk::contracts::state_store::StateStore;
use ods_store_sqlite::SqliteStateStore;
use serde::Serialize;

use super::relation_links::{LinkSettings, Links};
use super::state_plan::{Sources, Workspace, block_on, common, store_error};
use super::state_settings::StateSettings;
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, Module};
use crate::present::{Level, Present, Span, Tone, ViewNode};

/// `ods health`.
pub struct HealthCommand;

impl Module for HealthCommand {
    fn command(&self) -> Command {
        Command::new("health")
            .about("Check the project's health: built, tested, and whatever `[health]` adds")
            .subcommand_required(true)
            .arg_required_else_help(true)
            .subcommand(check_command())
            .subcommand(super::health_trust::command())
    }

    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        match matches.subcommand() {
            Some(("check", args)) => {
                let report = CheckReport::build(args, ctx.config)?;
                match report.failure() {
                    Some(error) => ctx.emit_failed(&report, error),
                    None => ctx.emit(&report),
                }
            }
            Some(("trust", args)) => {
                let health = super::serve_dashboard::health_settings(ctx.config)?;
                let report = super::health_trust::TrustReport::build(args, ctx.config, &health)?;
                ctx.emit(&report)
            }
            _ => unreachable!("clap requires a known subcommand"),
        }
    }
}

fn check_command() -> Command {
    common(
        Command::new("check")
            .about("Run every enabled health check; exit 5 when one at severity error fails"),
    )
    .arg(
        Arg::new("strict")
            .long("strict")
            .action(ArgAction::SetTrue)
            .help("Also fail when a check at severity error can't decide (unknown)"),
    )
    .arg(
        Arg::new("allow-scripts")
            .long("allow-scripts")
            .action(ArgAction::SetTrue)
            .help("Trust this project's probe checks as they are now, for this run only (e.g. in CI); `ods health trust` keeps trust"),
    )
    .arg(
        Arg::new("no-record")
            .long("no-record")
            .action(ArgAction::SetTrue)
            .help("Don't keep the findings beside the state store for the dashboard"),
    )
}

/// One node that isn't healthy, or every node in JSON.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct NodeHealth {
    id: String,
    health: Health,
    reasons: Vec<String>,
    findings: Vec<Finding>,
}

/// `ods health check`'s report.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct CheckReport {
    scope: String,
    state_db: PathBuf,
    checked_at: Timestamp,
    strict: bool,
    /// Where the findings were recorded; `None` with `--no-record`, or when there is no
    /// state store to keep them beside.
    recorded: Option<PathBuf>,
    /// Whether the last run's record said which nodes failed: without it, the
    /// `last_run_*` checks are unknown.
    last_run_known: bool,
    checks: Vec<CheckRun>,
    /// How many nodes have each health.
    counts: BTreeMap<Health, usize>,
    /// Every node, by id.
    nodes: Vec<NodeHealth>,
    /// Each coverage target's verdict on the project as a whole (`[health.coverage]`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    coverage: Vec<CoverageFinding>,
    /// Whether the verdict fails: exit 5.
    failed: bool,
}

impl CheckReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let mut health = super::serve_dashboard::health_settings(config)?;
        let strict = args.get_flag("strict");
        let settings = StateSettings::resolve(args, config)?;
        // Source freshness results as the dashboard reads them (`--sources`, else
        // `<target-dir>/sources.json`), so coverage is judged on the same evidence.
        let ws = Workspace::load(args, &settings, Sources::AsGiven)?;
        let scope = ws.scope.to_string();
        // Read only: no database is created or migrated to check health.
        let (latest, runs_index) = if ws.state_db.is_file() {
            let store = block_on(SqliteStateStore::open_existing(&ws.state_db))?
                .map_err(|e| store_error(&e))?;
            let read = block_on(async {
                let runs_index = store
                    .history(&ws.scope, super::serve_dashboard::RUNS_INDEXED)
                    .await?;
                let latest = store.latest(&ws.scope).await?;
                store.close().await;
                Ok::<_, ods_sdk::ProviderError>((latest, runs_index))
            })?;
            read.map_err(|e| store_error(&e))?
        } else {
            (None, Vec::new())
        };
        let last_builds =
            super::serve_catalog::last_builds(latest.as_ref(), &runs_index, &ws.manifest);
        let links = Links::new(
            ws.manifest.adapter_type.as_deref(),
            &LinkSettings::read(config),
        );
        let catalog =
            super::serve_catalog::catalog(&ws.manifest, &ws.target_dir, last_builds, &links);
        // Probe SQL again in the project's own dialect, then trust (ADR-0030 §4a, §4b).
        super::serve_dashboard::check_probe_sql(&mut health, ws.manifest.adapter_type.as_deref())?;
        super::health_trust::apply(&mut health, config, args.get_flag("allow-scripts"));
        let failures = last_failures(&ws, &scope);
        let last_run_known = failures.is_some();
        let check_scope = ods_web::health::check_scope(&catalog, failures);
        let checked_at = Timestamp::now();
        // Coverage is measured as the dashboard measures it (ADR-0030 §3).
        let measured =
            ods_web::health::project_coverage(&catalog, &super::serve_catalog::freshness(&ws));
        let report: HealthReport = block_on(health.run(&check_scope, ods_health::CHECK_TIMEOUT))?
            .with_coverage(health.coverage(&measured));
        let recorded = if args.get_flag("no-record") || !ws.state_db.is_file() {
            None
        } else {
            let dir = record::dir_for(&ws.state_db);
            let written = record::write(&dir, &HealthRecord::new(&scope, checked_at, report.clone()))
                .map_err(|e| {
                    CliError::new(
                        ExitStatus::Failure,
                        codes::HEALTH_RECORD,
                        format!(
                            "the health record can't be written in {}: {e}",
                            dir.display()
                        ),
                    )
                    .with_hint("the records already there are unchanged; `--no-record` checks without keeping one")
                })?;
            Some(written)
        };
        let failed = report.fails(strict);
        let counts = ods_health::counts(report.badges.values());
        Ok(Self {
            scope,
            state_db: ws.state_db.clone(),
            checked_at,
            strict,
            recorded,
            last_run_known,
            checks: report.checks,
            coverage: report.coverage,
            counts,
            nodes: report
                .badges
                .into_iter()
                .map(|(id, badge)| {
                    let HealthBadge {
                        health,
                        reasons,
                        findings,
                        ..
                    } = badge;
                    NodeHealth {
                        id,
                        health,
                        reasons,
                        findings,
                    }
                })
                .collect(),
            failed,
        })
    }

    /// The error to exit with when the verdict fails (exit 5).
    fn failure(&self) -> Option<CliError> {
        if !self.failed {
            return None;
        }
        let failing: Vec<&str> = self
            .nodes
            .iter()
            .filter(|n| n.findings.iter().any(|f| self.fails(f.severity, f.status)))
            .map(|n| n.id.as_str())
            .collect();
        let missed: Vec<&str> = self
            .coverage
            .iter()
            .filter(|c| self.fails(c.severity, c.status))
            .map(|c| c.measure.as_str())
            .collect();
        let mut parts = Vec::new();
        if !failing.is_empty() {
            parts.push(format!(
                "{} node{} fail{} a health check at severity error",
                failing.len(),
                if failing.len() == 1 { "" } else { "s" },
                if failing.len() == 1 { "s" } else { "" },
            ));
        }
        if !missed.is_empty() {
            parts.push(format!(
                "the {} coverage target{} at severity error {} met",
                missed.join(", "),
                if missed.len() == 1 { "" } else { "s" },
                if missed.len() == 1 { "isn't" } else { "aren't" },
            ));
        }
        Some(
            CliError::new(
                ExitStatus::CheckFailed,
                codes::HEALTH_FAILED,
                parts.join("; "),
            )
            .with_hint(if self.strict {
                "each node above says why; with --strict, a check at severity error that can't decide fails too"
            } else {
                "each node above says why; `[health.builtin.<check>] severity` sets how much a check counts"
            }),
        )
    }

    /// The coverage targets and their verdicts, when any is set.
    fn coverage_table(&self) -> Option<ViewNode> {
        (!self.coverage.is_empty()).then(|| ViewNode::Table {
            title: Some("Coverage targets".to_owned()),
            columns: vec![
                "measure".into(),
                "target".into(),
                "verdict".into(),
                "why".into(),
            ],
            rows: self
                .coverage
                .iter()
                .map(|c| {
                    let (verdict, tone) = match c.status {
                        Status::Pass => ("met", Tone::Success),
                        Status::Fail if c.severity == Severity::Error => ("missed", Tone::Error),
                        Status::Fail => ("missed", Tone::Warning),
                        _ => ("unknown", Tone::Muted),
                    };
                    vec![
                        vec![Span::toned(c.measure.as_str(), Tone::Code)],
                        vec![Span::plain(format!(
                            "{} ({})",
                            c.target,
                            severity(c.severity)
                        ))],
                        vec![Span::toned(verdict, tone)],
                        vec![Span::plain(c.reason.as_str())],
                    ]
                })
                .collect(),
            breaks: Vec::new(),
            footer: None,
        })
    }

    /// Whether a finding or verdict at `severity` with `status` makes the verdict fail.
    fn fails(&self, severity: Severity, status: Status) -> bool {
        severity == Severity::Error
            && (status == Status::Fail || (self.strict && status == Status::Unknown))
    }
}

/// What the last run kept beside the store says failed, if it ran for `scope` and says.
fn last_failures(ws: &Workspace, scope: &str) -> Option<LastFailures> {
    let (_, last) = super::state_retry::peek(&ws.state_db)?;
    if last.scope.as_deref() != Some(scope) {
        return None;
    }
    let outcome = last.outcome.as_ref()?;
    Some(LastFailures::new(
        last.recorded_at,
        format!("ods state {}", last.command),
        outcome.failed.iter().cloned(),
        outcome.skipped.iter().cloned(),
    ))
}

fn tone(health: Health) -> Tone {
    match health {
        Health::Healthy => Tone::Success,
        Health::Warning => Tone::Warning,
        Health::Failing => Tone::Error,
        _ => Tone::Muted,
    }
}

fn source(source: CheckSource) -> &'static str {
    match source {
        CheckSource::Builtin => "built-in",
        CheckSource::Declarative => "declared",
        _ => "plugin",
    }
}

fn severity(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warn => "warn",
        _ => "info",
    }
}

impl Present for CheckReport {
    const COMMAND: &'static str = "health.check";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![ViewNode::Heading(format!("Health of {}", self.scope))];
        let mut facts: Vec<(String, Vec<Span>)> = HEALTHS
            .iter()
            .map(|&health| {
                let count = self.counts.get(&health).copied().unwrap_or(0);
                (
                    health.key().to_owned(),
                    vec![Span::toned(
                        count.to_string(),
                        if count == 0 {
                            Tone::Muted
                        } else {
                            tone(health)
                        },
                    )],
                )
            })
            .collect();
        facts.push((
            "recorded".to_owned(),
            vec![match &self.recorded {
                Some(path) => Span::toned(path.display().to_string(), Tone::Code),
                None => Span::toned("no", Tone::Muted),
            }],
        ));
        blocks.push(ViewNode::KeyValue(facts));
        if !self.last_run_known {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(
                    "no record of the last run for this scope says what failed, so the last-run checks are unknown",
                )],
            });
        }
        blocks.push(ViewNode::Table {
            title: Some("Checks".to_owned()),
            columns: vec![
                "check".into(),
                "source".into(),
                "severity".into(),
                "what it checks".into(),
            ],
            rows: self
                .checks
                .iter()
                .map(|c| {
                    vec![
                        vec![Span::toned(c.id.as_str(), Tone::Code)],
                        vec![Span::plain(source(c.source))],
                        vec![Span::plain(severity(c.severity))],
                        vec![Span::plain(c.about.as_str())],
                    ]
                })
                .collect(),
            breaks: Vec::new(),
            footer: None,
        });
        blocks.extend(self.coverage_table());
        let unhealthy: Vec<&NodeHealth> = self
            .nodes
            .iter()
            .filter(|n| n.health != Health::Healthy)
            .collect();
        if unhealthy.is_empty() {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(if self.nodes.is_empty() {
                    "the project has no nodes to check"
                } else {
                    "every node is healthy"
                })],
            });
        } else {
            let mut sorted = unhealthy;
            sorted.sort_by_key(|n| (n.health != Health::Failing, n.health, n.id.as_str()));
            blocks.push(ViewNode::Table {
                title: Some("Not healthy".to_owned()),
                columns: vec!["node".into(), "health".into(), "why".into()],
                rows: sorted
                    .iter()
                    .map(|n| {
                        vec![
                            vec![Span::toned(n.id.as_str(), Tone::Code)],
                            vec![Span::toned(n.health.key(), tone(n.health))],
                            vec![Span::plain(n.reasons.join("; "))],
                        ]
                    })
                    .collect(),
                breaks: Vec::new(),
                footer: None,
            });
        }
        ViewNode::Group(blocks)
    }
}
