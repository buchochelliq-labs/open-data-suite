//! Probe checks (ADR-0030 §2 tier 3, §4a–§4d): one read-only query per selected node,
//! with a `pass` condition over the row it returns. A probe is configured in
//! `[[health.checks]]` with `kind = "probe"`, and runs only once three guards hold:
//! - its SQL is one read-only query, checked by the host's SQL analyzer when the
//!   configuration loads (§4a);
//! - its definition is trusted for this project (§4b, §4d);
//! - its connection can do no more than read what it probes (§4c).
//!
//! Probes need a warehouse, so they run only in `ods health check`, never in the
//! dashboard, which reads their findings from the health record (§6).

use std::collections::BTreeMap;

use ods_config::DeclaredCheckConfig;
use ods_sdk::contracts::probe::{PLACEHOLDER, ProbeStatement};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::pass::Pass;
use crate::{
    CheckRun, CheckSource, Finding, HealthConfigError, NodeFacts, Selector, Severity, Status,
};

/// What a probe's `{relation}` reads as when its SQL is checked as read-only: a plain
/// identifier, so the query parses before any relation is known.
const CHECKED_AS: &str = "ods_probe_relation";

/// A probe check, ready to be guarded and run.
#[derive(Debug, Clone)]
pub(crate) struct Probe {
    pub(crate) id: String,
    /// Where it is configured, e.g. `health.checks[2]`, for messages.
    at: String,
    /// `None` when it is off.
    pub(crate) severity: Option<Severity>,
    select: Selector,
    exclude: Option<Selector>,
    sql: String,
    pass_text: String,
    #[expect(
        dead_code,
        reason = "evaluated on the probe's row once probes run (#392)"
    )]
    pass: Pass,
    statement: ProbeStatement,
    digest: String,
    /// Whether its SQL was checked as one read-only query (§4a).
    read_only: bool,
    /// Whether its definition is trusted for this project (§4b).
    trusted: bool,
}

/// What a trust entry pins for one probe: the definition the user reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProbeDefinition {
    /// The check's id.
    pub id: String,
    /// Its query, as configured.
    pub sql: String,
    /// `sha256:` and the digest of what decides what runs and where (§4d).
    pub digest: String,
}

/// What a probe's digest covers, in a fixed order: what runs, and where.
#[derive(Serialize)]
struct Digested<'a> {
    id: &'a str,
    kind: &'a str,
    sql: &'a str,
    select: Option<&'a ods_config::HealthSelector>,
    exclude: Option<&'a ods_config::HealthSelector>,
}

impl Probe {
    pub(crate) fn from_config(
        config: &DeclaredCheckConfig,
        at: &str,
        severity: Option<Severity>,
        select: Option<Selector>,
        exclude: Option<Selector>,
    ) -> Result<Self, HealthConfigError> {
        if !config.require.is_empty() {
            return Err(HealthConfigError(format!(
                "{at}.require: a probe has `sql` and `pass`, not `require`"
            )));
        }
        // A probe never runs on every node by default: it names what it reads.
        let select = select.ok_or_else(|| {
            HealthConfigError(format!(
                "{at}.select: a probe names the nodes it runs on, e.g. select = {{ name = [\"orders\"] }}"
            ))
        })?;
        let sql = config.sql.as_deref().ok_or_else(|| {
            HealthConfigError(format!(
                "{at}.sql: a probe needs one read-only query with {PLACEHOLDER}, e.g. \"select count(*) as n from {PLACEHOLDER}\""
            ))
        })?;
        let pass_text = config.pass.as_deref().ok_or_else(|| {
            HealthConfigError(format!(
                "{at}.pass: a probe needs a condition over the columns its query returns, e.g. \"n > 0\""
            ))
        })?;
        let pass = Pass::parse(pass_text)
            .map_err(|e| HealthConfigError(format!("{at}.pass: `{pass_text}`: {e}")))?;
        let statement = ProbeStatement::new(sql, pass.columns())
            .map_err(|e| HealthConfigError(format!("{at}.sql: {e}")))?;
        let digested = Digested {
            id: &config.id,
            kind: "probe",
            sql,
            select: config.select.as_ref(),
            exclude: config.exclude.as_ref(),
        };
        let bytes =
            serde_json::to_vec(&digested).map_err(|e| HealthConfigError(format!("{at}: {e}")))?;
        Ok(Self {
            id: config.id.clone(),
            at: at.to_owned(),
            severity,
            select,
            exclude,
            sql: sql.to_owned(),
            pass_text: pass_text.to_owned(),
            pass,
            statement,
            digest: format!("sha256:{}", hex::encode(Sha256::digest(bytes))),
            read_only: false,
            trusted: false,
        })
    }

    /// Checks its SQL with `read_only`, the host's analyzer (§4a).
    pub(crate) fn check_read_only(
        &mut self,
        read_only: &dyn Fn(&str) -> Result<(), String>,
    ) -> Result<(), HealthConfigError> {
        read_only(&self.statement.render(CHECKED_AS)).map_err(|why| {
            HealthConfigError(format!(
                "{}.sql: probe `{}` must be one read-only query, but {why}",
                self.at, self.id
            ))
        })?;
        self.read_only = true;
        Ok(())
    }

    /// Marks it trusted, or not.
    pub(crate) fn set_trusted(&mut self, trusted: bool) {
        self.trusted = trusted;
    }

    /// What a trust entry pins for it.
    pub(crate) fn definition(&self) -> ProbeDefinition {
        ProbeDefinition {
            id: self.id.clone(),
            sql: self.sql.clone(),
            digest: self.digest.clone(),
        }
    }

    /// What it checks, for people.
    pub(crate) fn about(&self) -> String {
        format!("probe `{}`, passes when {}", self.sql, self.pass_text)
    }

    /// It, as a check that ran; `None` when it is off.
    pub(crate) fn run(&self) -> Option<CheckRun> {
        Some(CheckRun {
            id: self.id.clone(),
            source: CheckSource::Probe,
            severity: self.severity?,
            about: self.about(),
        })
    }

    fn applies(&self, node: &NodeFacts) -> bool {
        self.select.matches(node) && !self.exclude.as_ref().is_some_and(|s| s.matches(node))
    }

    /// Its finding about `node`; `None` when it is off. A probe that a guard stops is
    /// *unknown*, saying which guard, and never runs.
    pub(crate) fn finding(&self, node: &NodeFacts) -> Option<Finding> {
        let severity = self.severity?;
        let (status, reason) = if !self.applies(node) {
            (Status::Skipped, "not selected for this check".to_owned())
        } else if !self.read_only {
            (
                Status::Unknown,
                format!("{}: its SQL wasn't checked as one read-only query", self.id),
            )
        } else if !self.trusted {
            (
                Status::Unknown,
                format!(
                    "{}: not trusted for this project; review it and run `ods health trust`",
                    self.id
                ),
            )
        } else {
            (
                Status::Unknown,
                format!(
                    "{}: probe checks don't run yet; they come with the read-only connection check (#392)",
                    self.id
                ),
            )
        };
        let evidence = if status == Status::Skipped {
            BTreeMap::new()
        } else {
            BTreeMap::from([
                ("read_only_checked".to_owned(), self.read_only.to_string()),
                ("trusted".to_owned(), self.trusted.to_string()),
                ("sql".to_owned(), self.sql.clone()),
            ])
        };
        Some(Finding {
            check: self.id.clone(),
            source: CheckSource::Probe,
            status,
            severity,
            reason,
            evidence,
        })
    }
}
