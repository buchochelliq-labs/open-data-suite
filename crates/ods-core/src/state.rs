//! The State domain model (#11, #13, #16, #20; ADR-0013).
//!
//! These types are persisted by state stores and returned by the planner, so they are
//! provider-neutral and versioned ([`STATE_SCHEMA_VERSION`]):
//! - a [`StateSnapshot`] records what was last built successfully, per node;
//! - a [`Fingerprint`] identifies a node's code as named, separately hashed components;
//! - [`Evidence`] says what a decision rests on and how exact that is;
//! - an [`ExecutionPlan`] says, per node, whether to build or reuse it, and why.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::SchemaVersion;
use crate::freshness::FreshnessPolicy;

/// Version of [`StateSnapshot`] and [`ExecutionPlan`] documents.
pub const STATE_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

// ---------------------------------------------------------------------------- time

/// A point in time, to the second, in UTC. Serialized as RFC 3339 (`…Z`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// Seconds since the Unix epoch.
    pub const fn from_unix(seconds: i64) -> Self {
        Self(seconds)
    }

    /// Seconds since the Unix epoch.
    pub const fn unix(self) -> i64 {
        self.0
    }

    /// Now, from the system clock.
    pub fn now() -> Self {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        Self(secs)
    }

    /// Parses RFC 3339 and the variants dbt writes: `2026-09-25T03:14:58.849275Z`,
    /// `2026-09-25 03:14:58+00:00`, or no offset at all (taken as UTC). Fractions of a
    /// second are dropped.
    ///
    /// # Errors
    /// Returns a reason if the text isn't such a timestamp.
    pub fn parse(text: &str) -> Result<Self, String> {
        let bad = || format!("`{text}` is not an RFC 3339 timestamp");
        let text = text.trim();
        let (date, rest) = text.split_once(['T', 't', ' ']).ok_or_else(bad)?;
        let number = |part: &str| part.parse::<i64>().map_err(|_| bad());
        let parts: Vec<&str> = date.split('-').collect();
        let [year, month, day] = parts.as_slice() else {
            return Err(bad());
        };
        let (year, month, day) = (number(year)?, number(month)?, number(day)?);
        // Offset: `Z`, `+HH:MM`, `-HH:MM`, or none.
        let (clock, offset_secs) = if let Some(clock) = rest.strip_suffix(['Z', 'z']) {
            (clock, 0)
        } else if let Some(at) = rest.rfind(['+', '-']).filter(|&at| at >= 8) {
            let (clock, offset) = rest.split_at(at);
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let (h, m) = offset[1..].split_once(':').unwrap_or((&offset[1..], "0"));
            (clock, sign * (number(h)? * 3600 + number(m)? * 60))
        } else {
            (rest, 0)
        };
        let clock = clock.split('.').next().unwrap_or(clock);
        let fields: Vec<&str> = clock.split(':').collect();
        let (hour, minute, second) = match fields.as_slice() {
            [hour, minute] => (number(hour)?, number(minute)?, 0),
            [hour, minute, second] => (number(hour)?, number(minute)?, number(second)?),
            _ => return Err(bad()),
        };
        if !(1..=12).contains(&month)
            || !(1..=31).contains(&day)
            || hour > 23
            || minute > 59
            || second > 60
        {
            return Err(bad());
        }
        let days = days_from_civil(year, month, day);
        Ok(Self(
            days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs,
        ))
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day number from [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (days, secs) = (self.0.div_euclid(86_400), self.0.rem_euclid(86_400));
        let (y, m, d) = civil_from_days(days);
        write!(
            f,
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            secs / 3600,
            secs % 3600 / 60,
            secs % 60
        )
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------- fingerprints

/// A node's code identity: named components, each a SHA-256 digest, and one digest over
/// all of them (#13). Components are kept so a change can be explained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Fingerprint {
    /// SHA-256 over `name\0digest\n` for each component, in name order.
    pub digest: String,
    /// Component name → SHA-256 of its canonical content.
    pub components: BTreeMap<String, String>,
    /// Component name → SHA-256 of its content *before* canonicalisation, for
    /// components that ignore formatting (#209). Not part of [`digest`](Self::digest):
    /// it only lets a plan say that a reused node's text changed in form alone.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cosmetic: BTreeMap<String, String>,
}

impl Fingerprint {
    /// The component that names the scheme a provider fingerprinted with. When a
    /// provider changes how it fingerprints, this component changes, so no node is
    /// reused across the change (and the plan can say why).
    pub const SCHEME: &'static str = "scheme";

    /// A fingerprint from components' canonical content, which is hashed here.
    pub fn from_content<I, K, V>(components: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: AsRef<[u8]>,
    {
        Self::from_digests(
            components
                .into_iter()
                .map(|(name, content)| (name.into(), sha256_hex(content.as_ref())))
                .collect(),
        )
    }

    /// A fingerprint from already hashed components.
    pub fn from_digests(components: BTreeMap<String, String>) -> Self {
        let mut canonical = String::new();
        for (name, digest) in &components {
            canonical.push_str(name);
            canonical.push('\0');
            canonical.push_str(digest);
            canonical.push('\n');
        }
        Self {
            digest: sha256_hex(canonical.as_bytes()),
            components,
            cosmetic: BTreeMap::new(),
        }
    }

    /// Records the raw content of a component that ignores formatting.
    #[must_use]
    pub fn with_cosmetic(mut self, name: impl Into<String>, raw: impl AsRef<[u8]>) -> Self {
        self.cosmetic.insert(name.into(), sha256_hex(raw.as_ref()));
        self
    }

    /// Components whose raw content changed since `before` while the fingerprint
    /// didn't: formatting-only changes. Only components both recorded are compared.
    pub fn cosmetic_changes(&self, before: &Fingerprint) -> Vec<String> {
        self.cosmetic
            .iter()
            .filter(|(name, raw)| before.cosmetic.get(*name).is_some_and(|b| b != *raw))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// What differs from `before`: components changed, added and removed.
    pub fn diff(&self, before: &Fingerprint) -> FingerprintDiff {
        let names: BTreeSet<&String> = self
            .components
            .keys()
            .chain(before.components.keys())
            .collect();
        let mut diff = FingerprintDiff::default();
        for name in names {
            match (before.components.get(name), self.components.get(name)) {
                (Some(a), Some(b)) if a != b => diff.changed.push(name.clone()),
                (None, Some(_)) => diff.added.push(name.clone()),
                (Some(_), None) => diff.removed.push(name.clone()),
                _ => {}
            }
        }
        diff
    }
}

/// How two fingerprints differ, by component name (sorted).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FingerprintDiff {
    /// In both, with different content.
    pub changed: Vec<String>,
    /// Only in the newer fingerprint.
    pub added: Vec<String>,
    /// Only in the older fingerprint.
    pub removed: Vec<String>,
}

impl FingerprintDiff {
    /// Whether the fingerprints are the same.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.added.is_empty() && self.removed.is_empty()
    }

    /// Every differing component, sorted.
    pub fn all(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .changed
            .iter()
            .chain(&self.added)
            .chain(&self.removed)
            .cloned()
            .collect();
        all.sort();
        all
    }
}

// ---------------------------------------------------------------------------- evidence

/// How directly a piece of evidence shows what it claims (research §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Exactness {
    /// No evidence: the fact is unknown.
    None,
    /// Derived, e.g. propagated through a view.
    Inferred,
    /// Correlated but not the thing itself, e.g. a last-modified timestamp.
    Proxy,
    /// Shows the data changed in the sense that matters, e.g. `max(loaded_at)`.
    Semantic,
    /// Identifies the exact data, e.g. a table version.
    Exact,
}

impl Exactness {
    /// Whether this is good enough to reuse a node on: `semantic` or `exact`.
    pub fn allows_reuse(self) -> bool {
        self >= Exactness::Semantic
    }
}

/// One fact a decision rests on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Evidence {
    /// What kind of fact, e.g. `fingerprint`, `source_loaded_at`, `relation_exists`.
    pub kind: String,
    /// What it is about, e.g. a node id.
    pub subject: String,
    /// Its value, if there is one.
    pub value: Option<String>,
    /// How exact it is.
    pub exactness: Exactness,
}

impl Evidence {
    /// A piece of evidence.
    pub fn new(
        kind: impl Into<String>,
        subject: impl Into<String>,
        value: Option<String>,
        exactness: Exactness,
    ) -> Self {
        Self {
            kind: kind.into(),
            subject: subject.into(),
            value,
            exactness,
        }
    }
}

/// A version of a source's data: equal versions mean no new data.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct DataVersion {
    /// The version, e.g. `max(loaded_at)` or a table version.
    pub value: String,
    /// How exact it is.
    pub exactness: Exactness,
    /// Where it came from, e.g. `sources.json`.
    pub source: String,
}

impl DataVersion {
    /// A data version.
    pub fn new(value: impl Into<String>, exactness: Exactness, source: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            exactness,
            source: source.into(),
        }
    }
}

// ---------------------------------------------------------------------------- snapshots

/// Identifies a committed snapshot within a store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotId(pub u64);

impl fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What ODS knows about a node's last successful build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeState {
    /// Its code when it was built.
    pub fingerprint: Fingerprint,
    /// When the build finished.
    pub built_at: Timestamp,
    /// The run that built it.
    pub run_id: String,
    /// The version of each upstream source's data it saw; `None` when unknown.
    pub inputs: BTreeMap<String, Option<DataVersion>>,
    /// For each upstream node, the run whose build of it this node read. Compared with
    /// the parent's current run instead of clocks, which can disagree across machines.
    #[serde(default)]
    pub parents: BTreeMap<String, String>,
    /// The last time this build's checks (e.g. dbt tests) all passed. `None` when they
    /// haven't run since it was built, or failed (#220).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested: Option<TestRecord>,
}

/// Checks that passed on a node's build (#220).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct TestRecord {
    /// The run that ran the checks: the build itself, or a later test run.
    pub run_id: String,
    /// When they finished.
    pub at: Timestamp,
    /// The [digest](Fingerprint::digest) of the checks that passed, as the provider
    /// fingerprints the node's set of checks. When the checks change (one is added,
    /// removed or edited), the node is no longer tested. `None` in records written before
    /// it existed, which therefore don't count as tested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<String>,
}

impl TestRecord {
    /// A record that the checks with digest `checks` passed.
    pub fn new(run_id: impl Into<String>, at: Timestamp, checks: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            at,
            checks: Some(checks.into()),
        }
    }
}

impl NodeState {
    /// Whether its current build passed the checks whose digest is `checks` (the
    /// node's checks now). Never when the node has no checks, or they can't be
    /// identified: nothing then says the build is valid.
    pub fn is_tested_with(&self, checks: Option<&str>) -> bool {
        checks.is_some_and(|c| {
            self.tested
                .as_ref()
                .is_some_and(|t| t.checks.as_deref() == Some(c))
        })
    }
}

impl NodeState {
    /// A node's state after a successful build.
    pub fn new(
        fingerprint: Fingerprint,
        built_at: Timestamp,
        run_id: impl Into<String>,
        inputs: BTreeMap<String, Option<DataVersion>>,
    ) -> Self {
        Self {
            fingerprint,
            built_at,
            run_id: run_id.into(),
            inputs,
            parents: BTreeMap::new(),
            tested: None,
        }
    }
}

/// The last successful state of every node in a scope, as of one run (#11). Immutable
/// once committed: the next run commits a new snapshot whose `parent` is this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct StateSnapshot {
    /// Document version ([`STATE_SCHEMA_VERSION`]).
    pub schema_version: SchemaVersion,
    /// The snapshot this one follows, if any.
    pub parent: Option<SnapshotId>,
    /// When it was committed.
    pub created_at: Timestamp,
    /// The run it records.
    pub run_id: String,
    /// Node id → last successful build.
    pub nodes: BTreeMap<String, NodeState>,
}

impl StateSnapshot {
    /// A snapshot following `parent`.
    pub fn new(
        parent: Option<SnapshotId>,
        created_at: Timestamp,
        run_id: impl Into<String>,
        nodes: BTreeMap<String, NodeState>,
    ) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            parent,
            created_at,
            run_id: run_id.into(),
            nodes,
        }
    }
}

// ---------------------------------------------------------------------------- plans

/// What to do with a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanAction {
    /// Run it.
    Build,
    /// Keep its last successful build.
    Reuse,
}

/// Why a node gets its action. Codes are stable for machines; messages are for people.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReasonCode {
    /// No successful build recorded.
    NeverBuilt,
    /// Its code can't be fingerprinted completely.
    CodeEvidenceIncomplete,
    /// Its fingerprint changed.
    CodeChanged,
    /// A parent is being built because its code changed.
    UpstreamCodeChanged,
    /// It depends on something ODS doesn't know, or a parent is built for that reason.
    UnknownDependency,
    /// Its policy has settings ODS can't honour, so it is never reused.
    PolicyBlocksReuse,
    /// Upstream data is newer than what it was built from.
    NewUpstreamData,
    /// There is new data, but the node is within its lag tolerance.
    WithinLagTolerance,
    /// Some parents have new data, but the policy needs all of them to.
    QuorumNotMet,
    /// A source it reads has no usable data version.
    MissingDataEvidence,
    /// Code and inputs are the same as when it was last built.
    Unchanged,
}

/// One reason for a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Reason {
    /// Stable code.
    pub code: ReasonCode,
    /// Explanation.
    pub message: String,
}

impl Reason {
    /// A reason.
    pub fn new(code: ReasonCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The decision for one node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PlanEntry {
    /// Node id.
    pub node: String,
    /// Display name.
    pub name: String,
    /// What it is, e.g. `model`, `seed`, `snapshot`.
    pub kind: String,
    /// Build or reuse.
    pub action: PlanAction,
    /// Why, most important first.
    pub reasons: Vec<Reason>,
    /// What the decision rests on.
    pub evidence: Vec<Evidence>,
    /// Planned parents (nodes and sources) it depends on.
    pub depends_on: Vec<String>,
    /// Fingerprint digest at the last successful build.
    pub before: Option<String>,
    /// Fingerprint digest now.
    pub after: Option<String>,
    /// Components that differ between `before` and `after`.
    pub changed_components: Vec<String>,
    /// The freshness policy applied.
    pub policy: FreshnessPolicy,
    /// Distance from the roots of the DAG; entries are ordered by it.
    pub depth: u32,
}

impl PlanEntry {
    /// An entry; the evidence, dependency and fingerprint fields start empty.
    pub fn new(
        node: impl Into<String>,
        name: impl Into<String>,
        kind: impl Into<String>,
        action: PlanAction,
        reasons: Vec<Reason>,
        policy: FreshnessPolicy,
        depth: u32,
    ) -> Self {
        Self {
            node: node.into(),
            name: name.into(),
            kind: kind.into(),
            action,
            reasons,
            evidence: Vec::new(),
            depends_on: Vec::new(),
            before: None,
            after: None,
            changed_components: Vec::new(),
            policy,
            depth,
        }
    }
}

/// What to build and what to reuse (#20).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ExecutionPlan {
    /// Document version ([`STATE_SCHEMA_VERSION`]).
    pub schema_version: SchemaVersion,
    /// The snapshot the plan compares against, if any.
    pub based_on: Option<SnapshotId>,
    /// When the plan was made.
    pub created_at: Timestamp,
    /// Every selected node, by depth then id.
    pub entries: Vec<PlanEntry>,
}

impl ExecutionPlan {
    /// A plan.
    pub fn new(
        based_on: Option<SnapshotId>,
        created_at: Timestamp,
        entries: Vec<PlanEntry>,
    ) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            based_on,
            created_at,
            entries,
        }
    }

    /// Entries with this action, in plan order.
    pub fn with_action(&self, action: PlanAction) -> impl Iterator<Item = &PlanEntry> {
        self.entries.iter().filter(move |e| e.action == action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosmetic_digests_explain_but_never_change_a_fingerprint() {
        let base = Fingerprint::from_content([("sql", "select 1")]);
        let a = base.clone().with_cosmetic("sql", "select 1");
        let b = base.clone().with_cosmetic("sql", "SELECT 1");
        assert_eq!(a.digest, b.digest);
        assert!(a.diff(&b).is_empty());
        assert_eq!(b.cosmetic_changes(&a), ["sql"]);
        // Snapshots recorded before cosmetic digests existed claim nothing.
        assert!(b.cosmetic_changes(&base).is_empty());
        // Old documents without the field still read, and write back unchanged.
        let json = serde_json::to_string(&base).unwrap();
        assert!(!json.contains("cosmetic"));
        assert_eq!(serde_json::from_str::<Fingerprint>(&json).unwrap(), base);
    }

    #[test]
    fn timestamps_parse_dbt_formats_and_round_trip() {
        let t = Timestamp::parse("2026-09-25T03:14:58.849275Z").unwrap();
        assert_eq!(t.to_string(), "2026-09-25T03:14:58Z");
        assert_eq!(Timestamp::parse("2026-09-25 03:14:58+00:00").unwrap(), t);
        assert_eq!(Timestamp::parse("2026-09-25T05:14:58+02:00").unwrap(), t);
        assert_eq!(Timestamp::parse("2026-09-25T03:14:58").unwrap(), t);
        assert_eq!(Timestamp::parse("1970-01-01T00:00:00Z").unwrap().unix(), 0);
        assert_eq!(
            Timestamp::from_unix(951_782_400).to_string(),
            "2000-02-29T00:00:00Z"
        );
        assert!(Timestamp::parse("yesterday").is_err());
        assert!(Timestamp::parse("2026-13-01T00:00:00Z").is_err());
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Timestamp>(&json).unwrap(), t);
    }

    #[test]
    fn fingerprints_are_canonical_and_explain_their_differences() {
        let a = Fingerprint::from_content([("file", "select 1"), ("config", "{}")]);
        let b = Fingerprint::from_content([("config", "{}"), ("file", "select 1")]);
        assert_eq!(a, b, "component order doesn't matter");
        assert_eq!(a.digest.len(), 64);
        let c = Fingerprint::from_content([("file", "select 2"), ("macros", "m")]);
        let diff = c.diff(&a);
        assert_eq!(diff.changed, ["file"]);
        assert_eq!(diff.added, ["macros"]);
        assert_eq!(diff.removed, ["config"]);
        assert_eq!(diff.all(), ["config", "file", "macros"]);
        assert!(a.diff(&b).is_empty());
    }

    #[test]
    fn only_semantic_or_exact_evidence_allows_reuse() {
        assert!(!Exactness::None.allows_reuse());
        assert!(!Exactness::Proxy.allows_reuse());
        assert!(Exactness::Semantic.allows_reuse());
        assert!(Exactness::Exact.allows_reuse());
    }

    #[test]
    fn snapshots_round_trip_with_their_schema_version() {
        let node = NodeState::new(
            Fingerprint::from_content([("file", "x")]),
            Timestamp::from_unix(10),
            "run-1",
            BTreeMap::from([
                (
                    "source.p.raw.orders".to_owned(),
                    Some(DataVersion::new(
                        "2026-01-01T00:00:00Z",
                        Exactness::Semantic,
                        "sources.json",
                    )),
                ),
                ("source.p.raw.users".to_owned(), None),
            ]),
        );
        let snapshot = StateSnapshot::new(
            Some(SnapshotId(3)),
            Timestamp::from_unix(20),
            "run-1",
            BTreeMap::from([("model.p.a".to_owned(), node)]),
        );
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            json["schema_version"],
            serde_json::json!({"major": 1, "minor": 0})
        );
        assert_eq!(json["parent"], 3);
        assert_eq!(
            json["nodes"]["model.p.a"]["built_at"],
            "1970-01-01T00:00:10Z"
        );
        assert_eq!(
            serde_json::from_value::<StateSnapshot>(json).unwrap(),
            snapshot
        );
    }
}
