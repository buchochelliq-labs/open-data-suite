//! `ods health check` (#392, ADR-0030 §6): runs every enabled health check, built-in and
//! registered, on the project's nodes, records the findings beside the state store, and
//! exits 5 when a check at severity `error` fails, so CI can gate on it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_config::Loaded;
use ods_core::state::Timestamp;
use ods_health::record::{self, HealthRecord};
use ods_health::{
    CheckRun, CheckSource, CoverageFinding, Finding, Health, HealthBadge, HealthReport, Severity,
};
use ods_health::{ElevatedLogin, HEALTHS, LastFailures, ProbeConnection, Status};
use ods_sdk::contracts::state_store::StateStore;
use ods_store_sqlite::SqliteStateStore;
use serde::Serialize;

use super::relation_links::{LinkSettings, Links};
use super::state_plan::{Sources, Workspace, block_on, common, store_error};
use super::state_settings::{Origin, StateSettings};
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, Module};
use crate::present::{Diagnostic, Level, Present, Span, Tone, ViewNode};

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
                if args.get_flag("allow-elevated-login") {
                    // Before anything runs, whatever the output mode or what happens next.
                    let _ = writeln!(std::io::stderr(), "warning: {ELEVATED_WARNING}");
                }
                let report = CheckReport::build(args, ctx.config)?;
                match report.failure() {
                    Some(error) => ctx.emit_failed(&report, error),
                    None => ctx.emit(&report),
                }
            }
            Some(("trust", args)) => {
                let mut health = super::serve_dashboard::health_settings(ctx.config)?;
                let settings = StateSettings::resolve(args, ctx.config)?;
                health.pin_probe_connection(&probe_pins(&settings));
                let report = super::health_trust::TrustReport::build(args, ctx.config, &health)?;
                ctx.emit(&report)
            }
            _ => unreachable!("clap requires a known subcommand"),
        }
    }
}

fn check_command() -> Command {
    super::state_run::dbt_invocation_options(common(
        Command::new("check")
            .about("Run every enabled health check; exit 5 when one at severity error fails"),
    ))
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
        Arg::new("allow-elevated-login")
            .long("allow-elevated-login")
            .action(ArgAction::SetTrue)
            .help("Run probe checks even when their login can do more than read, or that can't be told: at your own risk, for this run only")
            .long_help(
                "Run probe checks even when the login of `[health.probes]`'s target can do \
                 more than read what they probe, or when that can't be told (ADR-0030 §4c). \
                 Each probe must still be one read-only query and trusted. ODS can't stop \
                 a query from writing under a login that may write: you use this at your \
                 own risk. ODS comes with no warranty (see its licence), and its authors \
                 aren't responsible for any consequence, including changed or destroyed \
                 data. It is a flag only, never a setting, and every run that uses it \
                 says so, with the login and what it can do.",
            ),
    )
    .arg(
        Arg::new("no-record")
            .long("no-record")
            .action(ArgAction::SetTrue)
            .help("Don't keep the findings beside the state store for the dashboard"),
    )
}

/// Keeps `record` beside the state store at `state_db`; where.
///
/// # Errors
/// It can't be written.
fn write_record(
    state_db: &std::path::Path,
    record: &HealthRecord,
    allow_elevated_login: bool,
) -> Result<PathBuf, CliError> {
    let dir = record::dir_for(state_db);
    record::write(&dir, record).map_err(|e| {
        CliError::new(
            ExitStatus::Failure,
            codes::HEALTH_RECORD,
            format!(
                "the health record can't be written in {}: {e}",
                dir.display()
            ),
        )
        .with_hint(if allow_elevated_login {
            "the records already there are unchanged; `--no-record` checks without keeping one. Probes ran with `--allow-elevated-login`, possibly under a login that can do more than read"
        } else {
            "the records already there are unchanged; `--no-record` checks without keeping one"
        })
    })
}

/// The dbt settings that decide where probes connect and that configuration set (so a
/// project can): the program, the project and profiles directories and the profile.
/// They are pinned in each probe's trust digest. Flags and `DBT_*` variables are the
/// user's own, so they aren't.
fn probe_pins(settings: &StateSettings) -> BTreeMap<String, String> {
    [
        ("dbt.program", Some(&settings.program)),
        ("dbt.project_dir", settings.project_dir.as_ref()),
        ("dbt.profiles_dir", settings.profiles_dir.as_ref()),
        ("dbt.profile", settings.profile.as_ref()),
    ]
    .into_iter()
    .filter_map(|(key, setting)| {
        let setting = setting?;
        matches!(setting.origin, Origin::Config(_)).then(|| (key.to_owned(), setting.value.clone()))
    })
    .collect()
}

/// What probe checks run through: dbt, on `[health.probes]`'s target (ADR-0030 §4c).
/// `None` when no probe is configured, or no target is: probes never borrow the build's
/// own target, which can write. dbt can't say what its login may do, so every probe it
/// runs needs `--allow-elevated-login` until a provider reports privileges. Each node
/// is probed only on the relation its build made, as the manifest names it.
///
/// # Errors
/// The probe target is the build's.
fn probe_connection(
    args: &ArgMatches,
    config: &Loaded,
    settings: &StateSettings,
    health: &ods_health::HealthSettings,
    manifest: &ods_provider_dbt::Manifest,
) -> Result<Option<ProbeConnection>, CliError> {
    if health.probe_definitions().is_empty() {
        return Ok(None);
    }
    let Some(probes) = config.config.health.probes.as_ref() else {
        return Ok(None);
    };
    let Some(target) = probes.target.as_deref() else {
        return Ok(None);
    };
    if settings.target.as_ref().is_some_and(|t| t.value == target) {
        return Err(CliError::new(
            ExitStatus::Config,
            codes::HEALTH_CONFIG,
            format!(
                "health.probes.target: `{target}` is the target the project builds with; probes never run on it"
            ),
        )
        .with_hint("name a target in profiles.yml whose login can only read what the probes select"));
    }
    let mut executor = super::state_run::executor(args, settings).target(target);
    let mut label = format!("dbt target `{target}`");
    if let Some(profile) = &probes.profile {
        executor = executor.profile(profile);
        let _ = write!(label, " (profile `{profile}`)");
    }
    let relations = manifest
        .nodes
        .iter()
        .filter_map(|n| Some((n.unique_id.clone(), n.relation_name.clone()?)))
        .collect();
    Ok(Some(
        ProbeConnection::without_privileges(Arc::new(executor))
            .labelled(label)
            .expecting(relations),
    ))
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
    /// With `--allow-elevated-login` only: the probes that ran under a login that can do
    /// more than read, or couldn't be shown to only read: the login, and what was found
    /// on each node's relation (empty when no probe needed it).
    #[serde(skip_serializing_if = "Option::is_none")]
    elevated_login: Option<ElevatedLogin>,
    /// Enabled probe checks that select no node health checks see (models, seeds and
    /// snapshots): they checked nothing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unmatched_probes: Vec<String>,
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
        // Where probes connect, as far as configuration decides it, is part of what is
        // trusted (ADR-0030 §4d).
        health.pin_probe_connection(&probe_pins(&settings));
        // Probe SQL again in the project's own dialect, then trust (ADR-0030 §4a, §4b).
        super::serve_dashboard::check_probe_sql(&mut health, ws.manifest.adapter_type.as_deref())?;
        super::health_trust::apply(&mut health, config, args.get_flag("allow-scripts"));
        let allow_elevated_login = args.get_flag("allow-elevated-login");
        if let Some(connection) = probe_connection(args, config, &settings, &health, &ws.manifest)?
        {
            health = health
                .with_probe_connection(connection.allowing_elevated_login(allow_elevated_login));
        }
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
            Some(write_record(
                &ws.state_db,
                &HealthRecord::new(&scope, checked_at, report.clone()),
                allow_elevated_login,
            )?)
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
            elevated_login: allow_elevated_login.then(|| report.elevated_login.unwrap_or_default()),
            unmatched_probes: report.unmatched_probes,
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

impl CheckReport {
    /// How many nodes have each health, and where the findings were recorded.
    fn counts_view(&self) -> ViewNode {
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
        ViewNode::KeyValue(facts)
    }
}

impl Present for CheckReport {
    const COMMAND: &'static str = "health.check";

    fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut out: Vec<Diagnostic> = self
            .elevated_login
            .iter()
            .map(|elevated| {
                Diagnostic::warning(codes::HEALTH_ELEVATED_LOGIN, elevated_warning(elevated))
            })
            .collect();
        if !self.unmatched_probes.is_empty() {
            out.push(Diagnostic::warning(
                codes::HEALTH_UNMATCHED_PROBE,
                unmatched_warning(&self.unmatched_probes),
            ));
        }
        out
    }

    fn view(&self) -> ViewNode {
        let mut blocks = vec![
            ViewNode::Heading(format!("Health of {}", self.scope)),
            self.counts_view(),
        ];
        if !self.unmatched_probes.is_empty() {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(unmatched_warning(&self.unmatched_probes))],
            });
        }
        if let Some(elevated) = &self.elevated_login {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(elevated_warning(elevated))],
            });
        }
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

/// What every run with `--allow-elevated-login` says (ADR-0030 §4c): that ODS can't stop
/// a probe from writing under a login that may, and that it is at the user's own risk.
const ELEVATED_WARNING: &str = "--allow-elevated-login: probe checks may run under a login that \
    can do more than read. ODS checks each probe is one read-only query, but it can't stop a query \
    from writing under a login that may write. You run them at your own risk: ODS comes with no \
    warranty (see its licence), and its authors aren't responsible for any consequence, including \
    changed or destroyed data.";

/// [`ELEVATED_WARNING`], and, when probes ran so, the connection, its login and what it
/// can do on each node's relation.
fn elevated_warning(elevated: &ElevatedLogin) -> String {
    let mut text = ELEVATED_WARNING.to_owned();
    if elevated.found.is_empty() {
        text.push_str(" This run, no probe needed it.");
        return text;
    }
    let who = match (&elevated.login, &elevated.connection) {
        (Some(login), Some(via)) => format!("the login `{login}` of {via}"),
        (Some(login), None) => format!("the login `{login}`"),
        (None, Some(via)) => format!("the login of {via}"),
        (None, None) => "the probe login".to_owned(),
    };
    let _ = write!(text, " This run, probes ran under {who}, which, on");
    for (i, (node, found)) in elevated.found.iter().enumerate() {
        let sep = if i == 0 { " " } else { "; on " };
        let _ = write!(text, "{sep}`{node}`, {found}");
    }
    text.push('.');
    text
}

/// What a run says about probe checks that selected nothing.
fn unmatched_warning(ids: &[String]) -> String {
    format!(
        "probe check{} {} select{} no model, seed or snapshot, so {} checked nothing (sources can't be probed yet); with --strict, one at severity error fails",
        if ids.len() == 1 { "" } else { "s" },
        ids.iter()
            .map(|id| format!("`{id}`"))
            .collect::<Vec<_>>()
            .join(", "),
        if ids.len() == 1 { "s" } else { "" },
        if ids.len() == 1 { "it" } else { "they" },
    )
}
