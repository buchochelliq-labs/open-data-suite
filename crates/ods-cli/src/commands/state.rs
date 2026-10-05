//! `ods state`: the M1 State MVP, in progress.
//! - `ods state policies` shows the freshness policies read from the project (#168);
//! - `ods state plan`, `record` and `history` plan against, record and list the state
//!   (ADR-0013, `state_plan`);
//! - `ods state run` plans, builds exactly what needs building with dbt, and records
//!   the successes (ADR-0014, `state_run`);
//! - the other subcommands (`explain`, …) report "not implemented".

use std::path::PathBuf;

use clap::{Arg, ArgMatches, Command};
use ods_core::freshness::format_duration;
use ods_core::{FreshnessPolicy, LoadedAt, PolicyOrigin, Quorum, UnappliedSetting};
use ods_provider_dbt::state_config::resolve;
use ods_provider_dbt::{ArtifactPreference, Artifacts};
use serde::Serialize;

use super::{
    Planned, state_doctor, state_explain, state_export, state_failure, state_plan, state_retry,
    state_run, state_test,
};
use crate::exit::{CliError, ExitStatus, codes};
use crate::module::{Context, Module};
use crate::present::{Level, Present, Span, Tone, ViewNode};

/// Captures the arguments of subcommands that don't exist yet (see [`Planned`]).
const PASSTHROUGH: &str = "args";

const ABOUT: &str = "Plan and run only what needs to run, with explanations";
const MILESTONE: &str = "M1 State MVP (v0.0.1)";

/// `ods state`.
pub struct State;

impl Module for State {
    fn command(&self) -> Command {
        Command::new("state")
            .about(format!(
                "{ABOUT} [preview: `run`, `plan`, `record`, `history`, `policies`, `doctor`, `retry`, `explain`, `explain-failure`, `diff`, `graph`, `export`; more in {MILESTONE}]"
            ))
            .args_conflicts_with_subcommands(true)
            .subcommand(
                Command::new("policies")
                    .about("Show the freshness policy of every model and how sources report new data, as read from dbt State configs")
                    .arg(
                        Arg::new("target-dir")
                            .long("target-dir")
                            .value_name("DIR")
                            .env("DBT_TARGET_PATH")
                            .hide_env_values(true)
                            .help("dbt target directory with manifest.json or dbt v2's Information Schema [default: <project-dir>/target]"),
                    )
                    .arg(
                        Arg::new("project-dir")
                            .long("project-dir")
                            .value_name("DIR")
                            .env("DBT_PROJECT_DIR")
                            .hide_env_values(true)
                            .help("The dbt project, whose target directory ODS reads [default: .]"),
                    )
                    .arg(
                        Arg::new("artifacts")
                            .long("artifacts")
                            .value_name("FORMAT")
                            .value_parser(["auto", "json", "info-schema"])
                            .default_value("auto")
                            .help("dbt artifacts to read"),
                    )
                    .arg(
                        Arg::new("model")
                            .long("model")
                            .value_name("MODEL")
                            .help("Only this model (name or unique_id)"),
                    ),
            )
            .subcommands(state_run::Kind::ALL.map(state_run::build_command))
            .subcommand(state_test::test_command())
            .subcommand(state_plan::plan_command())
            .subcommand(state_plan::record_command())
            .subcommand(state_plan::history_command())
            .subcommand(state_retry::retry_command())
            .subcommands(state_explain::Question::ALL.map(state_explain::question_command))
            .subcommand(state_explain::diff_command())
            .subcommand(state_explain::graph_command())
            .subcommand(state_failure::explain_failure_command())
            .subcommand(state_doctor::doctor_command())
            .subcommand(state_doctor::backup_command())
            .subcommand(state_doctor::reset_command())
            .subcommand(state_export::export_command())
            .arg(
                Arg::new(PASSTHROUGH)
                    .num_args(0..)
                    .trailing_var_arg(true)
                    .allow_hyphen_values(true)
                    .hide(true),
            )
    }

    fn passthrough_arg(&self) -> Option<&'static str> {
        Some(PASSTHROUGH)
    }

    fn run(&self, matches: &ArgMatches, ctx: &mut Context<'_>) -> Result<(), CliError> {
        if let Some((name, args)) = matches.subcommand()
            && let Some(kind) = state_run::Kind::named(name)
        {
            return state_run::RunReport::run(kind, args, ctx);
        }
        if let Some((name, args)) = matches.subcommand()
            && let Some(question) = state_explain::Question::named(name)
        {
            return ctx.emit(&state_explain::ExplainReport::build(
                question, args, ctx.config,
            )?);
        }
        match matches.subcommand() {
            Some(("policies", args)) => ctx.emit(&PoliciesReport::build(args, ctx.config)?),
            Some(("test", args)) => state_test::TestReport::run(args, ctx),
            Some(("plan", args)) => ctx.emit(&state_plan::PlanReport::build(args, ctx.config)?),
            Some(("record", args)) => ctx.emit(&state_plan::RecordReport::build(args, ctx.config)?),
            Some(("history", args)) if args.get_one::<String>("run").is_some() => {
                ctx.emit(&state_plan::RunHistoryReport::build(args, ctx.config)?)
            }
            Some(("history", args)) if args.get_one::<String>("node").is_some() => {
                ctx.emit(&state_explain::NodeHistoryReport::build(args, ctx.config)?)
            }
            Some(("history", args)) => {
                ctx.emit(&state_plan::HistoryReport::build(args, ctx.config)?)
            }
            Some(("explain-failure", args)) => ctx.emit(
                &state_failure::ExplainFailureReport::build(args, ctx.config)?,
            ),
            Some(("diff", args)) => ctx.emit(&state_explain::DiffReport::build(args, ctx.config)?),
            Some(("graph", args)) => {
                let report = state_explain::GraphReport::build(args, ctx.config)?;
                // The graph is for other tools: written as it is, unless JSON is asked for.
                if ctx.output.mode == crate::output::Mode::Json {
                    ctx.emit(&report)
                } else {
                    ctx.raw_out().write_all(report.text().as_bytes())?;
                    Ok(())
                }
            }
            Some(("retry", args)) => {
                let (last, line, failed) = state_retry::last_run(args, ctx.config)?;
                let matches = self.command().try_get_matches_from(&line).map_err(|e| {
                    CliError::new(
                        ExitStatus::Usage,
                        codes::STATE_INPUT,
                        format!(
                            "the last run, `{}`, no longer parses: {}",
                            last.shown(),
                            e.kind()
                        ),
                    )
                    .with_hint("run the command you want yourself; retry will then use it")
                })?;
                state_run::Steps::new(ctx.progress, 0).note(&format!(
                    "retrying {}`{}`",
                    if failed.is_some() {
                        "what failed in "
                    } else {
                        ""
                    },
                    last.shown()
                ));
                // `--failed` refuses a last `test`, so the command is one that builds.
                let kind = matches
                    .subcommand()
                    .and_then(|(name, sub)| Some((state_run::Kind::named(name)?, sub)));
                match (failed, kind) {
                    (Some(failed), Some((kind, sub))) => {
                        state_run::RunReport::run_with(kind, sub, ctx, Some(&failed))
                    }
                    (Some(_), None) => Err(CliError::new(
                        ExitStatus::Failure,
                        codes::INTERNAL,
                        format!("`{}` can't be retried with --failed", last.shown()),
                    )),
                    (None, _) => self.run(&matches, ctx),
                }
            }
            Some(("doctor", args)) => state_doctor::DoctorReport::run(args, ctx),
            Some(("backup", args)) => {
                ctx.emit(&state_doctor::BackupReport::build(args, ctx.config)?)
            }
            Some(("reset", args)) => ctx.emit(&state_doctor::ResetReport::build(args, ctx.config)?),
            Some(("export", args)) => state_export::ExportReport::run(args, ctx),
            _ => Planned::new("state", ABOUT, MILESTONE).run(matches, ctx),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct PoliciesReport {
    target_dir: PathBuf,
    uses_dbt_state: bool,
    models: Vec<ModelPolicy>,
    sources: Vec<SourceFreshness>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ModelPolicy {
    unique_id: String,
    lag_tolerance: String,
    #[serde(flatten)]
    policy: FreshnessPolicy,
    reuse_allowed: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct SourceFreshness {
    unique_id: String,
    loaded_at: LoadedAt,
}

impl PoliciesReport {
    fn build(args: &ArgMatches, config: &ods_config::Loaded) -> Result<Self, CliError> {
        let target_dir = super::state_settings::StateSettings::resolve(args, config)?.target_dir();
        let preference = match args.get_one::<String>("artifacts").map(String::as_str) {
            Some("json") => ArtifactPreference::Json,
            Some("info-schema") => ArtifactPreference::InfoSchema,
            _ => ArtifactPreference::Auto,
        };
        let artifacts = Artifacts::load_with(&target_dir, preference).map_err(|e| {
            CliError::new(ExitStatus::Failure, codes::LINEAGE_ARTIFACTS, e.to_string())
                .with_hint("run `dbt parse` or `dbt compile` first")
        })?;
        let resolved = resolve(&artifacts.manifest);
        let wanted = args.get_one::<String>("model");
        let matches =
            |id: &str| wanted.is_none_or(|w| id == w || id.rsplit('.').next() == Some(w.as_str()));
        let models: Vec<ModelPolicy> = resolved
            .nodes
            .into_iter()
            .filter(|(id, _)| matches(id))
            .map(|(unique_id, policy)| ModelPolicy {
                unique_id,
                lag_tolerance: format_duration(policy.lag_tolerance_secs),
                reuse_allowed: policy.allows_reuse(),
                policy,
            })
            .collect();
        if let Some(model) = wanted
            && models.is_empty()
        {
            return Err(CliError::new(
                ExitStatus::Usage,
                codes::LINEAGE_TARGET,
                format!("no model or snapshot is called `{model}`"),
            ));
        }
        Ok(Self {
            target_dir,
            uses_dbt_state: resolved.uses_dbt_state,
            models,
            sources: if wanted.is_some() {
                Vec::new()
            } else {
                resolved
                    .sources
                    .into_iter()
                    .map(|(unique_id, loaded_at)| SourceFreshness {
                        unique_id,
                        loaded_at,
                    })
                    .collect()
            },
        })
    }
}

fn origin_text(origin: &PolicyOrigin) -> String {
    match origin {
        PolicyOrigin::Configured { settings } => settings.join(", "),
        PolicyOrigin::FormatDefault { .. } => "dbt State default".to_owned(),
        PolicyOrigin::ConservativeDefault => "ODS default".to_owned(),
        other => format!("{other:?}"),
    }
}

fn unapplied_text(setting: &UnappliedSetting) -> String {
    format!("{}={}: {}", setting.setting, setting.value, setting.reason)
}

impl Present for PoliciesReport {
    const COMMAND: &'static str = "state.policies";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![
            ViewNode::Heading("Freshness policies".into()),
            ViewNode::KeyValue(vec![
                (
                    "artifacts".into(),
                    vec![Span::toned(
                        self.target_dir.display().to_string(),
                        Tone::Code,
                    )],
                ),
                (
                    "dbt State".into(),
                    vec![Span::plain(if self.uses_dbt_state {
                        "configured: unset settings use dbt State's defaults (45m, any)"
                    } else {
                        "not configured: rebuild on any new upstream data"
                    })],
                ),
            ]),
            ViewNode::Table {
                title: None,
                columns: vec![
                    "model".into(),
                    "lag tolerance".into(),
                    "fresh data from".into(),
                    "from".into(),
                    "reuse".into(),
                ],
                rows: self
                    .models
                    .iter()
                    .map(|m| {
                        vec![
                            vec![Span::toned(m.unique_id.as_str(), Tone::Code)],
                            vec![Span::plain(m.lag_tolerance.as_str())],
                            vec![Span::plain(match m.policy.require_fresh_data_from {
                                Quorum::All => "all parents",
                                _ => "any parent",
                            })],
                            vec![Span::plain(origin_text(&m.policy.origin))],
                            vec![if m.reuse_allowed {
                                Span::plain("allowed")
                            } else {
                                Span::toned("never", Tone::Warning)
                            }],
                        ]
                    })
                    .collect(),
            },
        ];
        for model in &self.models {
            for setting in &model.policy.unapplied {
                blocks.push(ViewNode::Notice {
                    level: if setting.blocks_reuse {
                        Level::Warning
                    } else {
                        Level::Info
                    },
                    message: vec![
                        Span::toned(model.unique_id.as_str(), Tone::Code),
                        Span::plain(format!(": {}", unapplied_text(setting))),
                    ],
                });
            }
            for setting in &model.policy.unknown {
                blocks.push(ViewNode::Notice {
                    level: Level::Warning,
                    message: vec![
                        Span::toned(model.unique_id.as_str(), Tone::Code),
                        Span::plain(format!(": unknown setting {setting}; never reused")),
                    ],
                });
            }
        }
        if !self.sources.is_empty() {
            blocks.push(ViewNode::Table {
                title: Some("How sources report new data".into()),
                columns: vec!["source".into(), "loaded at".into()],
                rows: self
                    .sources
                    .iter()
                    .map(|s| {
                        vec![
                            vec![Span::toned(s.unique_id.as_str(), Tone::Code)],
                            vec![Span::plain(match &s.loaded_at {
                                LoadedAt::Field(f) => format!("max({f})"),
                                LoadedAt::Query(q) => format!("query: {q}"),
                                LoadedAt::WarehouseMetadata => "warehouse metadata".to_owned(),
                                other => format!("{other:?}"),
                            })],
                        ]
                    })
                    .collect(),
            });
        }
        ViewNode::Group(blocks)
    }
}
