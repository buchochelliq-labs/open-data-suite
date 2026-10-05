//! `ods state savings` (#210, ADR-0029): what reuse saved, per run and in total, from
//! the state store's run ledger. Every figure is an estimate, and says which runs'
//! timings it used; reused nodes without a timing are counted, never guessed.

use std::path::PathBuf;

use clap::{Arg, ArgMatches, Command};
use ods_config::Loaded;
use ods_core::state::{RunEntry, RunEntryOutcome, Timestamp};
use ods_sdk::ProviderError;
use ods_sdk::contracts::state_store::StateStore;
use serde::Serialize;

use ods_store_sqlite::SqliteStateStore;

use super::state_plan::{Sources, Workspace, block_on, common, store_error};
use super::state_settings::StateSettings;
use crate::exit::{CliError, ExitStatus, codes};
use crate::present::{Level, Present, Span, Tone, ViewNode};

/// `ods state savings`'s arguments.
pub(super) fn savings_command() -> Command {
    common(Command::new("savings").about(
        "What reusing builds saved, per run and in total: estimates from each build's last measured time",
    ))
    .arg(
        Arg::new("since")
            .long("since")
            .value_name("DATE")
            .help("Only runs that finished on or after this date (`2026-10-01`) or time (RFC 3339)"),
    )
    .arg(
        Arg::new("limit")
            .long("limit")
            .value_name("N")
            .value_parser(clap::value_parser!(usize))
            .default_value("20")
            .help("How many runs to list; the totals count every run since `--since`"),
    )
}

/// One run's savings.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct RunSavings {
    run_id: String,
    finished_at: Timestamp,
    outcome: RunEntryOutcome,
    #[serde(flatten)]
    savings: ods_state::Savings,
    /// What the time avoided cost, at `[state.cost]`'s rate, when it is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<f64>,
}

/// The rate `[state.cost]` sets, and what the total time avoided cost at it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct Cost {
    rate_per_hour: f64,
    unit: String,
    /// What [`SavingsReport::totals`]'s time avoided cost.
    total: f64,
}

/// `ods state savings`'s report.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct SavingsReport {
    state_db: PathBuf,
    scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<Timestamp>,
    /// Every figure is an estimate of serial build time (ADR-0029 §2).
    estimate: bool,
    /// Whether the store keeps a run ledger; without one there is nothing to report.
    ledger: bool,
    /// How many runs the totals count.
    run_count: usize,
    /// The newest runs, at most `--limit`.
    runs: Vec<RunSavings>,
    /// Over every run since `--since`.
    totals: ods_state::Savings,
    /// What the time avoided cost, when `[state.cost]` sets a rate (ADR-0029): an
    /// estimate too.
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<Cost>,
}

impl SavingsReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let settings = StateSettings::resolve(args, config)?;
        let since = args
            .get_one::<String>("since")
            .map(|s| parse_since(s))
            .transpose()?;
        let limit = args.get_one::<usize>("limit").copied().unwrap_or(20);
        let ws = Workspace::load(args, &settings, Sources::Ignore)?;
        let mut report = Self {
            state_db: ws.state_db.clone(),
            scope: ws.scope.to_string(),
            since,
            estimate: true,
            ledger: true,
            run_count: 0,
            runs: Vec::new(),
            totals: ods_state::Savings::default(),
            cost: None,
        };
        let rate = config.config.state.cost.clone();
        // Reading changes nothing: no database is created, and none is migrated.
        if !ws.state_db.is_file() {
            return Ok(report);
        }
        let store = block_on(SqliteStateStore::open_existing(&ws.state_db))?
            .map_err(|e| store_error(&e))?;
        // A database from before the run ledger (version 1) has no runs yet; the next run
        // that writes migrates it.
        if block_on(store.schema_version())?.map_err(|e| store_error(&e))? < 2 {
            return Ok(report);
        }
        let entries: Vec<RunEntry> = match block_on(store.runs(&ws.scope, since, usize::MAX))? {
            Ok(entries) => entries,
            Err(ProviderError::Unsupported(_)) => {
                report.ledger = false;
                return Ok(report);
            }
            // The ledger, not the state: say so, rather than "the state is damaged".
            Err(e) => {
                return Err(CliError::new(
                    ExitStatus::Failure,
                    codes::STATE_STORE,
                    format!("the run ledger can't be read: {e}"),
                )
                .with_hint(
                    "the state itself is unaffected; `ods state doctor` lists the runs it can't read",
                ));
            }
        };
        report.run_count = entries.len();
        for entry in &entries {
            report.totals.add(&ods_state::run_savings(entry));
        }
        report.runs = entries
            .iter()
            .take(limit)
            .map(|e| RunSavings {
                run_id: e.run_id.clone(),
                finished_at: e.finished_at,
                outcome: e.outcome,
                savings: ods_state::run_savings(e),
                cost: None,
            })
            .collect();
        if let Some(rate) = rate {
            for run in &mut report.runs {
                run.cost = Some(rate.cost_of(run.savings.avoided_ms));
            }
            report.cost = Some(Cost {
                total: rate.cost_of(report.totals.avoided_ms),
                rate_per_hour: rate.rate_per_hour,
                unit: rate.unit,
            });
        }
        Ok(report)
    }
}

/// `2026-10-01` (midnight UTC) or an RFC 3339 time.
fn parse_since(text: &str) -> Result<Timestamp, CliError> {
    let full = if text.len() == 10 {
        format!("{text}T00:00:00Z")
    } else {
        text.to_owned()
    };
    Timestamp::parse(&full).map_err(|e| {
        CliError::new(
            ExitStatus::Usage,
            codes::STATE_INPUT,
            format!(
                "`--since {}` isn't a date or time: {e}",
                text.escape_debug()
            ),
        )
        .with_hint("give a date (`2026-10-01`) or an RFC 3339 time (`2026-10-01T09:00:00Z`)")
    })
}

/// The time saved, for people: `~4m 12s`, `at least ~4m 12s`, `none` when nothing was
/// reused, or `—` when nothing reused has a build time (unknown, not zero).
fn saved(savings: &ods_state::Savings) -> String {
    if savings.reused == 0 {
        return "none".to_owned();
    }
    if savings.timed == 0 {
        return super::run_stats::MISSING.to_owned();
    }
    let time = super::run_stats::duration(savings.avoided_ms);
    if savings.is_lower_bound() {
        format!("at least ~{time}")
    } else {
        format!("~{time}")
    }
}

/// A cost, for people: `1.25 USD`, `< 0.01 USD` for a little, `0 USD` for none.
fn money(amount: f64, unit: &str) -> String {
    if amount <= 0.0 {
        format!("0 {unit}")
    } else if amount < 0.005 {
        format!("< 0.01 {unit}")
    } else {
        format!("{amount:.2} {unit}")
    }
}

fn outcome(outcome: RunEntryOutcome) -> Span {
    match outcome {
        RunEntryOutcome::NothingToBuild => Span::toned("nothing to build", Tone::Success),
        RunEntryOutcome::Succeeded => Span::toned("succeeded", Tone::Success),
        RunEntryOutcome::Failed => Span::toned("failed", Tone::Error),
        RunEntryOutcome::NotRecorded => Span::toned("not recorded", Tone::Error),
        _ => Span::plain("unknown"),
    }
}

impl Present for SavingsReport {
    const COMMAND: &'static str = "state.savings";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![ViewNode::Heading(format!(
            "What reuse saved in {}",
            self.scope
        ))];
        if !self.ledger {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain(
                    "this state store keeps no run ledger, so there is nothing to report",
                )],
            });
            return ViewNode::Group(blocks);
        }
        if self.run_count == 0 {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(
                    "no runs in the ledger yet: each `ods state run`, `build`, `seed`, `snapshot` and `retry` from this version on adds one",
                )],
            });
            return ViewNode::Group(blocks);
        }
        let t = &self.totals;
        let mut total = vec![if t.reused == 0 {
            Span::toned("none: no run has reused a node yet", Tone::Muted)
        } else if t.timed == 0 {
            Span::toned("unknown: no reused node has a build time", Tone::Muted)
        } else {
            Span::toned(format!("{} of build time", saved(t)), Tone::Success)
        }];
        total.push(Span::toned(
            format!(
                " (estimate, serial: {} of {} nodes reused across {} run{}",
                t.reused,
                t.reused + t.built,
                self.run_count,
                if self.run_count == 1 { "" } else { "s" }
            ),
            Tone::Muted,
        ));
        if t.untimed > 0 {
            total.push(Span::toned(
                format!("; {} reused without a build time, not counted", t.untimed),
                Tone::Muted,
            ));
        }
        total.push(Span::toned(")", Tone::Muted));
        let mut facts = vec![("saved".to_owned(), total)];
        if let Some(cost) = &self.cost {
            facts.push((
                "cost avoided".to_owned(),
                vec![
                    Span::toned(
                        format!(
                            "{}~{}",
                            if t.is_lower_bound() { "at least " } else { "" },
                            money(cost.total, &cost.unit)
                        ),
                        Tone::Success,
                    ),
                    Span::toned(
                        format!(
                            " (estimate, at {} {} per hour of build time, `[state.cost]`)",
                            cost.rate_per_hour, cost.unit
                        ),
                        Tone::Muted,
                    ),
                ],
            ));
        }
        if let Some(since) = self.since {
            facts.push(("since".to_owned(), vec![Span::plain(since.to_string())]));
        }
        facts.push((
            "timings from".to_owned(),
            vec![Span::plain(match t.timed_by.len() {
                0 => "no run".to_owned(),
                1 => "1 run".to_owned(),
                n => format!("{n} runs"),
            })],
        ));
        blocks.push(ViewNode::KeyValue(facts));
        blocks.push(self.runs_table());
        ViewNode::Group(blocks)
    }
}

impl SavingsReport {
    /// The runs listed, newest first, with the cost column when a rate is set.
    fn runs_table(&self) -> ViewNode {
        ViewNode::Table {
            title: Some(if self.runs.len() < self.run_count {
                format!("The {} newest runs", self.runs.len())
            } else {
                "Runs".to_owned()
            }),
            columns: {
                let mut columns: Vec<String> = vec![
                    "run".into(),
                    "finished".into(),
                    "outcome".into(),
                    "reused".into(),
                    "built".into(),
                    "saved".into(),
                ];
                if self.cost.is_some() {
                    columns.push("cost avoided".into());
                }
                columns
            },
            rows: self
                .runs
                .iter()
                .map(|r| {
                    let mut row = vec![
                        vec![Span::toned(r.run_id.as_str(), Tone::Code)],
                        vec![Span::plain(r.finished_at.to_string())],
                        vec![outcome(r.outcome)],
                        vec![Span::plain(r.savings.reused.to_string())],
                        vec![Span::plain(r.savings.built.to_string())],
                        vec![Span::plain(saved(&r.savings))],
                    ];
                    if let (Some(cost), Some(amount)) = (&self.cost, r.cost) {
                        row.push(vec![Span::plain(if r.savings.timed == 0 {
                            super::run_stats::MISSING.to_owned()
                        } else if r.savings.is_lower_bound() {
                            format!("at least {}", money(amount, &cost.unit))
                        } else {
                            money(amount, &cost.unit)
                        })]);
                    }
                    row
                })
                .collect(),
            breaks: Vec::new(),
            footer: None,
        }
    }
}
