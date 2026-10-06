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
//! A module crate: it depends on `ods-core` and `ods-config` only, never on providers or
//! other modules (ADR-0001).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use globset::{Glob, GlobSet, GlobSetBuilder};
use ods_config::{
    HealthCheckConfig, HealthConfig, HealthSelector, HealthSeverity, UnknownCountsAs,
};
use ods_core::state::Timestamp;
use serde::Serialize;

// ---------------------------------------------------------------------------- facts

/// What the engine knows about a node: neutral facts from the project and the state
/// store, filled in by the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct NodeFacts {
    /// Its id, e.g. `model.shop.orders`.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its resource type, e.g. `model`, `seed`, `snapshot`.
    pub resource_type: String,
    /// Its file, relative to the project, if known.
    pub path: Option<String>,
    /// Its tags.
    pub tags: Vec<String>,
    /// How many tests (data and unit) read it.
    pub tests: usize,
    /// Its last successful build, if ODS recorded one.
    pub build: Option<BuildFacts>,
}

impl NodeFacts {
    /// A node with nothing known about it but its id, name and type.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        resource_type: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            resource_type: resource_type.into(),
            ..Self::default()
        }
    }
}

/// A node's last successful build.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BuildFacts {
    /// The run that built it.
    pub run_id: String,
    /// When the build finished.
    pub built_at: Timestamp,
    /// The run whose tests all passed on this build, and when, if recorded.
    pub tested: Option<(String, Timestamp)>,
    /// Whether the tests that passed are the node's tests now.
    pub checks_current: bool,
}

impl BuildFacts {
    /// A build by `run_id` at `built_at`, its tests not recorded.
    pub fn new(run_id: impl Into<String>, built_at: Timestamp) -> Self {
        Self {
            run_id: run_id.into(),
            built_at,
            tested: None,
            checks_current: false,
        }
    }

    /// Records that its tests passed in `run_id` at `at`, and whether they are the
    /// node's tests now.
    #[must_use]
    pub fn tested(mut self, run_id: impl Into<String>, at: Timestamp, current: bool) -> Self {
        self.tested = Some((run_id.into(), at));
        self.checks_current = current;
        self
    }
}

/// What the last run's record says failed. Only from a record of this scope: another
/// target's run says nothing about these nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LastFailures {
    /// When the run started.
    pub started_at: Timestamp,
    /// Its command, e.g. `ods state build`.
    pub command: String,
    /// Nodes that failed, or whose tests failed.
    pub failed: BTreeSet<String>,
    /// Nodes skipped because something upstream failed.
    pub skipped: BTreeSet<String>,
}

impl LastFailures {
    /// A run started at `started_at` by `command`.
    pub fn new(
        started_at: Timestamp,
        command: impl Into<String>,
        failed: impl IntoIterator<Item = String>,
        skipped: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            started_at,
            command: command.into(),
            failed: failed.into_iter().collect(),
            skipped: skipped.into_iter().collect(),
        }
    }
}

// ---------------------------------------------------------------------------- findings

/// How severe a check's failure is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Severity {
    /// The node is failing.
    Error,
    /// The node is a warning.
    Warn,
    /// Shown, and changes nothing.
    Info,
}

/// What a check concluded about a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Status {
    /// It passed.
    Pass,
    /// It failed.
    Fail,
    /// The check couldn't decide.
    Unknown,
    /// The check doesn't apply to the node, or had nothing to look at.
    Skipped,
}

/// One check's conclusion about one node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Finding {
    /// The check's id, e.g. `tests_required`.
    pub check: &'static str,
    /// Where the check comes from: `builtin` for now (ADR-0030 §2).
    pub source: &'static str,
    /// What it concluded.
    pub status: Status,
    /// How severe a failure is.
    pub severity: Severity,
    /// Why.
    pub reason: String,
}

/// A node's health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone)]
pub struct HealthSettings {
    checks: Vec<Configured>,
    unknown_counts_as: Health,
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

    /// The node's health, from every enabled check that applies to it.
    pub fn evaluate(&self, node: &NodeFacts, failures: Option<&LastFailures>) -> HealthBadge {
        let findings: Vec<Finding> = self
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
                    check: c.check.id(),
                    source: "builtin",
                    status,
                    severity,
                    reason,
                })
            })
            .collect();
        badge(findings, self.unknown_counts_as)
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
                Some(Severity::Info) => "info",
                None => "off",
            };
            parts.push(format!("{} ({severity}): {}", check.id(), check.about()));
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
        how
    }
}

fn short(run: &str) -> String {
    run.chars().take(8).collect()
}

/// One built-in check on one node.
fn run(check: Builtin, node: &NodeFacts, failures: Option<&LastFailures>) -> (Status, String) {
    let build = node.build.as_ref();
    // A failure counts until a later build replaces it (e.g. recorded elsewhere).
    let since = |f: &LastFailures| build.is_none_or(|b| b.built_at < f.started_at);
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
            matches!(f.check, "built" | "tests_passed")
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

#[cfg(test)]
mod tests;
