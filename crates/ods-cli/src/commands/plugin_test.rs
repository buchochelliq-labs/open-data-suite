//! `ods plugin test <name>` (ADR-0031 §5): runs the SDK's conformance suites against a
//! `[plugins.<name>]`, as a subprocess, for each contract it describes. A plugin author
//! runs it before publishing; a user, before trusting one in CI.
//!
//! - **`health_check`:** each check it lists, over a scope of fixture nodes.
//! - **`change_provider`:** over the sources given with `--source` (at least two it can
//!   read; a plugin's warehouse is its own, so ODS can't pick them), and
//!   `--unreadable` for one it can't, with its warehouse's settings, as a run sends
//!   them. Changing data isn't something a test of a
//!   read-only plugin can do, so that case is always skipped.
//! - **`relation_linker`:** over three-part relation names quoted with `--quote`, asked
//!   in one call as a run asks, with and without its warehouse's settings.
//! - **`error_catalogue`:** over the samples in `--errors` (a JSON list of
//!   `{"message", "expected"}`, `expected` a symptom or null), each classified by the
//!   plugin as it is asked.
//! - **`observed_lineage_source`:** over the export `--export` (and `--malformed`, one
//!   it must refuse).
//! - **`relation_probe`, `relation_privileges`:** not here: they need relations in a
//!   live warehouse. `ods health check` with `[health.probes] plugin` runs them.

use std::collections::BTreeMap;
use std::path::PathBuf;
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
            Arg::new("quote")
                .long("quote")
                .value_name("CHAR")
                .default_value("\"")
                .help("How its warehouse quotes identifiers, for the link suite's relations"),
        )
        .arg(
            Arg::new("errors")
                .long("errors")
                .value_name("FILE")
                .help("Error samples for its catalogue: a JSON list of {\"message\", \"expected\"}"),
        )
        .arg(
            Arg::new("export")
                .long("export")
                .value_name("FILE")
                .help("An export of its warehouse's lineage, for its observed lineage"),
        )
        .arg(
            Arg::new("malformed")
                .long("malformed")
                .value_name("FILE")
                .help("A file it must refuse to read as an export"),
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
        let inputs = WarehouseInputs {
            quote: args
                .get_one::<String>("quote")
                .and_then(|q| q.chars().next())
                .unwrap_or('"'),
            errors: args.get_one::<String>("errors").map(PathBuf::from),
            export: args.get_one::<String>("export").map(PathBuf::from),
            malformed: args.get_one::<String>("malformed").map(PathBuf::from),
        };
        block_on(run(config, plugin, sources, unreadable, inputs))?
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
    config: &Loaded,
    plugin: Declared,
    sources: Vec<RequestedSource>,
    unreadable: Option<RequestedSource>,
    inputs: WarehouseInputs,
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
            // What a run sends it: its warehouse's settings, as configured.
            let settings = described
                .answer
                .warehouse
                .as_deref()
                .map(|w| super::plugin_process::warehouse_settings(config, w))
                .unwrap_or_default();
            let harness = Versions {
                process: process.clone(),
                described: described.clone(),
                settings,
                sources,
                unreadable,
            };
            caught(async move { changes::run(&harness).await }).await
        };
        suites.push(result(CHANGE_PROVIDER.name, None, outcome));
    }
    let settings = described
        .answer
        .warehouse
        .as_deref()
        .map(|w| super::plugin_process::warehouse_settings(config, w))
        .unwrap_or_default();
    suites.extend(warehouse_suites(&process, &described, &settings, inputs).await);
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
    settings: BTreeMap<String, serde_json::Value>,
    sources: Vec<RequestedSource>,
    unreadable: Option<RequestedSource>,
}

#[async_trait]
impl changes::ChangeHarness for Versions {
    async fn provider(&self) -> Arc<dyn ChangeProvider> {
        match self
            .process
            .versions(&self.described, self.settings.clone())
        {
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

/// What the warehouse suites are run over, from the command line.
struct WarehouseInputs {
    quote: char,
    errors: Option<PathBuf>,
    export: Option<PathBuf>,
    malformed: Option<PathBuf>,
}

/// Runs `future` to its end from synchronous code, on a thread of its own with its own
/// runtime: the link, catalogue and lineage contracts are synchronous, and the suites
/// run them inside a runtime.
fn blocking<T: Send>(future: impl std::future::Future<Output = T> + Send) -> T {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(future),
                    Err(e) => panic!("no runtime to call the plugin with: {e}"),
                }
            })
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// The suites of the warehouse contracts `described` implements (ADR-0031 §3d), with
/// `settings` as a run sends them.
async fn warehouse_suites(
    process: &Arc<PluginProcess>,
    described: &ods_provider_process::Described,
    settings: &BTreeMap<String, serde_json::Value>,
    inputs: WarehouseInputs,
) -> Vec<SuiteResult> {
    use ods_sdk::contracts::error_catalogue::ERROR_CATALOGUE;
    use ods_sdk::contracts::observed_lineage::OBSERVED_LINEAGE_SOURCE;
    use ods_sdk::contracts::privileges::RELATION_PRIVILEGES;
    use ods_sdk::contracts::probe::RELATION_PROBE;
    use ods_sdk::contracts::relation_link::RELATION_LINKER;

    let skipped = |why: &str| {
        Ok(Report {
            passed: Vec::new(),
            skipped: vec![("every case", why.to_owned())],
        })
    };
    let mut suites = Vec::new();
    if described.provides(&RELATION_LINKER) {
        let harness = Linking::ask(process, settings, inputs.quote).await;
        let outcome = match harness {
            Ok(harness) => {
                caught(async move { ods_sdk::conformance::relation_link::run(&harness) }).await
            }
            Err(e) => Err(format!("its `link` call failed: {e}")),
        };
        suites.push(result(RELATION_LINKER.name, None, outcome));
    }
    if described.provides(&ERROR_CATALOGUE) {
        let outcome = match (&inputs.errors, &described.answer.catalogue) {
            (None, _) => skipped("give error samples with --errors"),
            (Some(_), None) => Err("it names no catalogue".to_owned()),
            (Some(file), Some(info)) => match samples(file) {
                Ok(samples) => {
                    let harness = Classifying {
                        catalogue: LiveCatalogue {
                            process: process.clone(),
                            info: info.clone(),
                        },
                        samples,
                    };
                    caught(async move { ods_sdk::conformance::error_catalogue::run(&harness) })
                        .await
                }
                Err(e) => Err(e),
            },
        };
        suites.push(result(ERROR_CATALOGUE.name, None, outcome));
    }
    if described.provides(&OBSERVED_LINEAGE_SOURCE) {
        let outcome = match &inputs.export {
            None => skipped("give an export of its warehouse's lineage with --export"),
            Some(export) => {
                let harness = Reading {
                    source: Arc::new(LiveLineage::new(process, settings, export)),
                    malformed: inputs.malformed.as_ref().map(|m| {
                        Arc::new(LiveLineage::new(process, settings, m))
                            as Arc<dyn ods_sdk::contracts::observed_lineage::ObservedLineageSource>
                    }),
                };
                caught(async move { ods_sdk::conformance::observed_lineage::run(&harness) }).await
            }
        };
        suites.push(result(OBSERVED_LINEAGE_SOURCE.name, None, outcome));
    }
    for contract in [RELATION_PROBE, RELATION_PRIVILEGES] {
        if described.provides(&contract) {
            suites.push(result(
                contract.name,
                None,
                skipped("it needs relations in a live warehouse: `ods health check` with `[health.probes] plugin` runs it"),
            ));
        }
    }
    suites
}

/// The link suite's relations, quoted with `quote`: three parts, as most warehouses
/// name a table.
struct Linking {
    quote: char,
    linker: Arc<dyn ods_sdk::contracts::relation_link::RelationLinker>,
    unconfigured: Arc<dyn ods_sdk::contracts::relation_link::RelationLinker>,
}

impl Linking {
    fn relation(&self, parts: &[&str]) -> String {
        let q = self.quote;
        parts
            .iter()
            .map(|p| format!("{q}{p}{q}"))
            .collect::<Vec<_>>()
            .join(".")
    }

    fn every(quote: char) -> Vec<String> {
        use ods_sdk::conformance::relation_link::RelationLinkHarness as _;
        let probe = Self {
            quote,
            linker: Arc::new(ods_provider_process::AnsweredLinker::new(
                "",
                BTreeMap::new(),
            )),
            unconfigured: Arc::new(ods_provider_process::AnsweredLinker::new(
                "",
                BTreeMap::new(),
            )),
        };
        let mut all = vec![probe.qualified(), probe.under_qualified(), probe.awkward()];
        all.extend(probe.malformed());
        all
    }

    /// Asks the plugin, in one call each, for the links of every relation the suite
    /// uses, with and without its settings, as a run asks.
    async fn ask(
        process: &Arc<PluginProcess>,
        settings: &BTreeMap<String, serde_json::Value>,
        quote: char,
    ) -> Result<Self, ods_sdk::ProviderError> {
        let relations = Self::every(quote);
        let linker = process.links(&relations, settings.clone()).await?;
        let unconfigured = process.links(&relations, BTreeMap::new()).await?;
        Ok(Self {
            quote,
            linker: Arc::new(ods_provider_process::AnsweredLinker::new(
                process.name(),
                linker,
            )),
            unconfigured: Arc::new(ods_provider_process::AnsweredLinker::new(
                process.name(),
                unconfigured,
            )),
        })
    }
}

impl ods_sdk::conformance::relation_link::RelationLinkHarness for Linking {
    fn linker(&self) -> Arc<dyn ods_sdk::contracts::relation_link::RelationLinker> {
        self.linker.clone()
    }
    fn unconfigured(&self) -> Option<Arc<dyn ods_sdk::contracts::relation_link::RelationLinker>> {
        Some(self.unconfigured.clone())
    }
    fn qualified(&self) -> String {
        self.relation(&["ods_conformance", "main", "orders"])
    }
    fn under_qualified(&self) -> String {
        self.relation(&["main", "orders"])
    }
    fn awkward(&self) -> String {
        self.relation(&["ods conformance", "ma?in", "or#d/e%rs"])
    }
    fn malformed(&self) -> Vec<String> {
        vec![
            self.relation(&["ods_conformance", "main", "."]),
            self.relation(&["ods_conformance", "main", ".."]),
            self.relation(&["ods_conformance", "main", ""]),
            format!("{}x", self.relation(&["ods_conformance", "main", "orders"])),
            // Whitespace inside an unquoted name.
            "ods_conformance.main.ord ers".to_owned(),
        ]
    }
}

/// An error sample, as `--errors` gives it.
#[derive(serde::Deserialize)]
struct SampleFields {
    message: String,
    #[serde(default)]
    expected: Option<ods_core::failure::Symptom>,
    #[serde(default)]
    sentinel: Option<String>,
}

/// The samples in `file`.
fn samples(
    file: &std::path::Path,
) -> Result<Vec<ods_sdk::conformance::error_catalogue::Sample>, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("{} can't be read: {e}", file.display()))?;
    let fields: Vec<SampleFields> = serde_json::from_str(&text)
        .map_err(|e| format!("{} isn't a JSON list of samples: {e}", file.display()))?;
    fields
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            let summary = ods_sdk::contracts::run_events::ErrorSummary::from_message(&f.message)
                .ok_or_else(|| format!("sample {i} has no message"))?;
            let mut sample = ods_sdk::conformance::error_catalogue::Sample {
                // A suite names its cases by `&'static str`; this command runs once.
                name: Box::leak(format!("--errors sample {i}").into_boxed_str()),
                summary,
                expected: f.expected,
                sentinel: None,
            };
            sample.sentinel = f.sentinel.map(|s| &*Box::leak(s.into_boxed_str()));
            Ok(sample)
        })
        .collect()
}

/// A plugin's catalogue, asked about each error as the suite classifies it.
struct LiveCatalogue {
    process: Arc<PluginProcess>,
    info: ods_sdk::contracts::error_catalogue::CatalogueInfo,
}

impl ods_sdk::Provider for LiveCatalogue {
    fn info(&self) -> ods_sdk::ProviderInfo {
        ods_sdk::ProviderInfo::new(
            ods_provider_process::KIND,
            self.process.name(),
            env!("CARGO_PKG_VERSION"),
            ods_core::CapabilitySet::from([ods_core::Capability::ErrorExplain]),
        )
    }
}

impl ods_sdk::contracts::error_catalogue::ErrorCatalogue for LiveCatalogue {
    fn catalogue(&self) -> ods_sdk::contracts::error_catalogue::CatalogueInfo {
        self.info.clone()
    }

    fn classify(
        &self,
        error: &ods_sdk::contracts::run_events::ErrorSummary,
    ) -> ods_sdk::contracts::error_catalogue::Classification {
        let asked = std::slice::from_ref(error);
        match blocking(self.process.classify(asked)) {
            Ok(mut classifications) => classifications.remove(0),
            Err(e) => panic!("its `classify` call failed: {e}"),
        }
    }
}

struct Classifying {
    catalogue: LiveCatalogue,
    samples: Vec<ods_sdk::conformance::error_catalogue::Sample>,
}

impl ods_sdk::conformance::error_catalogue::ErrorCatalogueHarness for Classifying {
    fn catalogue(&self) -> &dyn ods_sdk::contracts::error_catalogue::ErrorCatalogue {
        &self.catalogue
    }
    fn samples(&self) -> Vec<ods_sdk::conformance::error_catalogue::Sample> {
        self.samples.clone()
    }
}

/// A plugin reading an export, asked each time the suite reads it.
struct LiveLineage {
    process: Arc<PluginProcess>,
    settings: BTreeMap<String, serde_json::Value>,
    export: String,
}

impl LiveLineage {
    fn new(
        process: &Arc<PluginProcess>,
        settings: &BTreeMap<String, serde_json::Value>,
        export: &std::path::Path,
    ) -> Self {
        let export = std::path::absolute(export).unwrap_or_else(|_| export.to_owned());
        Self {
            process: process.clone(),
            settings: settings.clone(),
            export: export.display().to_string(),
        }
    }
}

impl ods_sdk::Provider for LiveLineage {
    fn info(&self) -> ods_sdk::ProviderInfo {
        ods_sdk::ProviderInfo::new(
            ods_provider_process::KIND,
            self.process.name(),
            env!("CARGO_PKG_VERSION"),
            ods_core::CapabilitySet::new(),
        )
    }
}

impl ods_sdk::contracts::observed_lineage::ObservedLineageSource for LiveLineage {
    fn observed_lineage(
        &self,
    ) -> Result<ods_sdk::contracts::observed_lineage::ObservedLineage, ods_sdk::ProviderError> {
        blocking(
            self.process
                .observed_lineage(&self.export, self.settings.clone()),
        )
    }
}

struct Reading {
    source: Arc<dyn ods_sdk::contracts::observed_lineage::ObservedLineageSource>,
    malformed: Option<Arc<dyn ods_sdk::contracts::observed_lineage::ObservedLineageSource>>,
}

impl ods_sdk::conformance::observed_lineage::ObservedLineageHarness for Reading {
    fn source(&self) -> Arc<dyn ods_sdk::contracts::observed_lineage::ObservedLineageSource> {
        self.source.clone()
    }
    fn malformed(
        &self,
    ) -> Option<Arc<dyn ods_sdk::contracts::observed_lineage::ObservedLineageSource>> {
        self.malformed.clone()
    }
}
