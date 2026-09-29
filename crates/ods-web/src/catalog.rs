//! The Catalog and the model pages (#313): what the binary hands over about the
//! project's nodes ([`CatalogInput`]), and the view models the pages and the JSON API
//! are built from ([`CatalogView`], [`ModelView`]).
//!
//! The binary reads the project's artifacts and the state store and fills a
//! [`CatalogInput`] with neutral facts (ADR-0001, ADR-0009); decisions come from the
//! same plan Home shows, made again for each request as of its time.
//! Nothing here guesses: a type or a test outcome that isn't recorded is shown as
//! unknown, and what is derived (a layer, column lineage that isn't parsed, a column
//! only the warehouse catalog lists) is marked so (AGENTS rule 3).

use std::collections::{BTreeMap, BTreeSet};

use ods_core::Confidence;
use ods_core::state::{ExecutionPlan, PlanAction, PlanEntry, ReasonCode, Timestamp};
use ods_lineage::GraphDocument;
use ods_lineage::export::{ColumnEdge, GraphNode};
use serde::Serialize;

use crate::dashboard::{DASHBOARD_SCHEMA_VERSION, Dashboard, StateInput, StateStatus};

// ------------------------------------------------------------------------- inputs

/// The project's nodes, filled in by the binary.
///
/// Shown to anyone who can reach the server: names, code and descriptions from the
/// project, never credentials.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CatalogInput {
    /// The nodes the project builds (models, seeds, snapshots, …), sorted by id.
    pub nodes: Vec<CatalogNode>,
    /// How [`CatalogNode::layer`] was worked out, for people, e.g. "the folder under
    /// the model paths". `None` when no node has a layer.
    pub layer_source: Option<String>,
    /// Names of other nodes the project's nodes read (e.g. sources), by id.
    pub names: BTreeMap<String, String>,
    /// Each node's last successful build, from the latest snapshot, by id. It must
    /// come from the same snapshot the plan is made against.
    pub last_builds: BTreeMap<String, LastBuild>,
    /// When the warehouse catalog that column types come from was written, if known.
    pub warehouse_as_of: Option<String>,
}

impl CatalogInput {
    /// A catalog of `nodes`.
    pub fn new(mut nodes: Vec<CatalogNode>) -> Self {
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        Self {
            nodes,
            ..Self::default()
        }
    }

    /// Says how layers were worked out.
    #[must_use]
    pub fn with_layer_source(mut self, source: impl Into<String>) -> Self {
        self.layer_source = Some(source.into());
        self
    }

    /// Sets the names of other nodes, e.g. sources.
    #[must_use]
    pub fn with_names(mut self, names: BTreeMap<String, String>) -> Self {
        self.names = names;
        self
    }

    /// Sets each node's last successful build.
    #[must_use]
    pub fn with_last_builds(mut self, builds: BTreeMap<String, LastBuild>) -> Self {
        self.last_builds = builds;
        self
    }

    /// Says when the warehouse catalog was written.
    #[must_use]
    pub fn with_warehouse_as_of(mut self, generated_at: Option<String>) -> Self {
        self.warehouse_as_of = generated_at;
        self
    }
}

/// A node of the project.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CatalogNode {
    /// Its id, e.g. `model.shop.orders`.
    pub id: String,
    /// Its name, e.g. `orders`.
    pub name: String,
    /// What it is: `model`, `seed`, `snapshot`, …
    pub resource_type: String,
    /// The language of its code, e.g. `sql` or `python`, if known.
    pub language: Option<String>,
    /// Its layer, only when the project says so (see [`CatalogInput::layer_source`]).
    pub layer: Option<String>,
    /// How it is materialized, e.g. `table` or `view`, if configured.
    pub materialization: Option<String>,
    /// Its tags, sorted.
    pub tags: Vec<String>,
    /// Its documented description.
    pub description: Option<String>,
    /// The relation it builds, as the project renders it.
    pub relation: Option<String>,
    /// Its file, relative to the project.
    pub file: Option<String>,
    /// Ids of the nodes it reads.
    pub depends_on: Vec<String>,
    /// Its columns, in the best order known.
    pub columns: Vec<CatalogColumn>,
    /// Its code as written.
    pub code: Option<String>,
    /// The tests on it.
    pub tests: Vec<CatalogTest>,
}

impl CatalogNode {
    /// A node with nothing known but its id, name and type.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        resource_type: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            resource_type: resource_type.into(),
            language: None,
            layer: None,
            materialization: None,
            tags: Vec::new(),
            description: None,
            relation: None,
            file: None,
            depends_on: Vec::new(),
            columns: Vec::new(),
            code: None,
            tests: Vec::new(),
        }
    }
}

/// Where a column's type comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TypeSource {
    /// Read from the warehouse's catalog when it was last documented.
    Warehouse,
    /// Declared in the project (e.g. a YAML `data_type`); not checked against the
    /// warehouse.
    Declared,
}

/// A column of a node.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CatalogColumn {
    /// Its name.
    pub name: String,
    /// Its type, only if known, and where that comes from.
    pub data_type: Option<(String, TypeSource)>,
    /// Its documented description.
    pub description: Option<String>,
    /// Its declared constraints, e.g. `not_null`, `primary_key`.
    pub constraints: Vec<String>,
    /// What lists it; empty when unknown.
    pub listed_by: Vec<ColumnSource>,
}

impl CatalogColumn {
    /// A column with nothing known but its name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            data_type: None,
            description: None,
            constraints: Vec::new(),
            listed_by: Vec::new(),
        }
    }
}

/// What says a column exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ColumnSource {
    /// Declared in the project, as of its current artifacts.
    Declared,
    /// The warehouse's catalog, as of when it was written: it may be older than the
    /// code.
    WarehouseCatalog,
    /// The data file a seed loads, as verified against the project.
    File,
}

/// What kind of test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TestKind {
    /// Checks the built data, e.g. `unique` or `not_null`.
    Data,
    /// Checks the code on fixed inputs.
    Unit,
}

/// A test on a node.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CatalogTest {
    /// Its id.
    pub id: String,
    /// Its name, e.g. `unique` or `not_null_orders_id`.
    pub name: String,
    /// The column it tests, if a column test.
    pub column: Option<String>,
    /// What kind of test.
    pub kind: TestKind,
    /// Whether it is one of the checks a recorded build's test record vouches for.
    pub covered_by_checks: bool,
}

impl CatalogTest {
    /// A test.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        column: Option<String>,
        kind: TestKind,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            column,
            kind,
            covered_by_checks: false,
        }
    }

    /// Says whether it is one of the node's checks, which a recorded build's test
    /// record vouches for as a set.
    #[must_use]
    pub fn covered(mut self, covered: bool) -> Self {
        self.covered_by_checks = covered;
        self
    }
}

/// A node's last successful build, from the latest snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LastBuild {
    /// The snapshot that first recorded it, when the history says.
    pub snapshot: Option<u64>,
    /// The run that built it.
    pub run_id: String,
    /// When the build finished.
    pub built_at: Timestamp,
    /// When its checks last all passed on this build, and in which run, if recorded.
    pub tested: Option<(String, Timestamp)>,
    /// The digest of the checks that passed then, as recorded.
    pub tested_checks: Option<String>,
    /// Whether those are the node's checks now (same digest, as the planner compares
    /// them). When not, a test added or edited since hasn't run: nothing vouches for it.
    pub checks_current: bool,
}

impl LastBuild {
    /// A build by `run_id` at `built_at`.
    pub fn new(snapshot: Option<u64>, run_id: impl Into<String>, built_at: Timestamp) -> Self {
        Self {
            snapshot,
            run_id: run_id.into(),
            built_at,
            tested: None,
            tested_checks: None,
            checks_current: false,
        }
    }

    /// Records that its checks, with digest `checks`, all passed in `run_id` at `at`;
    /// `current` says whether they are still the node's checks.
    #[must_use]
    pub fn with_tested(
        mut self,
        run_id: impl Into<String>,
        at: Timestamp,
        checks: Option<String>,
        current: bool,
    ) -> Self {
        self.tested = Some((run_id.into(), at));
        self.tested_checks = checks;
        self.checks_current = current;
        self
    }
}

// ------------------------------------------------------------------------ queries

/// The facets, in order: key (also the query parameter), label.
const FACETS: [(&str, &str); 6] = [
    ("type", "Resource type"),
    ("layer", "Layer"),
    ("materialized", "Materialization"),
    ("tag", "Tags"),
    ("decision", "Next-run decision"),
    ("lineage", "Lineage confidence"),
];

/// Columns the table sorts by.
const SORTS: [&str; 7] = [
    "name",
    "type",
    "layer",
    "materialized",
    "lineage",
    "decision",
    "last_built",
];

/// What the Catalog shows: facet filters (any selected value of a facet, every
/// facet), a name search and a sort. Read from and written to the URL query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CatalogQuery {
    /// Selected values by facet key; a facet with none selected doesn't filter.
    pub filters: BTreeMap<String, BTreeSet<String>>,
    /// Part of a name or id, if any.
    pub search: Option<String>,
    /// The column sorted by.
    pub sort: String,
    /// Whether descending.
    pub descending: bool,
}

impl CatalogQuery {
    /// From URL query pairs: `type=model&type=seed&decision=build&q=cust&sort=name&desc=1`.
    /// Unknown keys and sorts are ignored.
    pub fn from_pairs(pairs: &[(String, String)]) -> Self {
        let mut query = Self {
            sort: "name".to_owned(),
            ..Self::default()
        };
        for (key, value) in pairs {
            match key.as_str() {
                "q" => {
                    let value = value.trim();
                    if !value.is_empty() {
                        query.search = Some(value.to_owned());
                    }
                }
                "sort" if SORTS.contains(&value.as_str()) => value.clone_into(&mut query.sort),
                "desc" => query.descending = matches!(value.as_str(), "1" | "true"),
                key if FACETS.iter().any(|(k, _)| *k == key) && !value.is_empty() => {
                    query
                        .filters
                        .entry(key.to_owned())
                        .or_default()
                        .insert(value.clone());
                }
                _ => {}
            }
        }
        query
    }

    /// Back to URL query pairs, in a stable order.
    pub fn to_pairs(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for (key, _) in FACETS {
            for value in self.filters.get(key).into_iter().flatten() {
                pairs.push((key.to_owned(), value.clone()));
            }
        }
        if let Some(q) = &self.search {
            pairs.push(("q".to_owned(), q.clone()));
        }
        if self.sort != "name" {
            pairs.push(("sort".to_owned(), self.sort.clone()));
        }
        if self.descending {
            pairs.push(("desc".to_owned(), "1".to_owned()));
        }
        pairs
    }

    fn selected(&self, facet: &str, value: &str) -> bool {
        self.filters.get(facet).is_some_and(|v| v.contains(value))
    }
}

// -------------------------------------------------------------------- view models

/// The next-run decision for a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Built next run.
    Build,
    /// Its last build is kept.
    Reuse,
    /// No successful build is recorded, so it builds next run.
    NeverBuilt,
    /// Not known: the plan couldn't be made, or doesn't cover it.
    Unknown,
}

impl Decision {
    /// Its facet value.
    pub fn key(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Reuse => "reuse",
            Self::NeverBuilt => "never_built",
            Self::Unknown => "unknown",
        }
    }

    /// For people, in the facet list.
    pub fn label(self) -> &'static str {
        match self {
            Self::Build => "Build",
            Self::Reuse => "Reuse",
            Self::NeverBuilt => "Never built",
            Self::Unknown => "Unknown",
        }
    }
}

/// How well a node's column lineage is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageConfidence {
    /// Resolved from its SQL and known schemas.
    Parsed,
    /// Derived with assumptions.
    Inferred,
    /// Recorded by the platform while queries ran, possibly incomplete.
    Observed,
    /// Its SQL was read, but its lineage is unknown.
    Unknown,
    /// Its code couldn't be analyzed.
    Opaque,
    /// It has no code to analyze (e.g. a seed), or isn't in the lineage graph.
    NotApplicable,
}

impl LineageConfidence {
    /// Its facet value.
    pub fn key(self) -> &'static str {
        match self {
            Self::Parsed => "parsed",
            Self::Inferred => "inferred",
            Self::Observed => "observed",
            Self::Unknown => "unknown",
            Self::Opaque => "opaque",
            Self::NotApplicable => "n/a",
        }
    }

    fn of(node: Option<&GraphNode>) -> Self {
        match node {
            Some(n) if n.opaque => Self::Opaque,
            Some(n) => match n.confidence {
                Some(Confidence::Exact) => Self::Parsed,
                Some(Confidence::Inferred) => Self::Inferred,
                Some(Confidence::Observed) => Self::Observed,
                Some(_) => Self::Unknown,
                None => Self::NotApplicable,
            },
            None => Self::NotApplicable,
        }
    }
}

/// A node's decision, with why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct DecisionView {
    /// The decision.
    pub decision: Decision,
    /// The planner's reasons, most important first; empty without a plan.
    pub reasons: Vec<ReasonView>,
    /// Why, in a line, for people.
    pub summary: String,
}

/// One of the planner's reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ReasonView {
    /// The planner's stable code.
    pub code: ReasonCode,
    /// The code as words, e.g. `code changed`.
    pub label: String,
    /// The planner's message, with run ids shortened to eight characters.
    pub message: String,
}

/// A node's last successful build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LastBuildView {
    /// The snapshot that first recorded it, when known.
    pub snapshot: Option<u64>,
    /// The run that built it.
    pub run_id: String,
    /// Its first eight characters.
    pub short_run_id: String,
    /// When the build finished.
    pub built_at: Timestamp,
}

/// Where the Catalog's decisions come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct DecisionsBasis {
    /// Where the state is.
    pub state: StateStatus,
    /// The snapshot the plan compares against, if any.
    pub based_on: Option<u64>,
    /// Why the plan couldn't be made, if it couldn't (details on loopback only).
    pub error: Option<String>,
    /// What qualifies the plan.
    pub warnings: Vec<String>,
    /// Whether the plan checked that reused nodes' relations still exist. Always false
    /// here: the dashboard plans offline.
    pub relations_checked: bool,
    /// What the decisions don't say, e.g. that a reuse is taken on trust until a run
    /// checks the relation.
    pub caveats: Vec<String>,
}

/// A facet and its values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Facet {
    /// Its key, also its query parameter.
    pub key: &'static str,
    /// Its label.
    pub label: &'static str,
    /// How its values are worked out, when that needs saying.
    pub note: Option<String>,
    /// Its values, with how many of all the nodes have each.
    pub values: Vec<FacetValue>,
}

/// A facet value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct FacetValue {
    /// Its value in the query.
    pub value: String,
    /// For people.
    pub label: String,
    /// How many nodes have it, of all nodes (not only those shown).
    pub count: usize,
    /// Whether it is selected.
    pub selected: bool,
}

/// A row of the Catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CatalogRow {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// What it is.
    pub resource_type: String,
    /// What it is, for people, e.g. `python model`.
    pub type_label: String,
    /// Its layer, if the project says.
    pub layer: Option<String>,
    /// Its materialization, if configured.
    pub materialization: Option<String>,
    /// Its tags.
    pub tags: Vec<String>,
    /// How well its column lineage is known.
    pub lineage: LineageConfidence,
    /// Its next-run decision.
    pub decision: DecisionView,
    /// Its health; `None` until health signals exist (#117).
    pub health: Option<String>,
    /// Its last successful build; `None` when never built.
    pub last_build: Option<LastBuildView>,
    /// Its page, relative to the dashboard's root.
    pub href: String,
}

/// The Catalog: every node, with facets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CatalogView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// What is shown.
    pub query: CatalogQuery,
    /// Where the decisions come from.
    pub decisions: DecisionsBasis,
    /// How layers are worked out: they are derived, never declared.
    pub layer_source: Option<String>,
    /// The facets, in order; counts are over every node.
    pub facets: Vec<Facet>,
    /// How many nodes there are.
    pub total: usize,
    /// The nodes shown, sorted.
    pub rows: Vec<CatalogRow>,
}

/// A node another reads, or that reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeLink {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its page, relative to the dashboard's root, if it has one.
    pub href: Option<String>,
}

/// A column on the model page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ColumnView {
    /// Its name.
    pub name: String,
    /// Its type; `None` when unknown.
    pub data_type: Option<String>,
    /// Where the type comes from.
    pub type_source: Option<TypeSource>,
    /// For a warehouse type: when the warehouse catalog was written, if known.
    pub type_as_of: Option<String>,
    /// Only the warehouse catalog lists it, and neither the project nor the lineage
    /// of its current code does: it may have been dropped since.
    pub possibly_stale: bool,
    /// Its description.
    pub description: Option<String>,
    /// Tests on it, by name.
    pub tests: Vec<String>,
    /// Its declared constraints.
    pub constraints: Vec<String>,
    /// The columns it is computed from, as `node.column`, from the lineage graph.
    pub upstream: Vec<String>,
    /// Whether `upstream` is inferred rather than parsed from the code.
    pub upstream_inferred: bool,
}

/// A test on the model page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct TestView {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// The column it tests, if any.
    pub column: Option<String>,
    /// What kind of test.
    pub kind: TestKind,
    /// Its last recorded outcome: only as part of the node's checks, which the state
    /// records passing as a set; `None` when not recorded.
    pub last_outcome: Option<TestOutcome>,
}

/// A test's recorded outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct TestOutcome {
    /// Always `passed`: only passing checks are recorded.
    pub outcome: &'static str,
    /// The run that ran it.
    pub run_id: String,
    /// When.
    pub at: Timestamp,
}

/// When a node's checks last all passed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ChecksPassed {
    /// The run that ran them.
    pub run_id: String,
    /// When.
    pub at: Timestamp,
    /// Whether the checks changed since (a test added, removed or edited, by their
    /// recorded digest or the plan), so this no longer vouches for any of them.
    pub checks_changed_since: bool,
}

/// The code on the model page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CodeView {
    /// Its language, if known.
    pub language: Option<String>,
    /// As written, with its templating unresolved. Compiled code is never carried:
    /// it can hold values resolved from the environment or variables, such as
    /// credentials (AGENTS rule 9).
    pub raw: Option<String>,
}

/// Links to the other pages about a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ModelLinks {
    /// The lineage explorer, focused on it.
    pub lineage: String,
    /// Why the plan decided what it did: the Plan page's Why panel.
    pub why: String,
}

/// A model page: everything about one node, for every tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ModelView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// What it is.
    pub resource_type: String,
    /// What it is, for people.
    pub type_label: String,
    /// Its layer, if the project says.
    pub layer: Option<String>,
    /// How layers are worked out.
    pub layer_source: Option<String>,
    /// Its materialization.
    pub materialization: Option<String>,
    /// Its tags.
    pub tags: Vec<String>,
    /// Its description.
    pub description: Option<String>,
    /// The relation it builds.
    pub relation: Option<String>,
    /// Its file in the project; on loopback only.
    pub file: Option<String>,
    /// Its next-run decision, with the planner's reasons.
    pub decision: DecisionView,
    /// Where the decision comes from.
    pub decisions: DecisionsBasis,
    /// Its last successful build.
    pub last_build: Option<LastBuildView>,
    /// How well its column lineage is known.
    pub lineage: LineageConfidence,
    /// What the lineage analysis noted.
    pub lineage_notes: Vec<String>,
    /// Its columns.
    pub columns: Vec<ColumnView>,
    /// The nodes it reads, from the DAG.
    pub upstream: Vec<NodeLink>,
    /// The nodes that read it, from the DAG.
    pub downstream: Vec<NodeLink>,
    /// Its code.
    pub code: CodeView,
    /// The tests on it.
    pub tests: Vec<TestView>,
    /// When its checks last all passed on its current build, if recorded.
    pub checks_passed: Option<ChecksPassed>,
    /// Links to other pages.
    pub links: ModelLinks,
}

// ----------------------------------------------------------------------- building

/// Characters kept as they are in a node's URL segment; the rest are percent-encoded.
const SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// A value for a URL path segment or query value.
pub(crate) fn encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, SEGMENT).to_string()
}

/// A node's page, relative to the dashboard's root.
pub(crate) fn node_href(id: &str) -> String {
    format!("catalog/{}", encode(id))
}

/// `code_changed` → `code changed`.
fn words(code: ReasonCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.replace('_', " ")))
        .unwrap_or_else(|| format!("{code:?}"))
}

fn short(run_id: &str) -> String {
    run_id.chars().take(8).collect()
}

/// What a reuse doesn't say when the plan was made offline (AGENTS rules 3 and 4).
pub(crate) const REUSE_CAVEAT: &str = "Reuse is decided offline: a reused node's relation \
    isn't checked by this plan (it is when a run starts).";

/// The same, for one node.
pub(crate) const REUSE_RELATION: &str = "not checked by this plan; checked when a run starts";

/// What the catalog pages need from the dashboard, worked out once per request, with
/// everything looked up by id indexed once.
struct Context<'a> {
    input: &'a CatalogInput,
    basis: DecisionsBasis,
    details: bool,
    entries: BTreeMap<&'a str, &'a PlanEntry>,
    graph: BTreeMap<&'a str, &'a GraphNode>,
    nodes: BTreeMap<&'a str, &'a CatalogNode>,
    children: BTreeMap<&'a str, Vec<&'a str>>,
    /// Run ids to shorten in messages, longest first so none is cut by another.
    runs: Vec<&'a str>,
}

impl<'a> Context<'a> {
    fn new(
        dashboard: &'a Dashboard,
        plan: Option<&'a ExecutionPlan>,
        basis: DecisionsBasis,
        document: &'a GraphDocument,
        details: bool,
    ) -> Self {
        let input = &dashboard.catalog;
        let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for node in &input.nodes {
            for parent in &node.depends_on {
                children
                    .entry(parent.as_str())
                    .or_default()
                    .push(node.id.as_str());
            }
        }
        let mut runs: Vec<&str> = input
            .last_builds
            .values()
            .map(|b| b.run_id.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.len()));
        Self {
            input,
            basis,
            details,
            entries: plan
                .iter()
                .flat_map(|p| &p.entries)
                .map(|e| (e.node.as_str(), e))
                .collect(),
            graph: document.nodes.iter().map(|n| (n.id.as_str(), n)).collect(),
            nodes: input.nodes.iter().map(|n| (n.id.as_str(), n)).collect(),
            children,
            runs,
        }
    }

    /// The plan and what it rests on, for `now`.
    fn plan(
        dashboard: &Dashboard,
        details: bool,
        now: Timestamp,
    ) -> (Option<ExecutionPlan>, DecisionsBasis) {
        let (state, plan, error, warnings) = match &dashboard.state {
            StateInput::NoStore { .. } => (StateStatus::NoStore, None, None, Vec::new()),
            StateInput::Unreadable { .. } => (StateStatus::Unreadable, None, None, Vec::new()),
            StateInput::ProjectUnreadable { .. } => {
                (StateStatus::ProjectUnreadable, None, None, Vec::new())
            }
            StateInput::Recorded(recorded) => {
                let status = if recorded.runs.is_empty() {
                    StateStatus::NoRuns
                } else {
                    StateStatus::Recorded
                };
                match dashboard.plan_at(now) {
                    Some((Ok(plan), warnings)) => (status, Some(plan), None, warnings),
                    Some((Err(error), warnings)) => {
                        let error = if details {
                            error
                        } else {
                            "the plan couldn't be made; see the server log".to_owned()
                        };
                        (status, None, Some(error), warnings)
                    }
                    None => (status, None, None, Vec::new()),
                }
            }
        };
        let reuses = plan
            .as_ref()
            .is_some_and(|p: &ExecutionPlan| p.with_action(PlanAction::Reuse).next().is_some());
        let basis = DecisionsBasis {
            state,
            based_on: plan.as_ref().and_then(|p| p.based_on.map(|s| s.0)),
            error,
            warnings,
            relations_checked: false,
            caveats: if reuses {
                vec![REUSE_CAVEAT.to_owned()]
            } else {
                Vec::new()
            },
        };
        (plan, basis)
    }

    /// The decision for a node: the plan's, or what the state says without one.
    fn decision(&self, id: &str) -> DecisionView {
        if let Some(entry) = self.entries.get(id) {
            let reasons: Vec<ReasonView> = entry
                .reasons
                .iter()
                .map(|r| ReasonView {
                    code: r.code,
                    label: words(r.code),
                    message: self.shorten(&r.message),
                })
                .collect();
            let decision = match entry.action {
                PlanAction::Reuse => Decision::Reuse,
                _ if entry
                    .reasons
                    .iter()
                    .any(|r| r.code == ReasonCode::NeverBuilt) =>
                {
                    Decision::NeverBuilt
                }
                _ => Decision::Build,
            };
            let summary = match (reasons.first(), self.input.last_builds.get(id)) {
                // `sql changed since run 9ea38bd5`: what changed, from the plan, and
                // the run that built what is there now.
                (Some(r), Some(build))
                    if r.code == ReasonCode::CodeChanged
                        && !entry.changed_components.is_empty() =>
                {
                    format!(
                        "{} changed since run {}",
                        entry.changed_components.join(", "),
                        short(&build.run_id)
                    )
                }
                (Some(r), _) => r.message.clone(),
                // A build without a reason would be a planner bug: say so rather than
                // invent one.
                (None, _) => "no reason recorded".to_owned(),
            };
            return DecisionView {
                decision,
                reasons,
                summary,
            };
        }
        let (decision, summary) = match self.basis.state {
            StateStatus::NoStore => (
                Decision::NeverBuilt,
                "no state store yet: nothing was ever recorded, so it builds next run",
            ),
            StateStatus::NoRuns => (
                Decision::NeverBuilt,
                "no run recorded yet, so it builds next run",
            ),
            StateStatus::Unreadable => (
                Decision::Unknown,
                "the state store can't be read, so the decision isn't known",
            ),
            StateStatus::ProjectUnreadable => (
                Decision::Unknown,
                "the project can't be read, so the decision isn't known",
            ),
            StateStatus::Recorded if self.basis.error.is_some() => (
                Decision::Unknown,
                "the plan couldn't be made, so the decision isn't known",
            ),
            StateStatus::Recorded => (Decision::Unknown, "not in the plan"),
        };
        DecisionView {
            decision,
            reasons: Vec::new(),
            summary: summary.to_owned(),
        }
    }

    /// Long run ids read better short, as in the table.
    fn shorten(&self, message: &str) -> String {
        self.runs
            .iter()
            .fold(message.to_owned(), |m, run| m.replace(run, &short(run)))
    }

    fn last_build(&self, id: &str) -> Option<LastBuildView> {
        self.input.last_builds.get(id).map(|b| LastBuildView {
            snapshot: b.snapshot,
            run_id: b.run_id.clone(),
            short_run_id: short(&b.run_id),
            built_at: b.built_at,
        })
    }

    fn confidence(&self, id: &str) -> LineageConfidence {
        LineageConfidence::of(self.graph.get(id).copied())
    }

    fn name(&self, id: &str) -> String {
        self.nodes
            .get(id)
            .map(|n| n.name.clone())
            .or_else(|| self.input.names.get(id).cloned())
            .or_else(|| self.graph.get(id).map(|n| n.name.clone()))
            .unwrap_or_else(|| id.to_owned())
    }

    fn link(&self, id: &str) -> NodeLink {
        NodeLink {
            id: id.to_owned(),
            name: self.name(id),
            href: self.nodes.contains_key(id).then(|| node_href(id)),
        }
    }

    /// Links to `ids`, by name then id, once each.
    fn links<'i>(&self, ids: impl IntoIterator<Item = &'i str>) -> Vec<NodeLink> {
        let mut links: Vec<NodeLink> = ids.into_iter().map(|id| self.link(id)).collect();
        links.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        links.dedup_by(|a, b| a.id == b.id);
        links
    }

    fn row(&self, node: &CatalogNode) -> CatalogRow {
        CatalogRow {
            id: node.id.clone(),
            name: node.name.clone(),
            resource_type: node.resource_type.clone(),
            type_label: type_label(node),
            layer: node.layer.clone(),
            materialization: node.materialization.clone(),
            tags: node.tags.clone(),
            lineage: self.confidence(&node.id),
            decision: self.decision(&node.id),
            health: None,
            last_build: self.last_build(&node.id),
            href: node_href(&node.id),
        }
    }
}

/// `model` → `model`; a Python model → `python model`.
fn type_label(node: &CatalogNode) -> String {
    match node.language.as_deref() {
        Some(language) if language != "sql" && !language.is_empty() => {
            format!("{language} {}", node.resource_type)
        }
        _ => node.resource_type.clone(),
    }
}

/// A row's value for a facet: none, one or several (tags).
fn facet_values(row: &CatalogRow, facet: &str) -> Vec<String> {
    match facet {
        "type" => vec![row.resource_type.clone()],
        "layer" => row.layer.iter().cloned().collect(),
        "materialized" => row.materialization.iter().cloned().collect(),
        "tag" => row.tags.clone(),
        "decision" => vec![row.decision.decision.key().to_owned()],
        "lineage" => vec![row.lineage.key().to_owned()],
        _ => Vec::new(),
    }
}

fn matches(row: &CatalogRow, query: &CatalogQuery) -> bool {
    let facets = query.filters.iter().all(|(facet, wanted)| {
        wanted.is_empty()
            || facet_values(row, facet)
                .iter()
                .any(|value| wanted.contains(value))
    });
    let search = query.search.as_ref().is_none_or(|q| {
        let q = q.to_lowercase();
        row.name.to_lowercase().contains(&q) || row.id.to_lowercase().contains(&q)
    });
    facets && search
}

/// A sort key: missing values last either way.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum SortKey {
    Rank(usize),
    Text(String),
}

fn sort(rows: &mut [CatalogRow], query: &CatalogQuery) {
    let key = |row: &CatalogRow| -> Option<SortKey> {
        match query.sort.as_str() {
            "type" => Some(SortKey::Text(row.type_label.clone())),
            "layer" => row.layer.clone().map(SortKey::Text),
            "materialized" => row.materialization.clone().map(SortKey::Text),
            // Decisions and confidences in the facets' order, not alphabetically.
            "lineage" => CONFIDENCES
                .iter()
                .position(|c| *c == row.lineage)
                .map(SortKey::Rank),
            "decision" => DECISIONS
                .iter()
                .position(|d| *d == row.decision.decision)
                .map(SortKey::Rank),
            "last_built" => row
                .last_build
                .as_ref()
                .map(|b| SortKey::Text(b.built_at.to_string())),
            _ => Some(SortKey::Text(row.name.clone())),
        }
    };
    rows.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        let order = match (&ka, &kb) {
            (Some(x), Some(y)) if query.descending => y.cmp(x),
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        order
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Every decision, in the order the facet lists them.
const DECISIONS: [Decision; 4] = [
    Decision::Build,
    Decision::Reuse,
    Decision::NeverBuilt,
    Decision::Unknown,
];

/// Every confidence, strongest first.
const CONFIDENCES: [LineageConfidence; 6] = [
    LineageConfidence::Parsed,
    LineageConfidence::Inferred,
    LineageConfidence::Observed,
    LineageConfidence::Unknown,
    LineageConfidence::Opaque,
    LineageConfidence::NotApplicable,
];

/// The order of a facet's values: fixed for decisions and confidences (all listed,
/// zeros too), layers upstream first (by the shallowest node in the DAG), seeds last
/// among materializations, the rest alphabetical.
fn facet_order(
    cx: &Context<'_>,
    all: &[CatalogRow],
    key: &str,
    counts: &BTreeMap<String, usize>,
) -> Vec<(String, String)> {
    match key {
        "decision" => DECISIONS
            .iter()
            .map(|d| (d.key().to_owned(), d.label().to_owned()))
            .collect(),
        "lineage" => CONFIDENCES
            .iter()
            .map(|c| (c.key().to_owned(), c.key().to_owned()))
            .collect(),
        "layer" => {
            let depths = dag_depths(cx);
            let mut depth: BTreeMap<&str, usize> = BTreeMap::new();
            for row in all {
                if let Some(layer) = &row.layer {
                    let d = depths.get(row.id.as_str()).copied().unwrap_or(usize::MAX);
                    let e = depth.entry(layer.as_str()).or_insert(usize::MAX);
                    *e = (*e).min(d);
                }
            }
            let mut layers: Vec<(&str, usize)> = depth.into_iter().collect();
            layers.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(b.0)));
            layers
                .into_iter()
                .map(|(l, _)| (l.to_owned(), l.to_owned()))
                .collect()
        }
        _ => {
            let mut values: Vec<&String> = counts.keys().collect();
            if key == "materialized" {
                values.sort_by_key(|v| (v.as_str() == "seed", v.as_str()));
            }
            values.into_iter().map(|v| (v.clone(), v.clone())).collect()
        }
    }
}

/// Each node's depth in the build graph: 0 when it reads no other node here, else one
/// more than its deepest parent. From the project's own edges, so a node whose
/// lineage couldn't be analyzed still has its place.
fn dag_depths<'a>(cx: &Context<'a>) -> BTreeMap<&'a str, usize> {
    fn depth<'a>(
        id: &'a str,
        cx: &Context<'a>,
        memo: &mut BTreeMap<&'a str, usize>,
        visiting: &mut BTreeSet<&'a str>,
    ) -> usize {
        if let Some(d) = memo.get(id) {
            return *d;
        }
        // A cycle would be a broken project; stop rather than loop.
        if !visiting.insert(id) {
            return 0;
        }
        let d = cx.nodes.get(id).map_or(0, |node| {
            node.depends_on
                .iter()
                .filter_map(|p| cx.nodes.get_key_value(p.as_str()).map(|(k, _)| *k))
                .map(|p| depth(p, cx, memo, visiting) + 1)
                .max()
                .unwrap_or(0)
        });
        visiting.remove(id);
        memo.insert(id, d);
        d
    }
    let mut memo = BTreeMap::new();
    let mut visiting = BTreeSet::new();
    for id in cx.nodes.keys() {
        depth(id, cx, &mut memo, &mut visiting);
    }
    memo
}

/// The facets, with their values' counts over every node in `all`.
fn facets(cx: &Context<'_>, all: &[CatalogRow], query: &CatalogQuery) -> Vec<Facet> {
    FACETS
        .iter()
        .map(|&(key, label)| {
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for row in all {
                for value in facet_values(row, key) {
                    *counts.entry(value).or_default() += 1;
                }
            }
            let mut values: Vec<FacetValue> = facet_order(cx, all, key, &counts)
                .into_iter()
                .map(|(value, label)| FacetValue {
                    count: counts.get(&value).copied().unwrap_or(0),
                    selected: query.selected(key, &value),
                    value,
                    label,
                })
                .collect();
            // A selected value no node has any more still shows, to be cleared.
            for wanted in query.filters.get(key).into_iter().flatten() {
                if !values.iter().any(|v| &v.value == wanted) {
                    values.push(FacetValue {
                        value: wanted.clone(),
                        label: wanted.clone(),
                        count: 0,
                        selected: true,
                    });
                }
            }
            Facet {
                key,
                label,
                note: match key {
                    "layer" => Some(
                        cx.input
                            .layer_source
                            .clone()
                            .unwrap_or_else(|| "No layers: the project doesn't say.".to_owned()),
                    ),
                    "lineage" => {
                        Some("Column lineage from the SQL; seeds have none (n/a).".to_owned())
                    }
                    "decision" => Some(format!(
                        "From the plan against the latest snapshot, made offline. {REUSE_CAVEAT}"
                    )),
                    _ => None,
                },
                values,
            }
        })
        .collect()
}

impl Dashboard {
    /// The Catalog as of now. `details` shows error text (on loopback only).
    pub fn catalog(
        &self,
        document: &GraphDocument,
        query: &CatalogQuery,
        details: bool,
    ) -> CatalogView {
        self.catalog_at(document, query, details, Timestamp::now())
    }

    /// The Catalog as of `now`: decisions come from the plan made for `now`.
    pub fn catalog_at(
        &self,
        document: &GraphDocument,
        query: &CatalogQuery,
        details: bool,
        now: Timestamp,
    ) -> CatalogView {
        let (plan, basis) = Context::plan(self, details, now);
        let cx = Context::new(self, plan.as_ref(), basis, document, details);
        let all: Vec<CatalogRow> = cx.input.nodes.iter().map(|n| cx.row(n)).collect();
        let facets = facets(&cx, &all, query);
        let mut rows: Vec<CatalogRow> = all.into_iter().filter(|r| matches(r, query)).collect();
        sort(&mut rows, query);
        CatalogView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            query: query.clone(),
            decisions: cx.basis.clone(),
            layer_source: cx.input.layer_source.clone(),
            facets,
            total: cx.input.nodes.len(),
            rows,
        }
    }

    /// The page of node `id` as of now, or `None` if there is no such node.
    pub fn model(&self, document: &GraphDocument, id: &str, details: bool) -> Option<ModelView> {
        self.model_at(document, id, details, Timestamp::now())
    }

    /// The page of node `id` as of `now`.
    pub fn model_at(
        &self,
        document: &GraphDocument,
        id: &str,
        details: bool,
        now: Timestamp,
    ) -> Option<ModelView> {
        let node = self.catalog.nodes.iter().find(|n| n.id == id)?;
        let (plan, basis) = Context::plan(self, details, now);
        let cx = Context::new(self, plan.as_ref(), basis, document, details);
        let decision = cx.decision(id);
        let graph_node = cx.graph.get(id).copied();
        let lineage = cx.confidence(id);
        let columns = column_views(&cx, node, document, lineage);
        let upstream = cx.links(node.depends_on.iter().map(String::as_str));
        let downstream = cx.links(cx.children.get(id).into_iter().flatten().copied());
        let checks_changed = decision
            .reasons
            .iter()
            .any(|r| r.code == ReasonCode::ChecksChanged);
        let checks_passed = cx.input.last_builds.get(id).and_then(|b| {
            b.tested.as_ref().map(|(run_id, at)| ChecksPassed {
                run_id: run_id.clone(),
                at: *at,
                checks_changed_since: checks_changed || !b.checks_current,
            })
        });
        // Each test the record vouches for passed with the rest of the checks; the
        // others' outcomes aren't recorded.
        let vouched = checks_passed.as_ref().filter(|c| !c.checks_changed_since);
        let encoded = encode(id);
        Some(ModelView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            id: node.id.clone(),
            name: node.name.clone(),
            resource_type: node.resource_type.clone(),
            type_label: type_label(node),
            layer: node.layer.clone(),
            layer_source: cx.input.layer_source.clone(),
            materialization: node.materialization.clone(),
            tags: node.tags.clone(),
            description: node.description.clone(),
            relation: node.relation.clone(),
            file: node.file.clone().filter(|_| cx.details),
            decision,
            decisions: cx.basis.clone(),
            last_build: cx.last_build(id),
            lineage,
            lineage_notes: graph_node
                .map(|n| n.diagnostics.clone())
                .unwrap_or_default(),
            columns,
            upstream,
            downstream,
            code: CodeView {
                language: node.language.clone(),
                raw: node.code.clone().filter(|c| !c.is_empty()),
            },
            tests: node
                .tests
                .iter()
                .map(|t| TestView {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    column: t.column.clone(),
                    kind: t.kind,
                    last_outcome: vouched
                        .filter(|_| t.covered_by_checks)
                        .map(|c| TestOutcome {
                            outcome: "passed",
                            run_id: c.run_id.clone(),
                            at: c.at,
                        }),
                })
                .collect(),
            checks_passed,
            links: ModelLinks {
                lineage: format!("lineage?node={encoded}"),
                why: format!("state/plan?node={encoded}"),
            },
        })
    }
}

/// The columns of `node`, whose lineage confidence is `lineage`.
fn column_views(
    cx: &Context<'_>,
    node: &CatalogNode,
    document: &GraphDocument,
    lineage: LineageConfidence,
) -> Vec<ColumnView> {
    let id = node.id.as_str();
    let graph_node = cx.graph.get(id).copied();
    // Column lineage is fact only when parsed from the code (AGENTS rule 3).
    let upstream_inferred = lineage != LineageConfidence::Parsed;
    // By lower-cased name: a warehouse catalog may fold case (`CUSTOMER_ID`) where
    // the analyzer keeps the code's (`customer_id`).
    let mut edges: BTreeMap<String, Vec<&ColumnEdge>> = BTreeMap::new();
    for edge in document.column_edges.iter().filter(|e| e.to.node == id) {
        if let Some(column) = edge.to.column.as_deref() {
            edges.entry(column.to_lowercase()).or_default().push(edge);
        }
    }
    let code_columns: BTreeSet<String> = graph_node
        .map(|n| n.columns.iter().map(|c| c.to_lowercase()).collect())
        .unwrap_or_default();
    node.columns
        .iter()
        .map(|c| {
            let warehouse = c
                .data_type
                .as_ref()
                .is_some_and(|(_, s)| *s == TypeSource::Warehouse);
            ColumnView {
                name: c.name.clone(),
                data_type: c.data_type.as_ref().map(|(t, _)| t.clone()),
                type_source: c.data_type.as_ref().map(|(_, s)| *s),
                type_as_of: cx.input.warehouse_as_of.clone().filter(|_| warehouse),
                possibly_stale: c.listed_by == [ColumnSource::WarehouseCatalog]
                    && !code_columns.contains(&c.name.to_lowercase()),
                description: c.description.clone(),
                tests: node
                    .tests
                    .iter()
                    .filter(|t| {
                        t.column
                            .as_deref()
                            .is_some_and(|col| col.eq_ignore_ascii_case(&c.name))
                    })
                    .map(|t| t.name.clone())
                    .collect(),
                constraints: c.constraints.clone(),
                upstream: column_inputs(cx, id, edges.get(&c.name.to_lowercase())),
                upstream_inferred,
            }
        })
        .collect()
}

/// The columns a column is computed from (its incoming `edges`): `node.column` for
/// another node's, the bare name for the node's own.
fn column_inputs(cx: &Context<'_>, id: &str, edges: Option<&Vec<&ColumnEdge>>) -> Vec<String> {
    let mut inputs: Vec<String> = edges
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let from = e.from.column.as_deref()?;
            Some(if e.from.node == id {
                from.to_owned()
            } else {
                format!("{}.{from}", cx.name(&e.from.node))
            })
        })
        .collect();
    inputs.sort();
    inputs.dedup();
    inputs
}
