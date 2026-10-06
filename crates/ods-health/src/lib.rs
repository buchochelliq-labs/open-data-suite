//! The OpenDataSuite health engine (ADR-0030, #392): configurable checks that each give
//! a node a finding, and the badge rule that turns a node's findings into its health.
//!
//! Phase 1 holds the built-in checks, tuned by `[health]` in `ods.toml`: each can be
//! turned off, given a severity, and scoped with a selector. Later phases add
//! declarative, probe, script and plugin checks behind the same findings.
//!
//! Nothing here guesses (AGENTS rule 3): a check that can't decide says *unknown*, and
//! unknown never counts as healthy. Every finding names its check and why (rule 4).
//!
//! A module crate: it depends on `ods-core`, `ods-config` and `ods-sdk` (whose
//! `health_check` contract every check answers through), never on providers or other
//! modules (ADR-0001).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::sync::Arc;
use std::time::Duration;

use globset::{Glob, GlobSet, GlobSetBuilder};
use ods_config::{
    HealthCheckConfig, HealthConfig, HealthSelector, HealthSeverity, UnknownCountsAs,
};
pub use ods_sdk::contracts::health_check::{
    BuildFacts, CheckFinding, CheckInfo, CheckScope, HealthCheck, LastFailures, NodeFacts,
    Severity, Status,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------- findings

/// Where a check comes from (ADR-0030 §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CheckSource {
    /// One of ODS's own checks.
    Builtin,
    /// A check the project declares in `[[health.checks]]`.
    Declarative,
    /// A probe declared in `[[health.checks]]`: a read-only query on the warehouse.
    Probe,
    /// A check registered through the `health_check` contract: a plugin.
    Plugin,
}

impl CheckSource {
    /// Whether checks from it run in-process on facts any host has, so the dashboard
    /// works them out itself rather than reading them from a health record.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Builtin | Self::Declarative)
    }
}

/// One check's conclusion about one node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Finding {
    /// The check's id, e.g. `tests_required`.
    pub check: String,
    /// Where the check comes from (ADR-0030 §2).
    pub source: CheckSource,
    /// What it concluded.
    pub status: Status,
    /// How severe a failure is.
    pub severity: Severity,
    /// Why.
    pub reason: String,
    /// What it was concluded from, for machines (AGENTS rule 4), sorted by key: e.g. a
    /// build's `run_id` and `built_at`, or the last run's `last_run_command`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub evidence: BTreeMap<String, String>,
}

/// A node's health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Health {
    /// Every enabled check that applies passed.
    Healthy,
    /// A check at severity `warn` failed (or, if so configured, one couldn't decide).
    Warning,
    /// A check at severity `error` failed.
    Failing,
    /// A check couldn't decide, e.g. the node was never built; or no check applies.
    Unknown,
}

/// Every health, in the order they are listed.
pub const HEALTHS: [Health; 4] = [
    Health::Healthy,
    Health::Warning,
    Health::Failing,
    Health::Unknown,
];

impl Health {
    /// Its key, also its query value.
    pub fn key(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Warning => "warning",
            Self::Failing => "failing",
            Self::Unknown => "unknown",
        }
    }

    /// Its label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Healthy => "Healthy",
            Self::Warning => "Warning",
            Self::Failing => "Failing",
            Self::Unknown => "Unknown",
        }
    }

    /// What it means with the default checks, for people.
    pub fn how(self) -> &'static str {
        match self {
            Self::Healthy => {
                "Every enabled check passed: built, and its tests passed on that build as they are now (a seed needs none)."
            }
            Self::Warning => {
                "A check at severity warn failed: by default, no tests (models and snapshots), tests not recorded passing on this build or changed since, or skipped in the last run."
            }
            Self::Failing => {
                "A check at severity error failed: by default, it failed in the last run and hasn't been built since."
            }
            Self::Unknown => {
                "A check couldn't decide, e.g. ODS never built it: nothing to judge it on."
            }
        }
    }
}

/// A node's health, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct HealthBadge {
    /// The health.
    pub health: Health,
    /// Why, most important first.
    pub reasons: Vec<String>,
    /// Every finding the badge was worked out from, in check order.
    pub findings: Vec<Finding>,
}

// ---------------------------------------------------------------------------- checks

/// The built-in checks, in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Builtin {
    /// Was it ever built by ODS? Never built is unknown: nothing to judge it on.
    Built,
    /// Did it fail in the last run, and not get built since?
    LastRunFailed,
    /// Was it skipped in the last run because something upstream failed?
    LastRunSkipped,
    /// Has it any tests?
    TestsRequired,
    /// Did its tests pass on its current build, as they are now?
    TestsPassed,
}

/// Every built-in check, in order.
pub const BUILTINS: [Builtin; 5] = [
    Builtin::Built,
    Builtin::LastRunFailed,
    Builtin::LastRunSkipped,
    Builtin::TestsRequired,
    Builtin::TestsPassed,
];

impl Builtin {
    /// Its id, as `[health.builtin.<id>]` names it.
    pub fn id(self) -> &'static str {
        match self {
            Self::Built => "built",
            Self::LastRunFailed => "last_run_failed",
            Self::LastRunSkipped => "last_run_skipped",
            Self::TestsRequired => "tests_required",
            Self::TestsPassed => "tests_passed",
        }
    }

    /// Its severity unless configured.
    pub fn default_severity(self) -> Severity {
        match self {
            Self::LastRunFailed => Severity::Error,
            _ => Severity::Warn,
        }
    }

    /// The nodes it checks unless configured: tests are required of models and
    /// snapshots, every other check applies to every node.
    fn default_select(self) -> HealthSelector {
        let mut select = HealthSelector::default();
        if self == Self::TestsRequired {
            select.resource_type = vec!["model".to_owned(), "snapshot".to_owned()];
        }
        select
    }

    /// What it checks, for people.
    pub fn about(self) -> &'static str {
        match self {
            Self::Built => {
                "ODS built it: a node never built is unknown (it can't fail). Turned off, nodes are judged on the other checks alone."
            }
            Self::LastRunFailed => "It didn't fail in the last run, or was built since.",
            Self::LastRunSkipped => {
                "It wasn't skipped in the last run because of a failure upstream."
            }
            Self::TestsRequired => "It has at least one test (by default, models and snapshots).",
            Self::TestsPassed => "Its tests passed on its current build, as they are now.",
        }
    }
}

/// Why `[health]` can't be used. Its message names the setting.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HealthConfigError(String);

/// A selector, ready to match.
#[derive(Debug, Clone, Default)]
struct Selector {
    resource_type: BTreeSet<String>,
    tags: BTreeSet<String>,
    path: Option<GlobSet>,
    name: BTreeSet<String>,
}

impl Selector {
    fn new(config: &HealthSelector, at: &str) -> Result<Self, HealthConfigError> {
        let path = if config.path.is_empty() {
            None
        } else {
            let mut set = GlobSetBuilder::new();
            for pattern in &config.path {
                let glob = Glob::new(pattern).map_err(|e| {
                    HealthConfigError(format!("{at}.path: `{pattern}` isn't a valid glob: {e}"))
                })?;
                set.add(glob);
            }
            Some(
                set.build()
                    .map_err(|e| HealthConfigError(format!("{at}.path: {e}")))?,
            )
        };
        Ok(Self {
            resource_type: config.resource_type.iter().cloned().collect(),
            tags: config.tags.iter().cloned().collect(),
            path,
            name: config.name.iter().cloned().collect(),
        })
    }

    /// Whether `node` matches every field that is set.
    fn matches(&self, node: &NodeFacts) -> bool {
        (self.resource_type.is_empty() || self.resource_type.contains(&node.resource_type))
            && (self.tags.is_empty() || node.tags.iter().any(|t| self.tags.contains(t)))
            && self
                .path
                .as_ref()
                .is_none_or(|set| node.path.as_deref().is_some_and(|p| set.is_match(p)))
            && (self.name.is_empty() || self.name.contains(&node.name))
    }

    fn is_empty(&self) -> bool {
        self.resource_type.is_empty()
            && self.tags.is_empty()
            && self.path.is_none()
            && self.name.is_empty()
    }
}

/// A built-in check as configured.
#[derive(Debug, Clone)]
struct Configured {
    check: Builtin,
    /// `None` when it is off.
    severity: Option<Severity>,
    select: Selector,
    exclude: Selector,
}

impl Configured {
    fn applies(&self, node: &NodeFacts) -> bool {
        self.select.matches(node) && (self.exclude.is_empty() || !self.exclude.matches(node))
    }
}

/// The health checks as `[health]` sets them up: which run, how severe a failure is,
/// and on which nodes. [`HealthSettings::default`] is every built-in at its defaults.
#[derive(Clone)]
pub struct HealthSettings {
    checks: Vec<Configured>,
    declared: Vec<declared::Declared>,
    probes: Vec<probe::Probe>,
    connection: Option<ProbeConnection>,
    coverage: Vec<coverage::Target>,
    plugins: Vec<Plugin>,
    unknown_counts_as: Health,
}

/// A check registered through the `health_check` contract.
#[derive(Clone)]
struct Plugin {
    check: Arc<dyn HealthCheck>,
    severity: Severity,
}

/// Runs a registered check on `scope`, adding its finding to each node in `per_node`:
/// *unknown* for every node when it errs, takes longer than `timeout` or answers about a
/// node outside the scope, and for a node it leaves unanswered or answers twice.
async fn run_plugin(
    plugin: &Plugin,
    scope: &CheckScope,
    timeout: Duration,
    per_node: &mut BTreeMap<String, Vec<Finding>>,
) -> CheckRun {
    let info = plugin.check.describe();
    let run = CheckRun {
        id: info.id.clone(),
        source: CheckSource::Plugin,
        severity: plugin.severity,
        about: info.about.clone(),
    };
    let answered = match tokio::time::timeout(timeout, plugin.check.check(scope)).await {
        Ok(Ok(findings)) => Ok(findings),
        Ok(Err(e)) => Err(format!("the check couldn't run: {e}")),
        Err(_) => Err(format!(
            "the check didn't answer within {}s",
            timeout.as_secs()
        )),
    };
    // An answer about a node outside the scope breaks the contract: nothing the
    // check said can be trusted, so all of it is unknown (AGENTS rule 3).
    let answered = answered.and_then(|findings| {
            let ids: BTreeSet<&str> = scope.nodes.iter().map(|n| n.id.as_str()).collect();
            match findings.iter().find(|f| !ids.contains(f.node.as_str())) {
                Some(stranger) => Err(format!(
                    "the check answered about `{}`, which it wasn't asked about, so none of its answers count",
                    stranger.node
                )),
                None => Ok(findings),
            }
        });
    let mut answers: BTreeMap<&str, Vec<&CheckFinding>> = BTreeMap::new();
    if let Ok(findings) = &answered {
        for finding in findings {
            answers
                .entry(finding.node.as_str())
                .or_default()
                .push(finding);
        }
    }
    for node in &scope.nodes {
        let (status, reason, evidence) = match (&answered, answers.get(node.id.as_str())) {
            (Err(why), _) => (Status::Unknown, why.clone(), BTreeMap::new()),
            (Ok(_), None) => (
                Status::Unknown,
                "the check didn't answer about this node".to_owned(),
                BTreeMap::new(),
            ),
            (Ok(_), Some(many)) if many.len() > 1 => (
                Status::Unknown,
                "the check answered about this node more than once".to_owned(),
                BTreeMap::new(),
            ),
            (Ok(_), Some(one)) => (
                one[0].status,
                one[0].reason.clone(),
                one[0].evidence.clone(),
            ),
        };
        if let Some(findings) = per_node.get_mut(&node.id) {
            findings.push(Finding {
                check: info.id.clone(),
                source: CheckSource::Plugin,
                status,
                severity: plugin.severity,
                reason,
                evidence,
            });
        }
    }
    run
}

impl fmt::Debug for HealthSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HealthSettings")
            .field("checks", &self.checks)
            .field("declared", &self.declared)
            .field("probes", &self.probes)
            .field("connection", &self.connection)
            .field("coverage", &self.coverage)
            .field(
                "plugins",
                &self
                    .plugins
                    .iter()
                    .map(|p| p.check.describe().id)
                    .collect::<Vec<_>>(),
            )
            .field("unknown_counts_as", &self.unknown_counts_as)
            .finish()
    }
}

fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warn => "warn",
        _ => "info",
    }
}

/// How long a registered check may take before its findings are unknown.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// Every node's health after a run of every check: built-in and registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct HealthReport {
    /// Each node's badge, by id.
    pub badges: BTreeMap<String, HealthBadge>,
    /// The checks that ran: id, source and severity, in order.
    pub checks: Vec<CheckRun>,
    /// Each coverage target's verdict, about the project as a whole (ADR-0030 §3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage: Vec<CoverageFinding>,
    /// Probes that ran under a login the check didn't find read-only, because the run
    /// allowed it (`--allow-elevated-login`, ADR-0030 §4c): kept, so it is never hidden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevated_login: Option<ElevatedLogin>,
}

/// A check that ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CheckRun {
    /// Its id.
    pub id: String,
    /// Where it comes from.
    pub source: CheckSource,
    /// How severe its failure is.
    pub severity: Severity,
    /// What it checks.
    pub about: String,
}

impl HealthReport {
    /// Whether a finding at severity error failed, or, when `strict`, couldn't decide:
    /// the verdict of `ods health check` (exit 5).
    pub fn fails(&self, strict: bool) -> bool {
        let fails = |severity: Severity, status: Status| {
            severity == Severity::Error
                && (status == Status::Fail || (strict && status == Status::Unknown))
        };
        self.badges
            .values()
            .flat_map(|b| &b.findings)
            .any(|f| fails(f.severity, f.status))
            || self.coverage.iter().any(|c| fails(c.severity, c.status))
    }

    /// With each coverage target's verdict.
    #[must_use]
    pub fn with_coverage(mut self, coverage: Vec<CoverageFinding>) -> Self {
        self.coverage = coverage;
        self
    }
}

impl Default for HealthSettings {
    fn default() -> Self {
        Self::from_config(&HealthConfig::default())
            .unwrap_or_else(|_| unreachable!("the default configuration is valid"))
    }
}

impl HealthSettings {
    /// The settings `config` describes.
    ///
    /// # Errors
    /// A check id that isn't a built-in's, or a path pattern that isn't a valid glob.
    pub fn from_config(config: &HealthConfig) -> Result<Self, HealthConfigError> {
        let configured = declared::Declared::from_config(&config.checks)?;
        if let Some(unknown) = config
            .builtin
            .keys()
            .find(|id| !BUILTINS.iter().any(|b| b.id() == id.as_str()))
        {
            let known: Vec<&str> = BUILTINS.iter().map(|b| b.id()).collect();
            return Err(HealthConfigError(format!(
                "health.builtin.{unknown}: no built-in check is called `{unknown}`; the built-in checks are {}",
                known.join(", ")
            )));
        }
        let checks = BUILTINS
            .iter()
            .map(|&check| {
                let at = format!("health.builtin.{}", check.id());
                let own = config.builtin.get(check.id());
                let empty = HealthCheckConfig::default();
                let own = own.unwrap_or(&empty);
                let severity = match own.severity {
                    None => Some(check.default_severity()),
                    Some(HealthSeverity::Error) => Some(Severity::Error),
                    Some(HealthSeverity::Warn) => Some(Severity::Warn),
                    Some(HealthSeverity::Info) => Some(Severity::Info),
                    Some(HealthSeverity::Off) => None,
                };
                let select = match &own.select {
                    Some(select) => Selector::new(select, &format!("{at}.select"))?,
                    None => Selector::new(&check.default_select(), &at)?,
                };
                let exclude = match &own.exclude {
                    Some(exclude) => Selector::new(exclude, &format!("{at}.exclude"))?,
                    None => Selector::default(),
                };
                Ok(Configured {
                    check,
                    severity,
                    select,
                    exclude,
                })
            })
            .collect::<Result<Vec<_>, HealthConfigError>>()?;
        Ok(Self {
            checks,
            declared: configured.declared,
            probes: configured.probes,
            connection: None,
            coverage: coverage::Target::from_config(&config.coverage)?,
            plugins: Vec::new(),
            unknown_counts_as: match config.unknown_counts_as {
                Some(UnknownCountsAs::Warning) => Health::Warning,
                _ => Health::Unknown,
            },
        })
    }

    /// Whether `check` runs.
    pub fn enabled(&self, check: Builtin) -> bool {
        self.checks
            .iter()
            .any(|c| c.check == check && c.severity.is_some())
    }

    /// Each built-in check, its severity (`None` when off), in order.
    pub fn checks(&self) -> Vec<(Builtin, Option<Severity>)> {
        self.checks.iter().map(|c| (c.check, c.severity)).collect()
    }

    /// What a check that couldn't decide makes a node: unknown or warning.
    pub fn unknown_counts_as(&self) -> Health {
        self.unknown_counts_as
    }

    /// Adds a check registered through the `health_check` contract, at its own default
    /// severity. Its id must be well formed, and no other check's.
    ///
    /// # Errors
    /// A malformed id, or one another check already has.
    pub fn with_check(mut self, check: Arc<dyn HealthCheck>) -> Result<Self, HealthConfigError> {
        let info = check.describe();
        if !CheckInfo::valid_id(&info.id) {
            return Err(HealthConfigError(format!(
                "a health check's id `{}` isn't valid: lowercase letters, digits, `_`, `-` and `.`, starting with a letter",
                info.id
            )));
        }
        let taken = BUILTINS.iter().any(|b| b.id() == info.id)
            || self.declared.iter().any(|d| d.id == info.id)
            || self.probes.iter().any(|p| p.id == info.id)
            || self
                .plugins
                .iter()
                .any(|p| p.check.describe().id == info.id);
        if taken {
            return Err(HealthConfigError(format!(
                "two health checks are called `{}`",
                info.id
            )));
        }
        self.plugins.push(Plugin {
            severity: info.default_severity,
            check,
        });
        Ok(self)
    }

    /// Runs every check, built-in and registered, on `scope`'s nodes. A registered check
    /// that errs, takes longer than `timeout`, leaves a node unanswered or answers it
    /// twice gives *unknown* for those nodes, never a pass (AGENTS rule 3); answers
    /// about nodes outside the scope are ignored.
    pub async fn run(&self, scope: &CheckScope, timeout: Duration) -> HealthReport {
        // In-process checks, then probes, which need the warehouse.
        let (mut probed, elevated_login) = probe::run_all(
            &self.probes,
            &scope.nodes,
            self.connection.as_ref(),
            timeout,
        )
        .await;
        let mut per_node: BTreeMap<String, Vec<Finding>> = scope
            .nodes
            .iter()
            .map(|node| {
                let mut findings = self.live_findings(node, scope.last_run.as_ref());
                findings.extend(probed.remove(&node.id).unwrap_or_default());
                (node.id.clone(), findings)
            })
            .collect();
        let mut checks: Vec<CheckRun> = self
            .checks
            .iter()
            .filter_map(|c| {
                Some(CheckRun {
                    id: c.check.id().to_owned(),
                    source: CheckSource::Builtin,
                    severity: c.severity?,
                    about: c.check.about().to_owned(),
                })
            })
            .collect();
        checks.extend(self.declared.iter().filter_map(declared::Declared::run));
        checks.extend(self.probes.iter().filter_map(probe::Probe::run));
        for plugin in &self.plugins {
            checks.push(run_plugin(plugin, scope, timeout, &mut per_node).await);
        }
        HealthReport {
            badges: per_node
                .into_iter()
                .map(|(id, findings)| (id, badge(findings, self.unknown_counts_as)))
                .collect(),
            checks,
            coverage: Vec::new(),
            elevated_login,
        }
    }

    /// The node's health, from every enabled built-in check that applies to it.
    pub fn evaluate(&self, node: &NodeFacts, failures: Option<&LastFailures>) -> HealthBadge {
        self.evaluate_with(node, failures, None)
    }

    /// The node's health, from every enabled built-in check that applies to it, worked
    /// out now, and every other check's finding in `recorded`, the latest health record
    /// (ADR-0030 §6).
    pub fn evaluate_with(
        &self,
        node: &NodeFacts,
        failures: Option<&LastFailures>,
        recorded: Option<&record::Recorded>,
    ) -> HealthBadge {
        let mut findings = self.live_findings(node, failures);
        if let Some(recorded) = recorded {
            findings.extend(recorded.findings_for(&node.id));
        }
        badge(findings, self.unknown_counts_as)
    }

    /// Every enabled in-process check's finding about `node`: the built-ins, then the
    /// declared checks.
    fn live_findings(&self, node: &NodeFacts, failures: Option<&LastFailures>) -> Vec<Finding> {
        let mut findings: Vec<Finding> = self
            .checks
            .iter()
            .filter_map(|c| {
                let severity = c.severity?;
                let (status, reason) = if c.applies(node) {
                    run(c.check, node, failures)
                } else {
                    (Status::Skipped, "not selected for this check".to_owned())
                };
                Some(Finding {
                    check: c.check.id().to_owned(),
                    source: CheckSource::Builtin,
                    status,
                    severity,
                    reason,
                    evidence: if c.applies(node) {
                        evidence(c.check, node, failures)
                    } else {
                        BTreeMap::new()
                    },
                })
            })
            .collect();
        findings.extend(self.declared.iter().filter_map(|d| d.finding(node)));
        findings
    }

    /// Checks every probe's SQL with `read_only`, the host's SQL analyzer: a probe runs
    /// only once its query is known to be one read-only query (ADR-0030 §4a).
    ///
    /// # Errors
    /// A probe whose SQL isn't one read-only query: a configuration error, before
    /// anything connects.
    pub fn check_probe_sql(
        &mut self,
        read_only: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<(), HealthConfigError> {
        self.probes
            .iter_mut()
            .try_for_each(|p| p.check_read_only(read_only))
    }

    /// What a trust entry pins for each probe (ADR-0030 §4d).
    pub fn probe_definitions(&self) -> Vec<ProbeDefinition> {
        self.probes.iter().map(probe::Probe::definition).collect()
    }

    /// Marks the probes whose ids are in `trusted` as trusted for this project, and the
    /// rest as not (ADR-0030 §4b).
    pub fn trust_probes(&mut self, trusted: &BTreeSet<String>) {
        for probe in &mut self.probes {
            probe.set_trusted(trusted.contains(&probe.id));
        }
    }

    /// Probe checks run through `connection` (ADR-0030 §4c, §5). Without one, a probe
    /// that passes the other guards is *unknown*: there is nothing to run it through.
    /// Probes run one after another, each with its login check and its query limited to
    /// [`run`](Self::run)'s timeout apiece.
    #[must_use]
    pub fn with_probe_connection(mut self, connection: ProbeConnection) -> Self {
        self.connection = Some(connection);
        self
    }

    /// Each enabled coverage target's verdict on what a host `measured` (ADR-0030 §3).
    pub fn coverage(&self, measured: &[Measured]) -> Vec<CoverageFinding> {
        self.coverage
            .iter()
            .filter_map(|t| t.judge(measured))
            .collect()
    }

    /// Whether a check other than `last_run_failed`, built in or declared, is at
    /// severity error: it can fail a node without the last run's record.
    pub fn errs_without_run_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|c| c.check != Builtin::LastRunFailed && c.severity == Some(Severity::Error))
            || self
                .declared
                .iter()
                .any(|d| d.severity == Some(Severity::Error))
    }

    /// How badges are worked out under these settings, for people.
    pub fn how(&self, failures_known: bool) -> String {
        let mut parts = Vec::new();
        for (check, severity) in self.checks() {
            let severity = match severity {
                // It never fails, so only on or off means anything.
                Some(_) if check == Builtin::Built => "on",
                Some(Severity::Error) => "error",
                Some(Severity::Warn) => "warn",
                Some(_) => "info",
                None => "off",
            };
            parts.push(format!("{} ({severity}): {}", check.id(), check.about()));
        }
        for declared in &self.declared {
            parts.push(format!(
                "{} ({}, declared): {}",
                declared.id,
                declared.severity.map_or("off", severity_word),
                declared.about()
            ));
        }
        for probe in &self.probes {
            parts.push(format!(
                "{} ({}, probe): {}",
                probe.id,
                probe.severity.map_or("off", severity_word),
                probe.about()
            ));
        }
        for plugin in &self.plugins {
            let info = plugin.check.describe();
            parts.push(format!(
                "{} ({}, plugin): {}",
                info.id,
                severity_word(plugin.severity),
                info.about
            ));
        }
        let mut how = format!(
            "A node is failing when a check at severity error fails, then {} when a check couldn't decide, a warning when a check at severity warn fails, and healthy when every check that applies passed. Checks (configure them under [health] in ods.toml): {}",
            match self.unknown_counts_as {
                Health::Warning => "a warning",
                _ => "unknown",
            },
            parts.join(" ")
        );
        if !failures_known && self.enabled(Builtin::LastRunFailed) {
            how.push_str(
                " The last run's record doesn't say what failed, so the last-run checks can't decide, and nodes they apply to read unknown (or a warning, as configured).",
            );
        }
        if !self.coverage.is_empty() {
            let targets: Vec<String> = self
                .coverage
                .iter()
                .map(coverage::Target::describe)
                .collect();
            let _ = write!(
                how,
                " Coverage targets, for the project as a whole: {}.",
                targets.join(", ")
            );
        }
        how
    }
}

fn short(run: &str) -> String {
    run.chars().take(8).collect()
}

/// What a built-in check on `node` is concluded from, for machines (AGENTS rule 4).
fn evidence(
    check: Builtin,
    node: &NodeFacts,
    failures: Option<&LastFailures>,
) -> BTreeMap<String, String> {
    let mut evidence = BTreeMap::new();
    let mut put = |key: &str, value: String| {
        evidence.insert(key.to_owned(), value);
    };
    let build = node.build.as_ref();
    match check {
        Builtin::Built => {
            if let Some(b) = build {
                put("run_id", b.run_id.clone());
                put("built_at", b.built_at.to_string());
            }
        }
        Builtin::LastRunFailed | Builtin::LastRunSkipped => {
            if let Some(f) = failures {
                put("last_run_command", f.command.clone());
                put("last_run_started_at", f.started_at.to_string());
            }
            if let Some(b) = build {
                put("built_at", b.built_at.to_string());
            }
        }
        Builtin::TestsRequired => put("tests", node.tests.to_string()),
        Builtin::TestsPassed => {
            put("tests", node.tests.to_string());
            if let Some(b) = build {
                put("run_id", b.run_id.clone());
                if let Some((run, at)) = &b.tested {
                    put("tested_run_id", run.clone());
                    put("tested_at", at.to_string());
                    put("tests_current", b.checks_current.to_string());
                }
            }
        }
    }
    evidence
}

/// One built-in check on one node.
fn run(check: Builtin, node: &NodeFacts, failures: Option<&LastFailures>) -> (Status, String) {
    let build = node.build.as_ref();
    // A failure counts until a later build replaces it (e.g. recorded elsewhere).
    // Times are kept to the second, so a build in the same second as the record isn't
    // taken as later: the failure stands (AGENTS rule 3).
    let since = |f: &LastFailures| build.is_none_or(|b| b.built_at <= f.started_at);
    match check {
        Builtin::Built => match build {
            Some(b) => (
                Status::Pass,
                format!("built in run {} at {}", short(&b.run_id), b.built_at),
            ),
            None => (
                Status::Unknown,
                "never built by ODS: nothing to judge it on".to_owned(),
            ),
        },
        Builtin::LastRunFailed => match failures {
            // No record of this scope's last run: whether it failed can't be told, so
            // the check can't decide (AGENTS rule 3).
            None => (
                Status::Unknown,
                "the last run's record doesn't say what failed: failures aren't measured"
                    .to_owned(),
            ),
            Some(f) if f.failed.contains(&node.id) && since(f) => (
                Status::Fail,
                format!(
                    "failed in the last run ({}, started {}), and hasn't been built since",
                    f.command, f.started_at
                ),
            ),
            Some(_) => (Status::Pass, "didn't fail in the last run".to_owned()),
        },
        Builtin::LastRunSkipped => match failures {
            None => (
                Status::Unknown,
                "the last run's record doesn't say what was skipped".to_owned(),
            ),
            Some(f) if f.skipped.contains(&node.id) && since(f) => (
                Status::Fail,
                format!(
                    "skipped in the last run ({}) because something upstream failed",
                    f.command
                ),
            ),
            Some(_) => (Status::Pass, "wasn't skipped in the last run".to_owned()),
        },
        Builtin::TestsRequired => {
            if node.tests == 0 {
                (Status::Fail, "no tests: nothing checks its data".to_owned())
            } else {
                (Status::Pass, format!("{} test(s) read it", node.tests))
            }
        }
        Builtin::TestsPassed => match (node.tests, build) {
            (0, _) => (
                Status::Skipped,
                format!("a {} has no tests to run", node.resource_type),
            ),
            (_, None) => (
                Status::Skipped,
                "never built: its tests can't have passed on a build".to_owned(),
            ),
            (_, Some(b)) => match (&b.tested, b.checks_current) {
                (None, _) => (
                    Status::Fail,
                    "its tests haven't been recorded passing on its current build".to_owned(),
                ),
                (Some((run, _)), false) => (
                    Status::Fail,
                    format!(
                        "its tests changed since they passed in run {}: the new ones haven't run",
                        short(run)
                    ),
                ),
                (Some((run, at)), true) => (
                    Status::Pass,
                    format!("its tests passed in run {} at {at}", short(run)),
                ),
            },
        },
    }
}

/// The badge rule (ADR-0030 §1): failing on any error, then unknown (or a warning, as
/// configured) when a check couldn't decide, a warning on any warn, healthy when every
/// check that applies passed, and unknown when none applies. Unknown is never healthy.
fn badge(findings: Vec<Finding>, unknown_counts_as: Health) -> HealthBadge {
    let reasons = |pick: &dyn Fn(&Finding) -> bool| -> Vec<String> {
        findings
            .iter()
            .filter(|f| pick(f))
            .map(|f| f.reason.clone())
            .collect()
    };
    let errors = reasons(&|f| f.status == Status::Fail && f.severity == Severity::Error);
    let unknowns = reasons(&|f| f.status == Status::Unknown);
    let warnings = reasons(&|f| f.status == Status::Fail && f.severity == Severity::Warn);
    let (health, reasons) = if !errors.is_empty() {
        (Health::Failing, errors)
    } else if !unknowns.is_empty() && unknown_counts_as == Health::Unknown {
        (Health::Unknown, unknowns)
    } else if !unknowns.is_empty() || !warnings.is_empty() {
        (Health::Warning, [warnings, unknowns].concat())
    } else if findings.iter().any(|f| f.status == Status::Pass) {
        // Healthy, and why: the build, and its tests (or that it has none).
        let why = reasons(&|f| {
            matches!(f.check.as_str(), "built" | "tests_passed")
                && matches!(f.status, Status::Pass | Status::Skipped)
                && !f.reason.starts_with("not selected")
        });
        (Health::Healthy, why)
    } else {
        (
            Health::Unknown,
            vec!["no enabled check applies to it".to_owned()],
        )
    };
    HealthBadge {
        health,
        reasons,
        findings,
    }
}

impl fmt::Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

/// Counts nodes by health, every health listed.
pub fn counts<'a>(badges: impl IntoIterator<Item = &'a HealthBadge>) -> BTreeMap<Health, usize> {
    let mut counts: BTreeMap<Health, usize> = HEALTHS.iter().map(|h| (*h, 0)).collect();
    for badge in badges {
        *counts.entry(badge.health).or_default() += 1;
    }
    counts
}

mod coverage;
mod declared;
mod pass;
mod probe;
pub mod record;
pub mod trust;

pub use coverage::{COVERAGE_MEASURES, CoverageFinding, Measured, Share};
pub use probe::{ElevatedLogin, ProbeConnection, ProbeDefinition};

#[cfg(test)]
mod tests;
