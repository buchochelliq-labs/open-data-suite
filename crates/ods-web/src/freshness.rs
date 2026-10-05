//! Freshness evidence (#350): how ODS knows whether each of the project's inputs, its
//! sources and seeds, changed since the nodes reading it were built, and what that does
//! to the plan.
//!
//! The binary hands over the sources it knows ([`FreshnessInput`]): their current data
//! versions and where those came from. Seeds are the Catalog's own nodes. Everything
//! else comes from the same plan Home and the Catalog show, so a source's numbers here
//! are the ones `ods state explain` gives for a model reading it. Evidence that is
//! missing reads as unknown, and unknown evidence means build (AGENTS rule 3).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;

use ods_core::freshness::{FreshnessPolicy, PolicyOrigin, Quorum};
use ods_core::state::{DataVersion, Evidence, Exactness, StateSnapshot, Timestamp};
use ods_lineage::GraphDocument;
use serde::Serialize;

use crate::catalog::{Context, DecisionView, DecisionsBasis, NodeLink, short};
use crate::dashboard::{DASHBOARD_SCHEMA_VERSION, Dashboard};

// ------------------------------------------------------------------------- inputs

/// The project's sources, filled in by the binary. Seeds aren't listed here: they are
/// nodes, and come from the Catalog.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct FreshnessInput {
    /// The sources nodes read, sorted by id.
    pub sources: Vec<SourceInput>,
    /// When the sources' versions were measured, if something measured them.
    pub measured_at: Option<Timestamp>,
    /// What measured them, for people, e.g. a freshness results file; `None` when
    /// nothing did.
    pub measured_by: Option<String>,
}

impl FreshnessInput {
    /// The sources `sources`.
    pub fn new(mut sources: Vec<SourceInput>) -> Self {
        sources.sort_by(|a, b| a.id.cmp(&b.id));
        Self {
            sources,
            ..Self::default()
        }
    }

    /// Says when, and by what, the sources' versions were measured.
    #[must_use]
    pub fn measured(mut self, at: Option<Timestamp>, by: Option<String>) -> Self {
        self.measured_at = at;
        self.measured_by = by;
        self
    }
}

/// A source: what ODS knows about its data now.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceInput {
    /// Its id, as nodes name it in their dependencies.
    pub id: String,
    /// Its name, e.g. `raw.orders`.
    pub name: String,
    /// The relation it reads, if known.
    pub relation: Option<String>,
    /// How the project says its new data is measured, e.g. `max(_loaded_at)`; `None`
    /// when it says nothing, so its data can't be versioned.
    pub measured_with: Option<String>,
    /// A version a run reads from the warehouse when it starts, e.g. a table version
    /// from the table's history, which this screen can't: the server never connects to
    /// the warehouse. `None` when the warehouse offers none.
    pub read_by_runs: Option<String>,
    /// Its data version now, if anything reports one.
    pub version: Option<DataVersion>,
    /// When `version` was observed.
    pub observed_at: Option<Timestamp>,
    /// Where `version` came from, and why better sources of it weren't used, as the
    /// planner was told.
    pub version_evidence: Vec<Evidence>,
}

impl SourceInput {
    /// A source with nothing known about its data.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            relation: None,
            measured_with: None,
            read_by_runs: None,
            version: None,
            observed_at: None,
            version_evidence: Vec::new(),
        }
    }

    /// Sets the relation it reads.
    #[must_use]
    pub fn with_relation(mut self, relation: Option<String>) -> Self {
        self.relation = relation;
        self
    }

    /// Says how its new data is measured.
    #[must_use]
    pub fn measured_with(mut self, how: Option<String>) -> Self {
        self.measured_with = how;
        self
    }

    /// Says a run reads `version` (e.g. a table version) from the warehouse when it
    /// starts, which this screen doesn't.
    #[must_use]
    pub fn read_by_runs(mut self, version: Option<String>) -> Self {
        self.read_by_runs = version;
        self
    }

    /// Sets its data version now, when it was observed, and where it came from.
    #[must_use]
    pub fn with_version(
        mut self,
        version: Option<DataVersion>,
        observed_at: Option<Timestamp>,
        evidence: Vec<Evidence>,
    ) -> Self {
        self.version = version;
        self.observed_at = observed_at;
        self.version_evidence = evidence;
        self
    }
}

// ------------------------------------------------------------------------- views

/// How good a piece of evidence is, as the screen names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Grade {
    /// Identifies the exact data, e.g. a file checksum or a table version.
    Exact,
    /// Shows the data changed in the sense that matters, e.g. `max(loaded_at)`.
    Semantic,
    /// Correlated with the data, e.g. a last-modified time.
    Proxy,
    /// Derived, e.g. propagated through a view.
    Inferred,
    /// No evidence at all.
    Unknown,
}

impl Grade {
    /// The grade of `exactness`.
    pub fn of(exactness: Exactness) -> Self {
        match exactness {
            Exactness::Exact => Self::Exact,
            Exactness::Semantic => Self::Semantic,
            Exactness::Proxy => Self::Proxy,
            Exactness::Inferred => Self::Inferred,
            _ => Self::Unknown,
        }
    }

    /// Its name.
    pub fn key(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Semantic => "semantic",
            Self::Proxy => "proxy",
            Self::Inferred => "inferred",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the planner may reuse a node on evidence this good (as
    /// [`Exactness::allows_reuse`]).
    pub fn allows_reuse(self) -> bool {
        matches!(self, Self::Exact | Self::Semantic)
    }
}

/// A grade, what it is, and what it does to the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct GradeView {
    /// The grade.
    pub grade: Grade,
    /// What it is, for people.
    pub meaning: &'static str,
    /// What the planner does with it, e.g. `REUSE if equal`.
    pub plan: &'static str,
    /// Whether a node may be reused on it.
    pub allows_reuse: bool,
}

/// Every grade, best first.
fn grades() -> Vec<GradeView> {
    [
        (
            Grade::Exact,
            "Identifies the exact data: a file checksum or a table version.",
            "REUSE if equal",
        ),
        (
            Grade::Semantic,
            "Shows new data arrived, e.g. the latest load time of a source.",
            "REUSE if equal",
        ),
        (
            Grade::Proxy,
            "Correlated with the data, e.g. a last-modified time. Not enough to prove it unchanged.",
            "BUILD",
        ),
        (
            Grade::Inferred,
            "Derived indirectly. Shown as inferred, never as fact.",
            "BUILD",
        ),
        (
            Grade::Unknown,
            "No evidence at all. Nothing to compare, so readers build.",
            "BUILD",
        ),
    ]
    .into_iter()
    .map(|(grade, meaning, plan)| GradeView {
        grade,
        meaning,
        plan,
        allows_reuse: grade.allows_reuse(),
    })
    .collect()
}

/// What kind of input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InputKind {
    /// Data loaded by something else, which nodes read.
    Source,
    /// A file the project loads itself.
    Seed,
}

/// The evidence ODS has about an input now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct EvidenceView {
    /// What it is, e.g. `file checksum` or `max(_loaded_at)`.
    pub method: String,
    /// Its value now, shortened for people; `None` when unknown.
    pub value: Option<String>,
    /// How good it is.
    pub grade: Grade,
    /// When it was observed, if known.
    pub observed_at: Option<Timestamp>,
    /// Where it came from, and anything that qualifies it, as the planner was told.
    pub notes: Vec<String>,
}

/// A data version the nodes reading a source last built from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct RecordedVersion {
    /// The version, shortened for people; `None` when the build didn't know it.
    pub value: Option<String>,
    /// How good it was.
    pub grade: Grade,
    /// What it came from, e.g. a freshness results file or a table version.
    pub source: Option<String>,
    /// How many readers were built from it.
    pub readers: usize,
    /// The latest of their builds.
    pub built_at: Timestamp,
    /// That build's run, shortened.
    pub run_id: String,
}

/// A node reading an input, and what the plan decided for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ReaderView {
    /// The node.
    pub node: NodeLink,
    /// The plan's decision, and why.
    pub decision: DecisionView,
    /// Its freshness policy, for people; `None` without a plan.
    pub policy: Option<String>,
}

/// One input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct InputView {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// A source or a seed.
    pub kind: InputKind,
    /// The relation it is, or reads.
    pub relation: Option<String>,
    /// A seed's file, on loopback only.
    pub file: Option<String>,
    /// Its page, for a seed.
    pub href: Option<String>,
    /// What ODS knows about it now; `None` for a seed without a plan, whose file
    /// hasn't been compared with anything.
    pub evidence: Option<EvidenceView>,
    /// What its readers were last built from (a source), or its own last build (a
    /// seed), newest first.
    pub recorded: Vec<RecordedVersion>,
    /// What the plan says about it: for a seed, its own decision.
    pub decision: Option<DecisionView>,
    /// The nodes reading it directly, as the plan has them, and their decisions.
    pub readers: Vec<ReaderView>,
    /// Every node downstream: what a change to it can reach, its readers and theirs.
    /// Whether each rebuilds is up to its policy, so it isn't claimed here.
    pub downstream: Vec<NodeLink>,
}

/// The Freshness evidence screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct FreshnessView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The plan the decisions come from, and what qualifies it.
    pub decisions: DecisionsBasis,
    /// When the sources' versions were measured.
    pub measured_at: Option<Timestamp>,
    /// What measured them.
    pub measured_by: Option<String>,
    /// The grades, best first.
    pub grades: Vec<GradeView>,
    /// How many sources.
    pub sources: usize,
    /// How many seeds.
    pub seeds: usize,
    /// Every input: sources, then seeds, each by name.
    pub inputs: Vec<InputView>,
    /// The project in a sentence, for people.
    pub summary: String,
}

// ------------------------------------------------------------------------- building

/// `PlanEntry::policy`, for people.
fn policy_text(policy: &FreshnessPolicy) -> String {
    let lag = match policy.lag_tolerance_secs {
        0 => "rebuild on any new data".to_owned(),
        secs => format!("rebuild once new data is {} old", duration(secs)),
    };
    let quorum = match policy.require_fresh_data_from {
        Quorum::All => ", from every parent",
        _ => "",
    };
    let origin = match &policy.origin {
        PolicyOrigin::Configured { settings } => settings.join(", "),
        PolicyOrigin::FormatDefault { reason } => format!("the format's default: {reason}"),
        _ => "ODS default".to_owned(),
    };
    let reuse = if policy.allows_reuse() {
        ""
    } else {
        "; never reused: it has settings ODS can't honour"
    };
    format!("{lag}{quorum} ({origin}){reuse}")
}

/// `14400` → `4h`.
fn duration(secs: u64) -> String {
    let units = [(86_400, "d"), (3_600, "h"), (60, "m"), (1, "s")];
    let mut out = String::new();
    let mut left = secs;
    for (size, unit) in units {
        if left >= size {
            let _ = write!(out, "{}{unit}", left / size);
            left %= size;
        }
    }
    out
}

/// A version's value, short enough to read: digests are cut, like run ids.
fn value_text(value: &str) -> String {
    if value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("{}…", &value[..12])
    } else {
        value.to_owned()
    }
}

/// Each node's direct readers, by id.
type Children<'a> = BTreeMap<&'a str, Vec<&'a str>>;

/// Who reads what: the plan's dependencies when there is a plan, which skip what the
/// planner doesn't plan (an ephemeral model's readers read through it), so a reader
/// shown is one the plan decides; else the Catalog's.
fn children<'a>(cx: &Context<'a>, planned: bool) -> Children<'a> {
    if !planned {
        return cx.children.clone();
    }
    let mut children: Children<'a> = BTreeMap::new();
    for (id, entry) in &cx.entries {
        for parent in &entry.depends_on {
            children.entry(parent.as_str()).or_default().push(id);
        }
    }
    children
}

/// Everything downstream of `id`: what a change to it can reach. Whether each one
/// rebuilds is up to its policy (a lag tolerance can still reuse it).
fn downstream<'a>(children: &Children<'a>, id: &str) -> Vec<&'a str> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut queue: VecDeque<&str> = children.get(id).into_iter().flatten().copied().collect();
    while let Some(next) = queue.pop_front() {
        if seen.insert(next) {
            queue.extend(children.get(next).into_iter().flatten().copied());
        }
    }
    seen.into_iter().collect()
}

/// What a source's readers were built from, in the snapshot the plan compares against.
fn recorded_versions(snapshot: Option<&StateSnapshot>, id: &str) -> Vec<RecordedVersion> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let mut by_version: BTreeMap<Option<&DataVersion>, Vec<(&Timestamp, &str)>> = BTreeMap::new();
    for node in snapshot.nodes.values() {
        if let Some(version) = node.inputs.get(id) {
            by_version
                .entry(version.as_ref())
                .or_default()
                .push((&node.built_at, node.run_id.as_str()));
        }
    }
    let mut out: Vec<RecordedVersion> = by_version
        .into_iter()
        .filter_map(|(version, builds)| {
            let (built_at, run) = builds.iter().max()?;
            Some(RecordedVersion {
                value: version.map(|v| value_text(&v.value)),
                grade: version.map_or(Grade::Unknown, |v| Grade::of(v.exactness)),
                source: version.map(|v| v.source.clone()),
                readers: builds.len(),
                built_at: **built_at,
                run_id: short(run),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.built_at
            .cmp(&a.built_at)
            .then_with(|| a.value.cmp(&b.value))
    });
    out
}

impl Dashboard {
    /// The Freshness evidence screen as of now.
    pub fn freshness(&self, document: &GraphDocument, details: bool) -> FreshnessView {
        self.freshness_at(document, details, Timestamp::now())
    }

    /// The Freshness evidence screen as of `now`: decisions come from the plan made
    /// for `now`, as on the Catalog.
    pub fn freshness_at(
        &self,
        document: &GraphDocument,
        details: bool,
        now: Timestamp,
    ) -> FreshnessView {
        let (plan, basis) = Context::plan(self, details, now);
        let cx = Context::new(self, plan.as_ref(), basis, document, details);
        let children = children(&cx, plan.is_some());
        // The snapshot the plan compares against: what readers were last built from.
        let based_on = plan.as_ref().and_then(|p| p.based_on).map(|s| s.0);
        let snapshot = self.history().and_then(|h| {
            h.snapshots
                .iter()
                .find(|(id, _)| Some(*id) == based_on)
                .map(|(_, s)| s)
        });
        let mut sources: Vec<InputView> = self
            .freshness
            .sources
            .iter()
            .map(|source| source_view(&cx, &children, snapshot, source))
            .collect();
        sources.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        let mut seeds: Vec<InputView> = cx
            .input
            .nodes
            .iter()
            .filter(|n| n.resource_type == "seed")
            .map(|seed| seed_view(&cx, &children, snapshot, seed))
            .collect();
        seeds.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        let summary = summary(&sources, &seeds, plan.is_some());
        FreshnessView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            decisions: cx.basis.clone(),
            measured_at: self.freshness.measured_at,
            measured_by: self.freshness.measured_by.clone(),
            grades: grades(),
            sources: sources.len(),
            seeds: seeds.len(),
            inputs: sources.into_iter().chain(seeds).collect(),
            summary,
        }
    }
}

/// The nodes reading `id` directly, by name, with the plan's decisions.
fn readers(cx: &Context<'_>, children: &Children<'_>, id: &str) -> Vec<ReaderView> {
    let mut readers: Vec<ReaderView> = children
        .get(id)
        .into_iter()
        .flatten()
        .map(|reader| ReaderView {
            node: cx.link(reader),
            decision: cx.decision(reader),
            policy: cx.entries.get(reader).map(|e| policy_text(&e.policy)),
        })
        .collect();
    readers.sort_by(|a, b| a.node.name.cmp(&b.node.name));
    readers
}

/// Links to everything downstream of `id`, by name.
fn downstream_links(cx: &Context<'_>, children: &Children<'_>, id: &str) -> Vec<NodeLink> {
    let mut links: Vec<NodeLink> = downstream(children, id)
        .into_iter()
        .map(|id| cx.link(id))
        .collect();
    links.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    links
}

/// A source's row: its version now, as the planner was given it.
fn source_view(
    cx: &Context<'_>,
    children: &Children<'_>,
    snapshot: Option<&StateSnapshot>,
    source: &SourceInput,
) -> InputView {
    // A version runs read from the warehouse comes first, as the planner prefers it
    // when it is usable (ADR-0022); this screen can only say it exists.
    let method = match (&source.read_by_runs, &source.measured_with) {
        (Some(run), Some(here)) => format!("{run} (read when a run starts), else {here}"),
        (Some(run), None) => format!("{run} (read when a run starts)"),
        (None, Some(here)) => here.clone(),
        (None, None) => "nothing: the project doesn't say how its new data is measured".to_owned(),
    };
    let mut notes: Vec<String> = source
        .version
        .iter()
        .map(|v| format!("from {}", v.source))
        .chain(
            source
                .version_evidence
                .iter()
                .filter_map(|e| e.value.as_ref().map(|v| format!("{}: {v}", e.kind))),
        )
        .collect();
    if let Some(run) = &source.read_by_runs {
        notes.push(format!(
            "not read here: this screen doesn't connect to the warehouse, so its decisions don't use the {run}; a run reads it before deciding"
        ));
    }
    InputView {
        id: source.id.clone(),
        name: source.name.clone(),
        kind: InputKind::Source,
        relation: source.relation.clone(),
        file: None,
        href: None,
        evidence: Some(EvidenceView {
            method,
            value: source.version.as_ref().map(|v| value_text(&v.value)),
            grade: source
                .version
                .as_ref()
                .map_or(Grade::Unknown, |v| Grade::of(v.exactness)),
            observed_at: source.observed_at,
            notes,
        }),
        recorded: recorded_versions(snapshot, &source.id),
        decision: None,
        readers: readers(cx, children, &source.id),
        downstream: downstream_links(cx, children, &source.id),
    }
}

/// A seed's row: its file is fingerprinted like any node's code, so its own plan entry
/// carries the digest, unless the file couldn't be read.
fn seed_view(
    cx: &Context<'_>,
    children: &Children<'_>,
    snapshot: Option<&StateSnapshot>,
    seed: &crate::catalog::CatalogNode,
) -> InputView {
    let entry = cx.entries.get(seed.id.as_str());
    let fingerprint = entry.and_then(|e| {
        e.evidence
            .iter()
            .find(|ev| ev.kind == "fingerprint" && ev.subject == seed.id)
    });
    let evidence = entry.map(|e| EvidenceView {
        method: "file checksum".to_owned(),
        value: fingerprint.and_then(|f| f.value.as_deref().map(value_text)),
        grade: fingerprint.map_or(Grade::Unknown, |f| Grade::of(f.exactness)),
        observed_at: None,
        notes: if e.changed_components.is_empty() {
            Vec::new()
        } else {
            vec![format!("changed: {}", e.changed_components.join(", "))]
        },
    });
    let recorded = snapshot
        .and_then(|s| s.nodes.get(&seed.id))
        .map(|n| RecordedVersion {
            value: Some(value_text(&n.fingerprint.digest)),
            grade: Grade::Exact,
            source: Some("file checksum".to_owned()),
            readers: 1,
            built_at: n.built_at,
            run_id: short(&n.run_id),
        })
        .into_iter()
        .collect();
    InputView {
        id: seed.id.clone(),
        name: seed.name.clone(),
        kind: InputKind::Seed,
        relation: seed.relation.clone(),
        // A local path: on loopback only, as on the model page.
        file: seed.file.clone().filter(|_| cx.details),
        href: Some(crate::catalog::node_href(&seed.id)),
        evidence,
        recorded,
        decision: entry.map(|_| cx.decision(&seed.id)),
        readers: readers(cx, children, &seed.id),
        downstream: downstream_links(cx, children, &seed.id),
    }
}

/// The project in a sentence: how good its evidence is, and what the plan does with it.
fn summary(sources: &[InputView], seeds: &[InputView], planned: bool) -> String {
    let count = |n: usize, word: &str| crate::dashboard::state::count(n, word);
    if sources.is_empty() && seeds.is_empty() {
        return "The project has no sources or seeds: every node's inputs are other nodes."
            .to_owned();
    }
    if !planned {
        return format!(
            "{} and {}. Without a plan nothing is compared yet, so every reader builds.",
            count(sources.len(), "source"),
            count(seeds.len(), "seed"),
        );
    }
    let unknown = sources
        .iter()
        .filter(|s| s.evidence.as_ref().is_none_or(|e| !e.grade.allows_reuse()))
        .count();
    let reused_seeds = seeds
        .iter()
        .filter(|s| {
            s.decision
                .as_ref()
                .is_some_and(|d| d.decision == crate::catalog::Decision::Reuse)
        })
        .count();
    let mut parts = Vec::new();
    if !sources.is_empty() {
        parts.push(if unknown == 0 {
            format!(
                "Every source has evidence good enough to reuse its readers on ({}).",
                count(sources.len(), "source")
            )
        } else {
            format!(
                "{} of {} lack evidence good enough to reuse on: their readers build.",
                unknown,
                count(sources.len(), "source")
            )
        });
    }
    if !seeds.is_empty() {
        parts.push(format!(
            "{} of {} match their last build and are reused.",
            reused_seeds,
            count(seeds.len(), "seed")
        ));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies_say_what_they_do_and_where_they_came_from() {
        assert_eq!(
            policy_text(&FreshnessPolicy::conservative()),
            "rebuild on any new data (ODS default)"
        );
        let mut policy = FreshnessPolicy::conservative();
        policy.lag_tolerance_secs = 4 * 3600 + 30 * 60;
        policy.require_fresh_data_from = Quorum::All;
        policy.origin = PolicyOrigin::Configured {
            settings: vec!["state.lag_tolerance".into()],
        };
        assert_eq!(
            policy_text(&policy),
            "rebuild once new data is 4h30m old, from every parent (state.lag_tolerance)"
        );
        policy.unknown.push("state.when".into());
        assert!(policy_text(&policy).ends_with("never reused: it has settings ODS can't honour"));
    }

    #[test]
    fn only_exact_and_semantic_evidence_allows_reuse() {
        for (exactness, reuse) in [
            (Exactness::Exact, true),
            (Exactness::Semantic, true),
            (Exactness::Proxy, false),
            (Exactness::Inferred, false),
            (Exactness::None, false),
        ] {
            assert_eq!(Grade::of(exactness).allows_reuse(), reuse, "{exactness:?}");
            assert_eq!(
                Grade::of(exactness).allows_reuse(),
                exactness.allows_reuse()
            );
        }
        assert!(
            grades()
                .iter()
                .all(|g| g.allows_reuse == (g.plan != "BUILD"))
        );
    }

    #[test]
    fn digests_are_cut_and_other_values_kept() {
        assert_eq!(value_text(&"a".repeat(64)), format!("{}…", "a".repeat(12)));
        assert_eq!(value_text("2026-09-29T00:00:15Z"), "2026-09-29T00:00:15Z");
    }
}
