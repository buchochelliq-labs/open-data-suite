//! Health checks and what they found (#181, ADR-0023).
//!
//! `ods doctor` runs a set of checks over the project, configuration, tools, target,
//! state store and provider capabilities. Each check yields one [`CheckResult`]:
//! typed data, never a formatted string, so the CLI can render it for people and
//! serialise it for machines, and an editor or server can reuse it.
//!
//! The model is presentation-free and provider-neutral: what a check is about is data
//! (its id, category and the provider it concerns), not a branch in this crate.

use serde::Serialize;

/// What a check concluded.
///
/// `Unknown` and `Skipped` are outcomes in their own right: a check that couldn't
/// conclude is never reported as `Ok` (AGENTS.md rule 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CheckStatus {
    /// The check passed.
    Ok,
    /// The check was not run, by choice (e.g. a live check without `--connect`).
    Skipped,
    /// Something needs attention, but ODS can work.
    Warning,
    /// The check couldn't conclude, e.g. because a check it depends on failed.
    Unknown,
    /// ODS can't work correctly until this is fixed.
    Error,
}

impl CheckStatus {
    /// Every status, in the order of the summary.
    pub const ALL: [CheckStatus; 5] = [
        CheckStatus::Ok,
        CheckStatus::Warning,
        CheckStatus::Error,
        CheckStatus::Unknown,
        CheckStatus::Skipped,
    ];

    /// The status's stable name, as serialised.
    pub const fn name(self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Skipped => "skipped",
            CheckStatus::Warning => "warning",
            CheckStatus::Unknown => "unknown",
            CheckStatus::Error => "error",
        }
    }
}

/// What part of the environment a check looks at. Declared in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CheckCategory {
    /// ODS's own configuration: files, profile, effective values.
    Config,
    /// The project and the artifacts ODS reads.
    Project,
    /// External programs ODS runs, and their versions.
    Tools,
    /// Where the project builds (ADR-0017).
    Target,
    /// The state store.
    StateStore,
    /// What the wired providers can do, and what is missing.
    Capabilities,
    /// Live checks against the warehouse (only when asked for).
    Connectivity,
}

impl CheckCategory {
    /// The category's stable name, as serialised.
    pub const fn name(self) -> &'static str {
        match self {
            CheckCategory::Config => "config",
            CheckCategory::Project => "project",
            CheckCategory::Tools => "tools",
            CheckCategory::Target => "target",
            CheckCategory::StateStore => "state_store",
            CheckCategory::Capabilities => "capabilities",
            CheckCategory::Connectivity => "connectivity",
        }
    }
}

/// One fact a check relied on: a named value and, when it matters, where the value
/// came from (e.g. a flag, a variable, a configuration file or a default).
///
/// Values are never secrets: a credential appears only as its reference (ADR-0005).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Evidence {
    /// What the value is, e.g. `manifest` or `target_dir`.
    pub key: String,
    /// The value.
    pub value: String,
    /// Where the value came from, if that is part of the finding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl Evidence {
    /// A value.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            source: None,
        }
    }

    /// Says where the value came from.
    #[must_use]
    pub fn from_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }
}

/// The outcome of one check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CheckResult {
    /// Stable id of what was checked, `<category>.<name>`, e.g. `project.manifest`.
    pub id: String,
    /// What part of the environment it looks at.
    pub category: CheckCategory,
    /// The provider the check concerns, if any, as named on the command line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// What it concluded.
    pub status: CheckStatus,
    /// Whether ODS can't work without it: an `Unknown` outcome fails the run.
    pub required: bool,
    /// Stable code of the finding (`ODS-E…`, `ODS-W…`, `ODS-U…`); none when `Ok` or
    /// `Skipped`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// What was found, for people.
    pub message: String,
    /// The facts it relied on, in a fixed order.
    pub evidence: Vec<Evidence>,
    /// What to do about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl CheckResult {
    /// A check that passed.
    pub fn ok(id: impl Into<String>, category: CheckCategory, message: impl Into<String>) -> Self {
        Self::with_status(id, category, CheckStatus::Ok, None, message)
    }

    /// A check that was not run, by choice.
    pub fn skipped(
        id: impl Into<String>,
        category: CheckCategory,
        message: impl Into<String>,
    ) -> Self {
        Self::with_status(id, category, CheckStatus::Skipped, None, message)
    }

    /// A finding that needs attention.
    pub fn warning(
        id: impl Into<String>,
        category: CheckCategory,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::with_status(
            id,
            category,
            CheckStatus::Warning,
            Some(code.into()),
            message,
        )
    }

    /// A check that couldn't conclude.
    pub fn unknown(
        id: impl Into<String>,
        category: CheckCategory,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::with_status(
            id,
            category,
            CheckStatus::Unknown,
            Some(code.into()),
            message,
        )
    }

    /// A failure.
    pub fn error(
        id: impl Into<String>,
        category: CheckCategory,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::with_status(id, category, CheckStatus::Error, Some(code.into()), message)
    }

    fn with_status(
        id: impl Into<String>,
        category: CheckCategory,
        status: CheckStatus,
        code: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            category,
            provider: None,
            status,
            required: false,
            code,
            message: message.into(),
            evidence: Vec::new(),
            hint: None,
        }
    }

    /// Marks the check as one ODS can't work without.
    #[must_use]
    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// Names the provider the check concerns.
    #[must_use]
    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Adds a fact.
    #[must_use]
    pub fn evidence(mut self, evidence: Evidence) -> Self {
        self.evidence.push(evidence);
        self
    }

    /// Adds a named value.
    #[must_use]
    pub fn fact(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.evidence(Evidence::new(key, value))
    }

    /// Adds a hint.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Whether this outcome fails the run: an error, an unknown required check, or,
    /// when `strict`, any warning or unknown (ADR-0023 §3).
    pub fn fails(&self, strict: bool) -> bool {
        match self.status {
            CheckStatus::Error => true,
            CheckStatus::Unknown => self.required || strict,
            CheckStatus::Warning => strict,
            CheckStatus::Ok | CheckStatus::Skipped => false,
        }
    }
}

/// The overall outcome of a set of checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Verdict {
    /// Every check passed or was skipped.
    Healthy,
    /// Some checks warned or couldn't conclude, but none fails the run.
    Warnings,
    /// At least one check fails the run.
    Failed,
}

/// How many checks ended in each status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Summary {
    /// Checks that passed.
    pub ok: usize,
    /// Checks that warned.
    pub warning: usize,
    /// Checks that failed.
    pub error: usize,
    /// Checks that couldn't conclude.
    pub unknown: usize,
    /// Checks not run.
    pub skipped: usize,
}

/// The checks of one run, in display order, with their verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct HealthReport {
    /// The overall outcome.
    pub verdict: Verdict,
    /// Whether warnings and unknowns fail the run.
    pub strict: bool,
    /// How many checks ended in each status.
    pub summary: Summary,
    /// Every check, by category, then in the order they were given.
    pub checks: Vec<CheckResult>,
}

impl HealthReport {
    /// Orders `checks` by category (stable within one) and decides the verdict.
    pub fn new(mut checks: Vec<CheckResult>, strict: bool) -> Self {
        checks.sort_by_key(|c| c.category);
        let mut summary = Summary::default();
        for check in &checks {
            match check.status {
                CheckStatus::Ok => summary.ok += 1,
                CheckStatus::Warning => summary.warning += 1,
                CheckStatus::Error => summary.error += 1,
                CheckStatus::Unknown => summary.unknown += 1,
                CheckStatus::Skipped => summary.skipped += 1,
            }
        }
        let verdict = if checks.iter().any(|c| c.fails(strict)) {
            Verdict::Failed
        } else if checks
            .iter()
            .any(|c| matches!(c.status, CheckStatus::Warning | CheckStatus::Unknown))
        {
            Verdict::Warnings
        } else {
            Verdict::Healthy
        };
        Self {
            verdict,
            strict,
            summary,
            checks,
        }
    }

    /// The checks that fail the run, in display order.
    pub fn failing(&self) -> impl Iterator<Item = &CheckResult> {
        self.checks.iter().filter(|c| c.fails(self.strict))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(status: CheckStatus, required: bool) -> CheckResult {
        let base = match status {
            CheckStatus::Ok => CheckResult::ok("a.b", CheckCategory::Project, "fine"),
            CheckStatus::Skipped => CheckResult::skipped("a.b", CheckCategory::Project, "not run"),
            CheckStatus::Warning => {
                CheckResult::warning("a.b", CheckCategory::Project, "ODS-W0001", "hm")
            }
            CheckStatus::Unknown => {
                CheckResult::unknown("a.b", CheckCategory::Project, "ODS-U0001", "?")
            }
            CheckStatus::Error => {
                CheckResult::error("a.b", CheckCategory::Project, "ODS-E0001", "no")
            }
        };
        base.required(required)
    }

    #[test]
    fn verdicts_follow_the_exit_rules() {
        use CheckStatus::*;
        let verdict = |checks: Vec<CheckResult>, strict| HealthReport::new(checks, strict).verdict;
        assert_eq!(
            verdict(vec![check(Ok, true), check(Skipped, true)], false),
            Verdict::Healthy
        );
        assert_eq!(
            verdict(vec![check(Ok, true), check(Warning, false)], false),
            Verdict::Warnings
        );
        assert_eq!(verdict(vec![check(Warning, false)], true), Verdict::Failed);
        // An unknown optional check warns; an unknown required one fails.
        assert_eq!(
            verdict(vec![check(Unknown, false)], false),
            Verdict::Warnings
        );
        assert_eq!(verdict(vec![check(Unknown, false)], true), Verdict::Failed);
        assert_eq!(verdict(vec![check(Unknown, true)], false), Verdict::Failed);
        assert_eq!(verdict(vec![check(Error, false)], false), Verdict::Failed);
        assert_eq!(verdict(Vec::new(), true), Verdict::Healthy);
    }

    #[test]
    fn checks_are_grouped_by_category_keeping_their_order() {
        let report = HealthReport::new(
            vec![
                CheckResult::ok("tools.b", CheckCategory::Tools, ""),
                CheckResult::ok("config.a", CheckCategory::Config, ""),
                CheckResult::ok("tools.a", CheckCategory::Tools, ""),
            ],
            false,
        );
        let ids: Vec<&str> = report.checks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["config.a", "tools.b", "tools.a"]);
        assert_eq!(report.summary.ok, 3);
    }

    #[test]
    fn serialises_as_snake_case_and_omits_what_is_absent() {
        let result = CheckResult::warning(
            "project.freshness",
            CheckCategory::Project,
            "ODS-W0206",
            "stale",
        )
        .required(false)
        .provider("p")
        .fact("manifest", "target/manifest.json")
        .evidence(Evidence::new("target", "dev").from_source("flag"))
        .hint("recompile");
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": "project.freshness",
                "category": "project",
                "provider": "p",
                "status": "warning",
                "required": false,
                "code": "ODS-W0206",
                "message": "stale",
                "evidence": [
                    {"key": "manifest", "value": "target/manifest.json"},
                    {"key": "target", "value": "dev", "source": "flag"}
                ],
                "hint": "recompile"
            })
        );
        let ok = serde_json::to_value(CheckResult::ok("x.y", CheckCategory::StateStore, "fine"))
            .unwrap();
        assert!(ok.get("code").is_none() && ok.get("hint").is_none());
        assert_eq!(ok["category"], "state_store");
    }

    #[test]
    fn names_match_serialisation() {
        for status in CheckStatus::ALL {
            assert_eq!(serde_json::to_value(status).unwrap(), status.name());
        }
        assert_eq!(CheckCategory::StateStore.name(), "state_store");
    }
}
