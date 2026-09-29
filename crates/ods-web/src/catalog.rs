//! The Catalog and the model pages (#313): what the binary hands over about the
//! project's nodes ([`CatalogInput`]), and the view models the pages and the JSON API
//! are built from ([`CatalogView`], [`ModelView`]).
//!
//! The binary reads the project's artifacts and the state store and fills a
//! [`CatalogInput`] with neutral facts (ADR-0001, ADR-0009); decisions come from the
//! same plan Home shows, made again for every request. Nothing here guesses: a type,
//! a layer or a test outcome that isn't recorded is shown as unknown (AGENTS rule 3).

use std::collections::{BTreeMap, BTreeSet};

use ods_core::Confidence;
use ods_core::state::{ExecutionPlan, PlanAction, PlanEntry, ReasonCode, Timestamp};
use ods_lineage::GraphDocument;
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
    /// Each node's last successful build, from the latest snapshot, by id.
    pub last_builds: BTreeMap<String, LastBuild>,
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
    /// Its code as compiled, only if the artifacts carry it.
    pub compiled_code: Option<String>,
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
            compiled_code: None,
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
}

impl CatalogColumn {
    /// A column with nothing known but its name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            data_type: None,
            description: None,
            constraints: Vec::new(),
        }
    }
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
        }
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
}

impl LastBuild {
    /// A build by `run_id` at `built_at`.
    pub fn new(snapshot: Option<u64>, run_id: impl Into<String>, built_at: Timestamp) -> Self {
        Self {
            snapshot,
            run_id: run_id.into(),
            built_at,
            tested: None,
        }
    }

    /// Records that its checks all passed in `run_id` at `at`.
    #[must_use]
    pub fn with_tested(mut self, run_id: impl Into<String>, at: Timestamp) -> Self {
        self.tested = Some((run_id.into(), at));
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

    fn of(document: &GraphDocument, id: &str) -> Self {
        match document.nodes.iter().find(|n| n.id == id) {
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
    /// Its description.
    pub description: Option<String>,
    /// Tests on it, by name.
    pub tests: Vec<String>,
    /// Its declared constraints.
    pub constraints: Vec<String>,
    /// The columns it is computed from, as `node.column`, from the lineage graph.
    pub upstream: Vec<String>,
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
    /// Its own last outcome; always `None` until outcomes are recorded per test.
    pub last_outcome: Option<String>,
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
    /// Whether the plan says the checks changed since, so this no longer vouches for
    /// them.
    pub checks_changed_since: bool,
}

/// The code on the model page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CodeView {
    /// Its language, if known.
    pub language: Option<String>,
    /// As written.
    pub raw: Option<String>,
    /// As compiled, only if the artifacts carry it.
    pub compiled: Option<String>,
}

/// Links to the other pages about a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ModelLinks {
    /// The lineage explorer, focused on it.
    pub lineage: String,
    /// Why the plan decided what it did.
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

/// What the catalog pages need from the dashboard, worked out once per request.
struct Context<'a> {
    input: &'a CatalogInput,
    document: &'a GraphDocument,
    basis: DecisionsBasis,
    plan: Option<ExecutionPlan>,
    details: bool,
}

impl<'a> Context<'a> {
    fn new(
        dashboard: &'a Dashboard,
        document: &'a GraphDocument,
        details: bool,
        now: Timestamp,
    ) -> Self {
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
        Self {
            input: &dashboard.catalog,
            document,
            basis: DecisionsBasis {
                state,
                based_on: plan.as_ref().and_then(|p| p.based_on.map(|s| s.0)),
                error,
                warnings,
            },
            plan,
            details,
        }
    }

    fn entry(&self, id: &str) -> Option<&PlanEntry> {
        self.plan.as_ref()?.entries.iter().find(|e| e.node == id)
    }

    /// The decision for a node: the plan's, or what the state says without one.
    fn decision(&self, id: &str) -> DecisionView {
        if let Some(entry) = self.entry(id) {
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
            let summary = reasons.first().map_or_else(
                // A build without a reason would be a planner bug: say so rather than
                // invent one.
                || "no reason recorded".to_owned(),
                |r| r.message.clone(),
            );
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
        let runs: BTreeSet<&str> = self
            .input
            .last_builds
            .values()
            .map(|b| b.run_id.as_str())
            .collect();
        runs.into_iter()
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

    fn name(&self, id: &str) -> String {
        self.input
            .nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.name.clone())
            .or_else(|| self.input.names.get(id).cloned())
            .or_else(|| {
                self.document
                    .nodes
                    .iter()
                    .find(|n| n.id == id)
                    .map(|n| n.name.clone())
            })
            .unwrap_or_else(|| id.to_owned())
    }

    fn link(&self, id: &str) -> NodeLink {
        NodeLink {
            id: id.to_owned(),
            name: self.name(id),
            href: self
                .input
                .nodes
                .iter()
                .any(|n| n.id == id)
                .then(|| node_href(id)),
        }
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
            lineage: LineageConfidence::of(self.document, &node.id),
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

fn sort(rows: &mut [CatalogRow], query: &CatalogQuery) {
    // Missing values sort last either way, then by name, so the order is total.
    let key = |row: &CatalogRow| -> (bool, String) {
        let value = match query.sort.as_str() {
            "type" => Some(row.type_label.clone()),
            "layer" => row.layer.clone(),
            "materialized" => row.materialization.clone(),
            "lineage" => Some(row.lineage.key().to_owned()),
            "decision" => Some(row.decision.decision.key().to_owned()),
            "last_built" => row.last_build.as_ref().map(|b| b.built_at.to_string()),
            _ => Some(row.name.clone()),
        };
        (value.is_none(), value.unwrap_or_default())
    };
    rows.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        let order = ka.1.cmp(&kb.1);
        let order = if query.descending {
            order.reverse()
        } else {
            order
        };
        ka.0.cmp(&kb.0)
            .then(order)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
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
            // The decisions always list every value, so a zero says "none", not
            // "not shown"; confidences list those present, strongest first.
            let fixed: Option<Vec<(&str, &str)>> = match key {
                "decision" => Some(
                    [
                        Decision::Build,
                        Decision::Reuse,
                        Decision::NeverBuilt,
                        Decision::Unknown,
                    ]
                    .iter()
                    .map(|d| (d.key(), d.label()))
                    .collect(),
                ),
                "lineage" => Some(
                    [
                        LineageConfidence::Parsed,
                        LineageConfidence::Inferred,
                        LineageConfidence::Observed,
                        LineageConfidence::Unknown,
                        LineageConfidence::Opaque,
                        LineageConfidence::NotApplicable,
                    ]
                    .iter()
                    .map(|c| (c.key(), c.key()))
                    .filter(|(key, _)| counts.contains_key(*key))
                    .collect(),
                ),
                _ => None,
            };
            let mut values: Vec<FacetValue> = match fixed {
                Some(fixed) => fixed
                    .into_iter()
                    .map(|(value, label)| FacetValue {
                        value: value.to_owned(),
                        label: label.to_owned(),
                        count: counts.get(value).copied().unwrap_or(0),
                        selected: false,
                    })
                    .collect(),
                None => counts
                    .iter()
                    .map(|(value, count)| FacetValue {
                        value: value.clone(),
                        label: value.clone(),
                        count: *count,
                        selected: false,
                    })
                    .collect(),
            };
            // A selected value no node has any more still shows, to be cleared.
            for wanted in query.filters.get(key).into_iter().flatten() {
                if !values.iter().any(|v| &v.value == wanted) {
                    values.push(FacetValue {
                        value: wanted.clone(),
                        label: wanted.clone(),
                        count: 0,
                        selected: false,
                    });
                }
            }
            for value in &mut values {
                value.selected = query.selected(key, &value.value);
            }
            Facet {
                key,
                label,
                note: match key {
                    "layer" => Some(
                        cx.input
                            .layer_source
                            .clone()
                            .unwrap_or_else(|| "no layers: the project doesn't say".to_owned()),
                    ),
                    "lineage" => {
                        Some("Column lineage from the SQL; seeds have none (n/a).".to_owned())
                    }
                    "decision" => Some(
                        "From the plan against the latest snapshot, made for this request."
                            .to_owned(),
                    ),
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
        let cx = Context::new(self, document, details, now);
        let all: Vec<CatalogRow> = cx.input.nodes.iter().map(|n| cx.row(n)).collect();
        let facets = facets(&cx, &all, query);
        let mut rows: Vec<CatalogRow> = all.into_iter().filter(|r| matches(r, query)).collect();
        sort(&mut rows, query);
        CatalogView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            query: query.clone(),
            decisions: cx.basis.clone(),
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
        let cx = Context::new(self, document, details, now);
        let decision = cx.decision(id);
        let graph_node = document.nodes.iter().find(|n| n.id == id);
        let columns = node
            .columns
            .iter()
            .map(|c| ColumnView {
                name: c.name.clone(),
                data_type: c.data_type.as_ref().map(|(t, _)| t.clone()),
                type_source: c.data_type.as_ref().map(|(_, s)| *s),
                description: c.description.clone(),
                tests: node
                    .tests
                    .iter()
                    .filter(|t| t.column.as_deref() == Some(c.name.as_str()))
                    .map(|t| t.name.clone())
                    .collect(),
                constraints: c.constraints.clone(),
                upstream: column_inputs(&cx, id, &c.name),
            })
            .collect();
        let downstream: Vec<NodeLink> = cx
            .input
            .nodes
            .iter()
            .filter(|n| n.depends_on.iter().any(|d| d == id))
            .map(|n| cx.link(&n.id))
            .collect();
        let mut upstream: Vec<NodeLink> = node.depends_on.iter().map(|d| cx.link(d)).collect();
        upstream.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        upstream.dedup_by(|a, b| a.id == b.id);
        let checks_changed = decision
            .reasons
            .iter()
            .any(|r| r.code == ReasonCode::ChecksChanged);
        let checks_passed = cx
            .input
            .last_builds
            .get(id)
            .and_then(|b| b.tested.as_ref())
            .map(|(run_id, at)| ChecksPassed {
                run_id: run_id.clone(),
                at: *at,
                checks_changed_since: checks_changed,
            });
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
            lineage: LineageConfidence::of(document, id),
            lineage_notes: graph_node
                .map(|n| n.diagnostics.clone())
                .unwrap_or_default(),
            columns,
            upstream,
            downstream,
            code: CodeView {
                language: node.language.clone(),
                raw: node.code.clone().filter(|c| !c.is_empty()),
                compiled: node.compiled_code.clone().filter(|c| !c.is_empty()),
            },
            tests: node
                .tests
                .iter()
                .map(|t| TestView {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    column: t.column.clone(),
                    kind: t.kind,
                    last_outcome: None,
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

/// The columns `column` of node `id` is computed from, from the lineage graph: `node.column`
/// for another node's, the bare name for the node's own.
fn column_inputs(cx: &Context<'_>, id: &str, column: &str) -> Vec<String> {
    let mut inputs: Vec<String> = cx
        .document
        .column_edges
        .iter()
        .filter(|e| e.to.node == id && e.to.column.as_deref() == Some(column))
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
