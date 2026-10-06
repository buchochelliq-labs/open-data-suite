//! Probe checks (ADR-0030 §2 tier 3, §4a–§4d): one read-only query per selected node,
//! with a `pass` condition over the row it returns. A probe is configured in
//! `[[health.checks]]` with `kind = "probe"`, and runs only once three guards hold:
//! - its SQL is one read-only query, checked by the host's SQL analyzer when the
//!   configuration loads (§4a);
//! - its definition is trusted for this project (§4b, §4d);
//! - its connection can do no more than read what it probes (§4c).
//!
//! Probes need a warehouse, so they run only in `ods health check`, through a
//! [`ProbeConnection`] the host wires in, never in the dashboard, which reads their
//! findings from the health record (§6).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use ods_config::DeclaredCheckConfig;
use ods_sdk::contracts::privileges::{Access, RelationPrivileges};
use ods_sdk::contracts::probe::{
    PLACEHOLDER, ProbeAnswer, ProbeFilter, ProbeRequest, ProbeStatement, ProbeTarget, RelationProbe,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::pass::{Pass, Verdict};
use crate::{
    CheckRun, CheckSource, Finding, HealthConfigError, NodeFacts, Selector, Severity, Status,
};

/// The relation kinds a probe reads: a node's table or view. Anything else (e.g. an
/// ephemeral model, which has no relation) is skipped or unknown, never probed.
const KINDS: [&str; 3] = ["table", "view", "materialized_view"];

/// What probe checks run through (ADR-0030 §4c, §5): the warehouse connection the
/// host wires in, and what it can say about its login.
#[derive(Clone)]
pub struct ProbeConnection {
    probe: Arc<dyn RelationProbe>,
    privileges: Option<Arc<dyn RelationPrivileges>>,
    allow_elevated_login: bool,
    /// What the connection is, for people, when its login isn't known.
    label: Option<String>,
    /// The relation each node's build made, by node id: a probe reads that one only.
    relations: BTreeMap<String, String>,
}

impl ProbeConnection {
    /// Probes run through `connection`, whose login it also asks about before each
    /// probe (§4c). One object answers both, so the login checked is the login the
    /// query runs under: a check through another connection would prove nothing.
    pub fn new<C>(connection: Arc<C>) -> Self
    where
        C: RelationProbe + RelationPrivileges + 'static,
    {
        Self {
            probe: connection.clone(),
            privileges: Some(connection),
            allow_elevated_login: false,
            label: None,
            relations: BTreeMap::new(),
        }
    }

    /// Probes run through `probe`, which can't say what its login may do: every probe
    /// is refused unless the run allows an elevated login (§4c).
    pub fn without_privileges(probe: Arc<dyn RelationProbe>) -> Self {
        Self {
            probe,
            privileges: None,
            allow_elevated_login: false,
            label: None,
            relations: BTreeMap::new(),
        }
    }

    /// Names the connection for people (e.g. "dbt target `health_ro`"), where its
    /// login isn't reported.
    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The relation each node's build made, by node id: a probe reads that relation
    /// only, and a node the connection resolves elsewhere (e.g. under another schema)
    /// is *unknown*, never judged on other data.
    #[must_use]
    pub fn expecting(mut self, relations: BTreeMap<String, String>) -> Self {
        self.relations = relations;
        self
    }

    /// Runs probes even when the login check finds more than read access, or can't
    /// tell: `--allow-elevated-login`, for one run, at the user's own risk (§4c). The
    /// SQL and trust guards still hold.
    #[must_use]
    pub fn allowing_elevated_login(mut self, allow: bool) -> Self {
        self.allow_elevated_login = allow;
        self
    }
}

impl fmt::Debug for ProbeConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProbeConnection")
            .field("probe", &self.probe.info().kind)
            .field(
                "privileges",
                &self.privileges.as_ref().map(|p| p.info().kind),
            )
            .field("allow_elevated_login", &self.allow_elevated_login)
            .field("label", &self.label)
            .field("relations", &self.relations)
            .finish()
    }
}

/// Probes that ran under a login the check found could do more than read, or couldn't
/// tell about, because the run allowed it (`--allow-elevated-login`, §4c).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ElevatedLogin {
    /// The login, as the warehouse names it, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    /// The connection, as the host names it (e.g. "dbt target `health_ro`"), when it
    /// gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// What the check found, by node id: what the login holds beyond reading, or why
    /// it couldn't tell.
    pub found: BTreeMap<String, String>,
}

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
    pass: Pass,
    statement: ProbeStatement,
    /// What its digest is made from, before [`pin`](Self::pin).
    digested: Vec<u8>,
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
    /// The connection it runs through, `[health.probes]`: a repository that points
    /// trusted probes at another target or profile has them untrusted again. Left out
    /// when not set, so a digest without one is as it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile: Option<&'a str>,
}

impl Probe {
    pub(crate) fn from_config(
        config: &DeclaredCheckConfig,
        at: &str,
        severity: Option<Severity>,
        select: Option<Selector>,
        exclude: Option<Selector>,
        connection: Option<&ods_config::HealthProbesConfig>,
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
        // Health checks see models, seeds and snapshots; a source would never be probed.
        if config
            .select
            .as_ref()
            .is_some_and(|s| s.resource_type.iter().any(|t| t == "source"))
        {
            return Err(HealthConfigError(format!(
                "{at}.select: probes read models, seeds and snapshots; sources can't be probed yet"
            )));
        }
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
            target: connection.and_then(|c| c.target.as_deref()),
            profile: connection.and_then(|c| c.profile.as_deref()),
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
            digest: format!("sha256:{}", hex::encode(Sha256::digest(&bytes))),
            digested: bytes,
            read_only: false,
            trusted: false,
        })
    }

    /// Adds `connection`, the host's settings that decide where it runs and which the
    /// project can set (e.g. the profile and program it connects with), to its digest,
    /// so changing them needs trust again (§4d). Nothing changes when there are none.
    pub(crate) fn pin(&mut self, connection: &BTreeMap<String, String>) {
        let mut bytes = self.digested.clone();
        if !connection.is_empty() {
            bytes.push(b'\n');
            bytes.extend(serde_json::to_vec(connection).unwrap_or_default());
        }
        self.digest = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
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

    pub(crate) fn applies(&self, node: &NodeFacts) -> bool {
        self.select.matches(node) && !self.exclude.as_ref().is_some_and(|s| s.matches(node))
    }

    /// What every finding of it carries: the guards it passed, and its query.
    fn evidence(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("read_only_checked".to_owned(), self.read_only.to_string()),
            ("trusted".to_owned(), self.trusted.to_string()),
            ("sql".to_owned(), self.sql.clone()),
        ])
    }

    fn finding(
        &self,
        severity: Severity,
        status: Status,
        reason: String,
        evidence: BTreeMap<String, String>,
    ) -> Finding {
        Finding {
            check: self.id.clone(),
            source: CheckSource::Probe,
            status,
            severity,
            reason,
            evidence,
        }
    }

    /// Its finding about `node` when it doesn't run there: not selected, or stopped by
    /// a guard that needs no warehouse (§4a, §4b), or with nothing to run through.
    /// `None` when it can run on `node`. A guard that stops it makes it *unknown*,
    /// saying which guard, never a pass.
    fn held(&self, severity: Severity, node: &NodeFacts, connected: bool) -> Option<Finding> {
        let reason = if !self.applies(node) {
            return Some(self.finding(
                severity,
                Status::Skipped,
                "not selected for this check".to_owned(),
                BTreeMap::new(),
            ));
        } else if !self.read_only {
            format!("{}: its SQL wasn't checked as one read-only query", self.id)
        } else if !self.trusted {
            format!(
                "{}: not trusted for this project; review it and run `ods health trust`",
                self.id
            )
        } else if !connected {
            format!(
                "{}: no connection to run probes through; set `[health.probes] target` to a target whose login can only read (ADR-0030 §4c)",
                self.id
            )
        } else {
            return None;
        };
        Some(self.finding(severity, Status::Unknown, reason, self.evidence()))
    }

    /// `nodes` split into those the build made a relation for (all of them when the
    /// host gave none) and an *unknown* finding for each of the others, which have
    /// nothing to probe: whatever the connection finds under their name isn't the
    /// build's (e.g. an ephemeral model's).
    fn built<'n>(
        &self,
        severity: Severity,
        nodes: &[&'n NodeFacts],
        connection: &ProbeConnection,
    ) -> (Vec<&'n NodeFacts>, BTreeMap<String, Finding>) {
        let (built, unbuilt): (Vec<&NodeFacts>, Vec<&NodeFacts>) = nodes.iter().partition(|n| {
            connection.relations.is_empty() || connection.relations.contains_key(&n.id)
        });
        let refused = unbuilt
            .into_iter()
            .map(|node| {
                let finding = self.finding(
                    severity,
                    Status::Unknown,
                    format!(
                        "{}: the build made no relation for it (e.g. it is ephemeral), so there is nothing to probe",
                        self.id
                    ),
                    self.evidence(),
                );
                (node.id.clone(), finding)
            })
            .collect();
        (built, refused)
    }

    /// Runs it on `nodes`, each of which passed [`held`](Self::held): checks the
    /// login on their relations (§4c), then sends the query for the ones it allows, and
    /// judges each row with `pass`. Its finding about each node, by id.
    async fn run_on(
        &self,
        severity: Severity,
        nodes: &[&NodeFacts],
        connection: &ProbeConnection,
        timeout: Duration,
        elevated: &mut Option<ElevatedLogin>,
    ) -> BTreeMap<String, Finding> {
        let (nodes, mut out) = self.built(severity, nodes, connection);
        if nodes.is_empty() {
            return out;
        }
        let targets: Vec<ProbeTarget> = nodes
            .iter()
            .map(|n| {
                let target = ProbeTarget::new(n.id.clone(), n.name.clone());
                match connection.relations.get(&n.id) {
                    Some(relation) => target.expecting(relation.clone()),
                    None => target,
                }
            })
            .collect();
        let (login, access) = login_check(connection, &targets, timeout).await;
        let who = match (&login, &connection.label) {
            (Some(login), _) => format!("the login `{login}`"),
            (None, Some(label)) => format!("the login of {label}"),
            (None, None) => "the probe login".to_owned(),
        };
        // Each target to probe, how the login check went, and what it found if overridden.
        let mut allowed: Vec<(ProbeTarget, &'static str, Option<String>)> = Vec::new();
        for target in targets {
            let access = access
                .get(&target.id)
                .cloned()
                .unwrap_or_else(|| Access::Unknown("the check didn't report on it".to_owned()));
            let found = match access {
                Access::ReadOnly => {
                    allowed.push((target, "read_only", None));
                    continue;
                }
                Access::Elevated(what) => format!("can do more than read: {}", what.join(", ")),
                Access::Unknown(why) => format!("couldn't be shown to only read: {why}"),
                _ => "couldn't be shown to only read".to_owned(),
            };
            if connection.allow_elevated_login {
                let entry = elevated.get_or_insert_with(ElevatedLogin::default);
                if entry.login.is_none() {
                    entry.login.clone_from(&login);
                }
                if entry.connection.is_none() {
                    entry.connection.clone_from(&connection.label);
                }
                entry.found.insert(target.id.clone(), found.clone());
                allowed.push((target, "overridden", Some(found)));
                continue;
            }
            let mut evidence = self.evidence();
            evidence.insert("login_check".to_owned(), "refused".to_owned());
            if let Some(login) = &login {
                evidence.insert("login".to_owned(), login.clone());
            }
            if let Some(label) = &connection.label {
                evidence.insert("connection".to_owned(), label.clone());
            }
            let reason = format!(
                "{}: refused: probes run only under a read-only login, and on this relation {} {found}. Use a read-only login for probes, or `--allow-elevated-login` at your own risk",
                self.id, who,
            );
            out.insert(
                target.id,
                self.finding(severity, Status::Unknown, reason, evidence),
            );
        }
        if allowed.is_empty() {
            return out;
        }
        let asked: Vec<ProbeTarget> = allowed.iter().map(|(t, _, _)| t.clone()).collect();
        let answered = self.send(&asked, connection, timeout).await;
        for (target, check, found) in allowed {
            let mut evidence = self.evidence();
            evidence.insert("login_check".to_owned(), check.to_owned());
            if let Some(found) = found {
                evidence.insert("login_found".to_owned(), found);
            }
            if let Some(login) = &login {
                evidence.insert("login".to_owned(), login.clone());
            }
            if let Some(label) = &connection.label {
                evidence.insert("connection".to_owned(), label.clone());
            }
            let (status, reason) = match &answered {
                Err(why) => (Status::Unknown, format!("{}: {why}", self.id)),
                Ok(answers) => self.judge(answers.get(&target.id), &mut evidence),
            };
            out.insert(target.id, self.finding(severity, status, reason, evidence));
        }
        out
    }

    /// Its verdict on one target's answer (`None` inside for one answered twice), with
    /// the row it judged added to `evidence`.
    fn judge(
        &self,
        answer: Option<&Option<ProbeAnswer>>,
        evidence: &mut BTreeMap<String, String>,
    ) -> (Status, String) {
        match answer {
            Some(Some(ProbeAnswer::Rows(rows))) => {
                let row = rows.first().cloned().unwrap_or_default();
                for (column, value) in &row {
                    evidence.insert(format!("row.{column}"), value.clone());
                }
                match self.pass.evaluate(&row) {
                    Verdict::Pass => (
                        Status::Pass,
                        format!("{}: {} holds", self.id, self.pass_text),
                    ),
                    Verdict::Fail(why) => (Status::Fail, format!("{}: {why}", self.id)),
                    Verdict::Unknown(why) => (Status::Unknown, format!("{}: {why}", self.id)),
                }
            }
            Some(Some(ProbeAnswer::Skipped(why))) => {
                (Status::Skipped, format!("{}: not probed: {why}", self.id))
            }
            Some(Some(ProbeAnswer::Unknown(why))) => {
                (Status::Unknown, format!("{}: {why}", self.id))
            }
            Some(None) => (
                Status::Unknown,
                format!("{}: the probe answered about it twice", self.id),
            ),
            Some(Some(_)) | None => (
                Status::Unknown,
                format!("{}: the probe didn't report on it", self.id),
            ),
        }
    }

    /// Sends its query for `targets`: each target's answer, or `None` for one answered
    /// twice; `Err` when nothing was read.
    async fn send(
        &self,
        targets: &[ProbeTarget],
        connection: &ProbeConnection,
        timeout: Duration,
    ) -> Result<BTreeMap<String, Option<ProbeAnswer>>, String> {
        let request = ProbeFilter::kinds(KINDS)
            .and_then(|filter| ProbeRequest::new(filter, vec![self.statement.clone()]))
            .map_err(|e| e.to_string())?
            .with_timeout(timeout);
        // The provider stops on its own time out; this one is for one that doesn't.
        let report = tokio::time::timeout(timeout, connection.probe.probe(&request, targets))
            .await
            .map_err(|_| format!("the probe didn't answer within {}", seconds(timeout)))?
            .map_err(|e| format!("the probe couldn't run: {e}"))?;
        // An answer about a relation it wasn't asked about means it ran where it
        // shouldn't have: none of its answers can be trusted.
        if let Some((stranger, _)) = report
            .targets
            .iter()
            .find(|(id, _)| !targets.iter().any(|t| t.id == *id))
        {
            return Err(format!(
                "the probe answered about `{stranger}`, which it wasn't asked about, so none of its answers count"
            ));
        }
        let mut answers: BTreeMap<String, Option<ProbeAnswer>> = BTreeMap::new();
        for (id, answer) in report.targets {
            answers
                .entry(id)
                .and_modify(|seen| *seen = None)
                .or_insert(Some(answer));
        }
        Ok(answers)
    }
}

/// `duration` for people: `30s`, `0.5s`.
fn seconds(duration: Duration) -> String {
    format!("{}s", duration.as_secs_f64())
}

/// What the connection's login may do on each of `targets`, read just before a probe
/// runs on them (§4c), and the login's name. A target it can't tell about is unknown:
/// a connection that can't report privileges, a report that can't be read, or one that
/// leaves a target out or answers it twice.
async fn login_check(
    connection: &ProbeConnection,
    targets: &[ProbeTarget],
    timeout: Duration,
) -> (Option<String>, BTreeMap<String, Access>) {
    let all = |why: String| {
        targets
            .iter()
            .map(|t| (t.id.clone(), Access::Unknown(why.clone())))
            .collect()
    };
    let Some(privileges) = &connection.privileges else {
        return (
            None,
            all("the probe connection can't report what its login may do".to_owned()),
        );
    };
    let report = match tokio::time::timeout(timeout, privileges.privileges(targets)).await {
        Ok(Ok(report)) => report,
        Ok(Err(e)) => return (None, all(format!("its privileges couldn't be read: {e}"))),
        Err(_) => {
            return (
                None,
                all(format!(
                    "reading its privileges took longer than {}",
                    seconds(timeout)
                )),
            );
        }
    };
    // An answer about a relation it wasn't asked about breaks the contract: none of its
    // answers can be trusted.
    if let Some((stranger, _)) = report
        .targets
        .iter()
        .find(|(id, _)| !targets.iter().any(|t| t.id == *id))
    {
        return (
            None,
            all(format!(
                "the check answered about `{stranger}`, which it wasn't asked about"
            )),
        );
    }
    let mut access: BTreeMap<String, Access> = BTreeMap::new();
    for (id, found) in report.targets {
        access
            .entry(id)
            .and_modify(|seen| *seen = Access::Unknown("the check answered twice".to_owned()))
            .or_insert(found);
    }
    (report.login, access)
}

/// Every enabled probe's findings about `nodes`, by node id, in probe order, the probes
/// that ran under a login the check didn't find read-only, if the run allowed it, and
/// the probes that selected nothing.
pub(crate) async fn run_all(
    probes: &[Probe],
    nodes: &[NodeFacts],
    connection: Option<&ProbeConnection>,
    timeout: Duration,
) -> ProbeRun {
    let mut findings: BTreeMap<String, Vec<Finding>> = BTreeMap::new();
    let mut elevated = None;
    let mut unmatched = Vec::new();
    for probe in probes {
        let Some(severity) = probe.severity else {
            continue;
        };
        if !nodes.iter().any(|n| probe.applies(n)) {
            unmatched.push(probe.id.clone());
        }
        let mut found: BTreeMap<String, Finding> = BTreeMap::new();
        let mut ready: Vec<&NodeFacts> = Vec::new();
        for node in nodes {
            match probe.held(severity, node, connection.is_some()) {
                Some(finding) => {
                    found.insert(node.id.clone(), finding);
                }
                None => ready.push(node),
            }
        }
        if let (Some(connection), false) = (connection, ready.is_empty()) {
            found.extend(
                probe
                    .run_on(severity, &ready, connection, timeout, &mut elevated)
                    .await,
            );
        }
        for node in nodes {
            if let Some(finding) = found.remove(&node.id) {
                findings.entry(node.id.clone()).or_default().push(finding);
            }
        }
    }
    ProbeRun {
        findings,
        elevated,
        unmatched,
    }
}

/// What [`run_all`] found.
pub(crate) struct ProbeRun {
    /// Each node's probe findings, by id, in probe order.
    pub(crate) findings: BTreeMap<String, Vec<Finding>>,
    /// Probes that ran under a login not shown to only read, as the run allowed.
    pub(crate) elevated: Option<ElevatedLogin>,
    /// Enabled probes that select no node in scope: they checked nothing.
    pub(crate) unmatched: Vec<String>,
}
