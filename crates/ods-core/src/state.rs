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
/// 1.1 adds [`StateSnapshot::target`]; 1.2 adds [`StateSnapshot::sources`].
pub const STATE_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 2);

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// ---------------------------------------------------------------------------- time

/// A point in time, to the second, in UTC, between the years -9999 and 9999. Serialized
/// as RFC 3339 (`…Z`), which reads back as the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The earliest time jiff represents: -9999-01-02T01:59:59Z.
    const MIN_UNIX: i64 = -377_705_023_201;
    /// The latest time jiff represents: 9999-12-30T22:00:00Z.
    const MAX_UNIX: i64 = 253_402_207_200;

    /// Seconds since the Unix epoch. A time outside the years -9999 to 9999 (only
    /// saturating arithmetic produces one, e.g. "never due") becomes the nearest
    /// representable time, so every `Timestamp` prints and parses back exactly.
    pub const fn from_unix(seconds: i64) -> Self {
        if seconds < Self::MIN_UNIX {
            Self(Self::MIN_UNIX)
        } else if seconds > Self::MAX_UNIX {
            Self(Self::MAX_UNIX)
        } else {
            Self(seconds)
        }
    }

    /// Seconds since the Unix epoch.
    pub const fn unix(self) -> i64 {
        self.0
    }

    /// Now, from the system clock.
    pub fn now() -> Self {
        Self(jiff::Timestamp::now().as_second())
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
        let instant = match text.parse::<jiff::Timestamp>() {
            Ok(instant) => instant,
            // dbt sometimes writes no offset; it means UTC. A date alone is not a time.
            Err(_) if !text.contains(['T', 't', ' ']) => return Err(bad()),
            Err(_) => text
                .parse::<jiff::civil::DateTime>()
                .and_then(|civil| civil.to_zoned(jiff::tz::TimeZone::UTC))
                .map_err(|_| bad())?
                .timestamp(),
        };
        // `as_second` truncates toward zero; round down so fractions before 1970 are
        // dropped the same way as after.
        Ok(Self(
            instant.as_second() - i64::from(instant.subsec_nanosecond() < 0),
        ))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `from_unix` keeps every value in jiff's range, so this always succeeds.
        let instant = jiff::Timestamp::from_second(self.0).map_err(|_| fmt::Error)?;
        write!(f, "{instant}")
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
    /// Where it was built, if known. A build is only reused in the same target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetIdentity>,
    /// Source id → the data its checks last passed on (#232). ODS doesn't build
    /// sources, but it runs their checks (e.g. dbt source tests) when their data is new.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, SourceState>,
}

/// The last time a source's checks all passed, and on which data (#232). A source is
/// an input with checks: they vouch for a version of its data, as a node's checks vouch
/// for a build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SourceState {
    /// The data version measured before the checks ran. `None` when unknown: the
    /// record then vouches for no version, and the checks run again.
    pub version: Option<DataVersion>,
    /// When the version was measured.
    pub observed_at: Option<Timestamp>,
    /// The checks that passed, and when.
    pub tested: TestRecord,
}

impl SourceState {
    /// Checks that passed on `version` of a source's data, measured at `observed_at`.
    pub fn new(
        version: Option<DataVersion>,
        observed_at: Option<Timestamp>,
        tested: TestRecord,
    ) -> Self {
        Self {
            version,
            observed_at,
            tested,
        }
    }
}

/// Which warehouse target builds went to, without anything secret: enough to tell
/// two targets apart, never enough to connect (AGENTS.md rule 9).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct TargetIdentity {
    /// The target's name, e.g. `prod`.
    pub name: String,
    /// The profile it belongs to, if the engine has profiles.
    pub profile: Option<String>,
    /// The kind of warehouse, e.g. an adapter type.
    pub kind: Option<String>,
    /// Where it is, for people: a host, account or file, with anything that could be a
    /// credential (a user, a query string) left out. Not enough to tell targets apart.
    pub location: Option<String>,
    /// A digest of where it is, in full: tells targets apart without keeping, or
    /// showing, what the location might carry.
    pub location_digest: Option<String>,
    /// The database or catalog builds go to.
    pub database: Option<String>,
}

impl TargetIdentity {
    /// A target called `name`, with nothing else known yet.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            profile: None,
            kind: None,
            location: None,
            location_digest: None,
            database: None,
        }
    }

    /// Sets the profile.
    #[must_use]
    pub fn profile(mut self, profile: Option<String>) -> Self {
        self.profile = profile;
        self
    }

    /// Sets the kind.
    #[must_use]
    pub fn kind(mut self, kind: Option<String>) -> Self {
        self.kind = kind;
        self
    }

    /// Sets the location.
    #[must_use]
    pub fn location(mut self, location: Option<String>) -> Self {
        self.location = location;
        self
    }

    /// Sets the location's digest.
    #[must_use]
    pub fn location_digest(mut self, digest: Option<String>) -> Self {
        self.location_digest = digest;
        self
    }

    /// Sets the database.
    #[must_use]
    pub fn database(mut self, database: Option<String>) -> Self {
        self.database = database;
        self
    }
}

impl fmt::Display for TargetIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        let details: Vec<&str> = [&self.profile, &self.kind, &self.location, &self.database]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        if !details.is_empty() {
            write!(f, " ({})", details.join(", "))?;
        }
        Ok(())
    }
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
            target: None,
            sources: BTreeMap::new(),
        }
    }

    /// Sets where its builds went.
    #[must_use]
    pub fn with_target(mut self, target: Option<TargetIdentity>) -> Self {
        self.target = target;
        self
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
    /// A full refresh was asked for, and rebuilds it from scratch (e.g. an incremental
    /// model).
    FullRefreshRequested,
    /// A parent is rebuilt from scratch by a full refresh, or reads one that is, so
    /// its data is rebuilt too, whatever the lag tolerance: a full refresh is how data
    /// is corrected.
    UpstreamFullRefresh,
    /// Its last build went to another target (another host, account, database or
    /// profile), or to one ODS can't identify.
    TargetChanged,
    /// It would be reused, but its relation isn't in the warehouse any more.
    RelationMissing,
    /// It would be reused, but whether its relation is still in the warehouse couldn't
    /// be checked.
    RelationUnverified,
    /// Its checks haven't passed since ODS started recording them, or failed last time.
    NotTested,
    /// Its checks were added, removed or edited since they last passed.
    ChecksChanged,
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
        assert_eq!(Timestamp::parse("2026-09-25t03:14:58z").unwrap(), t);
        assert_eq!(Timestamp::parse("2026-09-25T03:14:58+0000").unwrap(), t);
        assert_eq!(
            Timestamp::parse(" 2026-09-25T03:14:58.1-01:30 ").unwrap(),
            Timestamp::from_unix(t.unix() + 5400)
        );
        assert_eq!(
            Timestamp::parse("2026-09-25T03:14").unwrap(),
            Timestamp::from_unix(t.unix() - 58)
        );
        assert_eq!(
            Timestamp::parse("1969-12-31T23:59:59.5Z").unwrap().unix(),
            -1
        );
        assert_eq!(Timestamp::from_unix(-1).to_string(), "1969-12-31T23:59:59Z");
        assert_eq!(Timestamp::MIN_UNIX, jiff::Timestamp::MIN.as_second());
        assert_eq!(Timestamp::MAX_UNIX, jiff::Timestamp::MAX.as_second());
        for edge in [
            Timestamp::from_unix(i64::MIN),
            Timestamp::from_unix(i64::MAX),
        ] {
            let json = serde_json::to_string(&edge).unwrap();
            assert_eq!(
                serde_json::from_str::<Timestamp>(&json).unwrap(),
                edge,
                "{json}"
            );
        }
        assert_eq!(
            Timestamp::from_unix(i64::MAX).to_string(),
            "9999-12-30T22:00:00Z"
        );
        assert!(Timestamp::parse("yesterday").is_err());
        assert!(Timestamp::parse("2026-13-01T00:00:00Z").is_err());
        assert!(Timestamp::parse("2026-02-30T00:00:00Z").is_err());
        assert!(Timestamp::parse("2026-09-25").is_err());
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
            serde_json::json!({"major": 1, "minor": 2})
        );
        assert_eq!(json["parent"], 3);
        // No target or sources: left out, so older documents and ours look alike.
        assert!(json.get("target").is_none());
        assert!(json.get("sources").is_none());
        assert_eq!(
            json["nodes"]["model.p.a"]["built_at"],
            "1970-01-01T00:00:10Z"
        );
        assert_eq!(
            serde_json::from_value::<StateSnapshot>(json).unwrap(),
            snapshot
        );
        // A 1.0 document, as stored before targets were recorded, still reads: with
        // no target.
        let old = serde_json::json!({
            "schema_version": {"major": 1, "minor": 0},
            "parent": null,
            "created_at": "1970-01-01T00:00:20Z",
            "run_id": "run-0",
            "nodes": {}
        });
        let old: StateSnapshot = serde_json::from_value(old).unwrap();
        assert!(STATE_SCHEMA_VERSION.can_read(old.schema_version));
        assert_eq!(old.target, None);
        // With one, it round-trips too.
        let targeted = snapshot.with_target(Some(
            TargetIdentity::new("prod").location(Some("db.example.com".into())),
        ));
        let json = serde_json::to_value(&targeted).unwrap();
        assert_eq!(json["target"]["name"], "prod");
        assert_eq!(
            serde_json::from_value::<StateSnapshot>(json).unwrap(),
            targeted
        );
        // A 1.1 document, as stored before source checks were recorded, reads with
        // none (#232).
        let old = serde_json::json!({
            "schema_version": {"major": 1, "minor": 1},
            "parent": null,
            "created_at": "1970-01-01T00:00:20Z",
            "run_id": "run-0",
            "nodes": {},
            "target": {"name": "prod", "profile": null, "kind": null, "location": null,
                       "location_digest": null, "database": null}
        });
        let old: StateSnapshot = serde_json::from_value(old).unwrap();
        assert!(STATE_SCHEMA_VERSION.can_read(old.schema_version));
        assert!(old.sources.is_empty());
        // With source checks, it round-trips.
        let mut checked = targeted;
        checked.sources.insert(
            "source.p.raw.orders".to_owned(),
            SourceState::new(
                Some(DataVersion::new(
                    "2026-01-01T00:00:00Z",
                    Exactness::Semantic,
                    "sources.json",
                )),
                Some(Timestamp::from_unix(15)),
                TestRecord::new("run-1", Timestamp::from_unix(20), "checks"),
            ),
        );
        let json = serde_json::to_value(&checked).unwrap();
        assert_eq!(
            json["sources"]["source.p.raw.orders"]["version"]["value"],
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(
            json["sources"]["source.p.raw.orders"]["tested"]["checks"],
            "checks"
        );
        assert_eq!(
            serde_json::from_value::<StateSnapshot>(json).unwrap(),
            checked
        );
    }
}
