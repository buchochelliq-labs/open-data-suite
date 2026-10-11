//! `ods plugin test <name>` (ADR-0031 §5): runs the SDK's conformance suites against a
//! `[plugins.<name>]`, as a subprocess, for each contract it describes. A plugin author
//! runs it before publishing; a user, before trusting one in CI.
//!
//! - **`health_check`:** each check it lists, over a scope of fixture nodes.
//! - **`change_provider`:** over the sources given with `--source` (at least two it can
//!   read; a plugin's warehouse is its own, so ODS can't pick them), and
//!   `--unreadable` for one it can't. Changing data isn't something a test of a
//!   read-only plugin can do, so that case is always skipped.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use clap::{Arg, ArgAction, ArgMatches, Command};
use ods_config::Loaded;
use ods_provider_process::PluginProcess;
use ods_sdk::conformance::{Report, changes, health_check};
use ods_sdk::contracts::changes::{CHANGE_PROVIDER, ChangeProvider, RequestedSource};
use ods_sdk::contracts::health_check::{CheckScope, HEALTH_CHECK, HealthCheck, NodeFacts};
use serde::Serialize;

use super::plugin_process::{Declared, declared, trusted};
use super::state_plan::block_on;
use crate::exit::{CliError, ExitStatus, codes};
use crate::present::{Level, Present, Span, Tone, ViewNode};

/// `ods plugin test`.
pub(super) fn command() -> Command {
    Command::new("test")
        .about("Run the SDK's conformance suites against a declared out-of-process plugin")
        .long_about(
            "Starts the `[plugins.<NAME>]` plugin, asks it to describe itself, and runs the \
             SDK's conformance suite for each contract it implements, as ODS would call \
             it. Its health checks are asked about fixture nodes. Its source versions are \
             asked about the sources given with --source (two it can read), and \
             --unreadable (one it can't). A plugin a project declares runs only once \
             trusted (`ods health trust`), or with --allow-scripts.",
        )
        .arg(
            Arg::new("name")
                .required(true)
                .value_name("NAME")
                .help("The plugin, as `[plugins.<NAME>]` names it"),
        )
        .arg(
            Arg::new("source")
                .long("source")
                .value_name("ID")
                .action(ArgAction::Append)
                .help("A source it can read, by dbt id (e.g. source.shop.raw.orders); give two"),
        )
        .arg(
            Arg::new("unreadable")
                .long("unreadable")
                .value_name("ID")
                .help("A source that exists but whose version it can't read"),
        )
        .arg(
            Arg::new("allow-scripts")
                .long("allow-scripts")
                .action(ArgAction::SetTrue)
                .help("Run the plugin even when the project's declaration isn't trusted, for this run only"),
        )
}

/// One suite, against one of the plugin's providers.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct SuiteResult {
    /// The contract, e.g. `health_check`.
    contract: &'static str,
    /// The check, for `health_check`.
    #[serde(skip_serializing_if = "Option::is_none")]
    check: Option<String>,
    passed: Vec<&'static str>,
    skipped: Vec<Skipped>,
    /// The case that failed, and how; the cases after it didn't run.
    #[serde(skip_serializing_if = "Option::is_none")]
    failed: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct Skipped {
    case: String,
    why: String,
}

/// `ods plugin test`'s report.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct TestReport {
    plugin: String,
    command: Vec<String>,
    /// The name and version it describes itself with.
    described_as: String,
    /// The contracts it names that this `ods` doesn't know: not tested.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unknown_contracts: Vec<String>,
    suites: Vec<SuiteResult>,
}

impl TestReport {
    pub(super) fn build(args: &ArgMatches, config: &Loaded) -> Result<Self, CliError> {
        let name = args
            .get_one::<String>("name")
            .map(String::as_str)
            .unwrap_or_default();
        let all = declared(config)?;
        let Some(plugin) = all.iter().find(|p| p.name == name).cloned() else {
            return Err(CliError::new(
                ExitStatus::Usage,
                codes::PLUGIN_UNKNOWN,
                format!("no `[plugins.{name}]` is declared"),
            )
            .with_hint(if all.is_empty() {
                "the configuration declares no out-of-process plugins".to_owned()
            } else {
                format!(
                    "the declared plugins are: {}",
                    all.iter()
                        .map(|p| format!("`{}`", p.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }));
        };
        let allow = args.get_flag("allow-scripts");
        if !trusted(config, std::slice::from_ref(&plugin), allow).contains(name) {
            return Err(super::plugin_process::Held {
                name: name.to_owned(),
                why: "the project declares it, and it isn't trusted as it is now".to_owned(),
            }
            .error());
        }
        let sources: Vec<RequestedSource> = args
            .get_many::<String>("source")
            .into_iter()
            .flatten()
            .map(|id| requested(id))
            .collect();
        let unreadable = args.get_one::<String>("unreadable").map(|id| requested(id));
        block_on(run(plugin, sources, unreadable))?
    }

    /// The error to exit with when a case failed (exit 5).
    pub(super) fn failure(&self) -> Option<CliError> {
        let failed: Vec<String> = self
            .suites
            .iter()
            .filter_map(|s| {
                s.failed.as_ref().map(|f| match &s.check {
                    Some(check) => format!("{} ({check}): {f}", s.contract),
                    None => format!("{}: {f}", s.contract),
                })
            })
            .collect();
        (!failed.is_empty()).then(|| {
            CliError::new(
                ExitStatus::CheckFailed,
                codes::PLUGIN_TEST_FAILED,
                format!(
                    "the plugin `{}` fails the SDK's suites: {}",
                    self.plugin,
                    failed.join("; ")
                ),
            )
        })
    }
}

/// A source asked about by its dbt id, named by what follows `source.<package>.`.
fn requested(id: &str) -> RequestedSource {
    let name = id.splitn(3, '.').nth(2).unwrap_or(id);
    RequestedSource::new(id, name)
}

async fn run(
    plugin: Declared,
    sources: Vec<RequestedSource>,
    unreadable: Option<RequestedSource>,
) -> Result<TestReport, CliError> {
    let process = Arc::new(PluginProcess::new(
        &plugin.name,
        plugin.command.clone(),
        &plugin.root,
        plugin.timeout,
    ));
    let described = process.describe().await.map_err(|e| {
        super::plugin_process::Held {
            name: plugin.name.clone(),
            why: e.to_string(),
        }
        .error()
    })?;
    let mut suites = Vec::new();
    if described.provides(&HEALTH_CHECK) {
        for check in process.checks(&described, &BTreeMap::new()) {
            let id = check.describe().id;
            let harness = Checks(Arc::new(check));
            let outcome = caught(async move { health_check::run(&harness).await }).await;
            suites.push(result(HEALTH_CHECK.name, Some(id), outcome));
        }
    }
    if described.provides(&CHANGE_PROVIDER) {
        let outcome = if sources.len() < 2 {
            Ok(Report {
                passed: Vec::new(),
                skipped: vec![(
                    "every case",
                    "give two sources it can read with --source".to_owned(),
                )],
            })
        } else {
            let harness = Versions {
                process: process.clone(),
                described: described.clone(),
                sources,
                unreadable,
            };
            caught(async move { changes::run(&harness).await }).await
        };
        suites.push(result(CHANGE_PROVIDER.name, None, outcome));
    }
    Ok(TestReport {
        plugin: plugin.name,
        command: plugin.command,
        described_as: format!("{} {}", described.answer.name, described.answer.version)
            .trim()
            .to_owned(),
        unknown_contracts: described.unknown,
        suites,
    })
}

fn result(
    contract: &'static str,
    check: Option<String>,
    outcome: Result<Report, String>,
) -> SuiteResult {
    match outcome {
        Ok(report) => SuiteResult {
            contract,
            check,
            passed: report.passed,
            skipped: report
                .skipped
                .into_iter()
                .map(|(case, why)| Skipped {
                    case: case.to_owned(),
                    why,
                })
                .collect(),
            failed: None,
        },
        Err(failed) => SuiteResult {
            contract,
            check,
            passed: Vec::new(),
            skipped: Vec::new(),
            failed: Some(failed),
        },
    }
}

/// Runs a suite, whose failing case panics with its name, as a task: its report, or the
/// panic's message. The panic isn't printed: the report says it.
async fn caught(
    suite: impl std::future::Future<Output = Report> + Send + 'static,
) -> Result<Report, String> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = tokio::spawn(suite).await;
    std::panic::set_hook(hook);
    outcome.map_err(|e| match e.try_into_panic() {
        Ok(payload) => payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "a case failed".to_owned()),
        Err(e) => e.to_string(),
    })
}

/// A plugin check under the `health_check` suite, asked about fixture nodes.
struct Checks(Arc<dyn HealthCheck>);

impl health_check::HealthCheckHarness for Checks {
    fn check(&self) -> Arc<dyn HealthCheck> {
        self.0.clone()
    }

    fn scope(&self) -> CheckScope {
        CheckScope::new(
            vec![
                NodeFacts::new("model.ods_conformance.orders", "orders", "model"),
                NodeFacts::new("model.ods_conformance.customers", "customers", "model"),
                NodeFacts::new("seed.ods_conformance.countries", "countries", "seed"),
            ],
            None,
        )
    }

    fn undecidable(&self) -> Option<CheckScope> {
        None
    }
}

/// A plugin's source versions under the `change_provider` suite.
struct Versions {
    process: Arc<PluginProcess>,
    described: ods_provider_process::Described,
    sources: Vec<RequestedSource>,
    unreadable: Option<RequestedSource>,
}

#[async_trait]
impl changes::ChangeHarness for Versions {
    async fn provider(&self) -> Arc<dyn ChangeProvider> {
        // The settings are the plugin's own business here: none are given.
        match self.process.versions(&self.described, BTreeMap::new()) {
            Some(v) => Arc::new(v),
            None => unreachable!("only run when it implements change_provider"),
        }
    }

    fn readable(&self) -> Vec<RequestedSource> {
        self.sources.clone()
    }

    async fn commit(&self, _: &RequestedSource) -> bool {
        false
    }

    fn unreadable(&self) -> Option<RequestedSource> {
        self.unreadable.clone()
    }
}

impl Present for TestReport {
    const COMMAND: &'static str = "plugin.test";

    fn view(&self) -> ViewNode {
        let mut blocks = vec![ViewNode::KeyValue(vec![
            (
                "plugin".into(),
                vec![Span::toned(self.plugin.as_str(), Tone::Code)],
            ),
            (
                "command".into(),
                vec![Span::toned(self.command.join(" "), Tone::Code)],
            ),
            (
                "describes itself as".into(),
                vec![Span::plain(self.described_as.as_str())],
            ),
        ])];
        let mut rows = Vec::new();
        for suite in &self.suites {
            let what = match &suite.check {
                Some(check) => format!("{} ({check})", suite.contract),
                None => suite.contract.to_owned(),
            };
            for case in &suite.passed {
                rows.push(vec![
                    vec![Span::toned(what.as_str(), Tone::Code)],
                    vec![Span::plain(*case)],
                    vec![Span::toned("passed", Tone::Success)],
                ]);
            }
            for skipped in &suite.skipped {
                rows.push(vec![
                    vec![Span::toned(what.as_str(), Tone::Code)],
                    vec![Span::plain(skipped.case.as_str())],
                    vec![Span::toned(
                        format!("skipped: {}", skipped.why),
                        Tone::Muted,
                    )],
                ]);
            }
            if let Some(failed) = &suite.failed {
                rows.push(vec![
                    vec![Span::toned(what.as_str(), Tone::Code)],
                    vec![Span::plain("")],
                    vec![Span::toned(format!("failed: {failed}"), Tone::Error)],
                ]);
            }
        }
        if rows.is_empty() {
            blocks.push(ViewNode::Notice {
                level: Level::Warning,
                message: vec![Span::plain("it implements no contract this ods can test")],
            });
        } else {
            blocks.push(ViewNode::Table {
                title: Some("Conformance".to_owned()),
                columns: vec!["contract".into(), "case".into(), "result".into()],
                rows,
                breaks: Vec::new(),
                footer: None,
            });
        }
        if !self.unknown_contracts.is_empty() {
            blocks.push(ViewNode::Notice {
                level: Level::Info,
                message: vec![Span::plain(format!(
                    "not tested, as this ods doesn't know them: {}",
                    self.unknown_contracts.join(", ")
                ))],
            });
        }
        ViewNode::Group(blocks)
    }
}
