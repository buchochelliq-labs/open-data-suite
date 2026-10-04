//! The Impact simulator (#347): what a proposed change to some columns reaches
//! downstream, and what it would break. The view model behind `/lineage/impact` and
//! `/api/lineage/impact`.
//!
//! Nothing is run or written: the change is simulated on the analyzed column graph,
//! with the same engine as `ods lineage impact` ([`ColumnGraph::impact`]), so the
//! models it says must run are the CLI's. On top of that it says which of them would
//! fail ([`ColumnGraph::breaks`]). A model whose lineage is unknown is said to be
//! unknown, never unaffected (AGENTS rule 3).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ods_core::{ColumnRef, DirectKind, EdgeKind, IndirectKind, RelationName};
use ods_lineage::{
    Breaks, Change, ColumnChangeKind, ColumnGraph, Impact, ImpactReason, NodeImpact, NodeLineage,
};
use serde::Serialize;

use crate::catalog::CatalogInput;
use crate::dashboard::DASHBOARD_SCHEMA_VERSION;

/// At most this many changes are simulated at once; a longer query is cut, and says so.
pub const MAX_CHANGES: usize = 20;

/// The column trail stops after this many steps, and says so.
pub const MAX_TRAIL: usize = 200;

/// A proposed change to a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChangeKind {
    /// The column is renamed: the old name is removed and the new one added.
    Rename,
    /// Its type changes: its values change.
    Retype,
    /// The column is removed.
    Drop,
}

impl ChangeKind {
    /// Its key in the URL, e.g. `drop`.
    pub fn key(self) -> &'static str {
        match self {
            ChangeKind::Rename => "rename",
            ChangeKind::Retype => "retype",
            ChangeKind::Drop => "drop",
        }
    }

    /// For people, e.g. `type change`.
    pub fn label(self) -> &'static str {
        match self {
            ChangeKind::Rename => "rename",
            ChangeKind::Retype => "type change",
            ChangeKind::Drop => "drop",
        }
    }

    fn parse(key: &str) -> Option<Self> {
        match key {
            "rename" => Some(ChangeKind::Rename),
            "retype" => Some(ChangeKind::Retype),
            "drop" => Some(ChangeKind::Drop),
            _ => None,
        }
    }

    /// Every kind, in the order the form offers them.
    pub const ALL: [ChangeKind; 3] = [ChangeKind::Rename, ChangeKind::Retype, ChangeKind::Drop];
}

/// One row of the form, as asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProposedChange {
    /// The column as typed, e.g. `orders.amount`.
    pub input: String,
    /// The change; `None` until one is chosen.
    pub change: Option<ChangeKind>,
    /// The new name (rename) or type (type change), if given.
    pub to: Option<String>,
    /// The node, once found.
    pub node: Option<String>,
    /// The node's name, once found.
    pub model: Option<String>,
    /// The column as the node spells it, once found.
    pub column: Option<String>,
    /// Its type now, if the project or the warehouse catalog says.
    pub current_type: Option<String>,
    /// Why this change can't be simulated, if it can't.
    pub problem: Option<String>,
    /// Something to know about how it was simulated, e.g. that the node's columns
    /// are unknown so a change to all its rows was assumed.
    pub note: Option<String>,
}

impl ProposedChange {
    /// An empty row of the form.
    pub(crate) fn blank() -> Self {
        Self {
            input: String::new(),
            change: None,
            to: None,
            node: None,
            model: None,
            column: None,
            current_type: None,
            problem: None,
            note: None,
        }
    }
}

/// The query: the proposed changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImpactQuery {
    rows: Vec<(String, Option<String>, Option<String>)>,
    cut: bool,
    /// Whether the form asked for one more, empty row.
    pub add: bool,
}

impl ImpactQuery {
    /// From the URL's pairs: each `column` starts a row, and the `change` (or the
    /// form's `change-<row>` radio) and `to` after it belong to that row. Rows without
    /// a column are ignored. `remove=<row>` drops a row and `add` asks for an empty one,
    /// for the form's buttons.
    pub fn from_pairs(pairs: &[(String, String)]) -> Self {
        let mut query = Self::default();
        let mut remove: Option<usize> = None;
        let mut rows: Vec<(String, Option<String>, Option<String>)> = Vec::new();
        for (key, value) in pairs {
            let value = value.trim();
            match key.as_str() {
                "column" => rows.push((value.to_owned(), None, None)),
                "to" => {
                    if let Some(row) = rows.last_mut() {
                        row.2 = Some(value.to_owned()).filter(|v| !v.is_empty());
                    }
                }
                "add" => query.add = true,
                "remove" => remove = value.parse().ok(),
                // The form's radios say which row they belong to (`change-<row>`), so
                // a reordered URL still binds each change to its column; a plain
                // `change` belongs to the column before it.
                key if key == "change" || key.starts_with("change-") => {
                    let row = match key.strip_prefix("change-") {
                        Some(index) => index.parse().ok().and_then(|i: usize| rows.get_mut(i)),
                        None => rows.last_mut(),
                    };
                    if let Some(row) = row {
                        row.1 = Some(value.to_owned());
                    }
                }
                _ => {}
            }
        }
        if let Some(index) = remove.filter(|i| *i < rows.len()) {
            rows.remove(index);
        }
        rows.retain(|(column, _, _)| !column.is_empty());
        if rows.len() > MAX_CHANGES {
            rows.truncate(MAX_CHANGES);
            query.cut = true;
        }
        query.rows = rows;
        query
    }

    /// One change, e.g. from a column's link.
    pub fn one(column: &str, change: ChangeKind) -> Self {
        Self {
            rows: vec![(column.to_owned(), Some(change.key().to_owned()), None)],
            cut: false,
            add: false,
        }
    }

    /// Whether nothing was asked.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// How a node's lineage is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LineageStatus {
    /// Its SQL was analyzed.
    Parsed,
    /// Nothing is known about how it uses its inputs (e.g. a Python model).
    Opaque,
    /// It is reached only through an opaque node, so its impact is assumed.
    Inferred,
}

/// What the change does to a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Verdict {
    /// Its SQL names a column that would no longer exist: its build fails.
    Breaks,
    /// Its lineage is unknown: it may name the column or not.
    Unknown,
    /// It passes a removed column through `select *` and loses it too.
    LosesColumn,
    /// The model being changed.
    Changed,
    /// Its values or rows may change, but nothing it names goes away.
    Affected,
}

/// A node that must run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct MustRun {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// What the change does to it.
    pub verdict: Verdict,
    /// How its lineage is known.
    pub lineage: LineageStatus,
    /// Its columns the change reaches; `None` when unknown.
    pub columns: Option<Vec<String>>,
    /// Whether its rows (so every column) may change.
    pub rows_changed: bool,
    /// How it reads what changed, e.g. `aggregation`, `filter`, `select *`.
    pub how: Vec<String>,
    /// Why, for people, one sentence each.
    pub reasons: Vec<String>,
}

/// A reader of a changed model that doesn't need to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Skipped {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Why, for people.
    pub reason: String,
}

/// One step of the column trail: from a column to what it feeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct TrailStep {
    /// How many steps from a changed column (1 for its direct readers).
    pub depth: usize,
    /// `model.column`.
    pub from: String,
    /// `model.column`, or `model` when it shapes the model's rows.
    pub to: String,
    /// How, e.g. `aggregation` or `filter`.
    pub how: String,
}

/// A test on a node that must run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct TestRun {
    /// The node it tests.
    pub node: String,
    /// The test's name.
    pub name: String,
    /// The column it tests, if a column test.
    pub column: Option<String>,
}

/// What the simulated changes reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ImpactResult {
    /// The changes given to the engine, as `ods lineage impact` takes them.
    pub engine_changes: Vec<Change>,
    /// Ids of the nodes the engine says must run downstream, sorted: `ods lineage
    /// impact`'s `run` for the same changes.
    pub reached: Vec<String>,
    /// The changed models and everything reached, the ones that break first.
    pub must_run: Vec<MustRun>,
    /// Readers of a changed model that don't use what changed.
    pub skipped: Vec<Skipped>,
    /// How many other nodes aren't downstream of the change at all.
    pub not_downstream: usize,
    /// An `ods state build` of exactly what must run.
    pub selector: String,
    /// How the change travels from column to column.
    pub trail: Vec<TrailStep>,
    /// Whether the trail was cut at [`MAX_TRAIL`] steps.
    pub trail_cut: bool,
    /// Whether the build command selects exactly what must run: false when a name in
    /// it is shared by another node, so it may select more.
    pub selector_exact: bool,
    /// The tests on the nodes that must run.
    pub tests: Vec<TestRun>,
}

/// The Impact simulator's view model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ImpactView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The proposed changes, as understood.
    pub changes: Vec<ProposedChange>,
    /// Whether more than [`MAX_CHANGES`] were asked for, and the rest left out.
    pub cut: bool,
    /// What they reach; `None` when nothing was asked, or a change can't be simulated.
    pub result: Option<ImpactResult>,
    /// Every `model.column` that can be picked, sorted.
    #[serde(skip)]
    pub column_options: Vec<String>,
}

/// Builds the view for `query` on `graph`. `names` maps node ids to names; `catalog`
/// gives column types and tests.
pub fn impact_view(
    graph: &ColumnGraph,
    names: &BTreeMap<String, String>,
    catalog: &CatalogInput,
    query: &ImpactQuery,
) -> ImpactView {
    let name_of = |id: &str| {
        names
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.rsplit('.').next().unwrap_or(id).to_owned())
    };
    let changes: Vec<ProposedChange> = query
        .rows
        .iter()
        .map(|(input, change, to)| {
            propose(graph, names, catalog, input, change.as_deref(), to.as_ref())
        })
        .collect();
    let column_options = column_options(graph, &name_of);
    // A row without a change yet (e.g. opened from a column's link) waits for one.
    let result = if changes.is_empty()
        || changes
            .iter()
            .any(|c| c.problem.is_some() || c.change.is_none())
    {
        None
    } else {
        Some(simulate(graph, catalog, &changes, &name_of))
    };
    ImpactView {
        schema_version: DASHBOARD_SCHEMA_VERSION,
        changes,
        cut: query.cut,
        result,
        column_options,
    }
}

/// `model.column` for each column; a model whose name another node shares is named by
/// its id, so every option picks the node it was listed for.
fn column_options(graph: &ColumnGraph, name_of: &dyn Fn(&str) -> String) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for node in graph.nodes() {
        *counts.entry(name_of(&node.id)).or_default() += 1;
    }
    let mut options: BTreeSet<String> = BTreeSet::new();
    for node in graph.nodes() {
        let name = name_of(&node.id);
        let model = if counts.get(&name).copied().unwrap_or(0) > 1 {
            node.id.clone()
        } else {
            name
        };
        for column in &node.columns {
            options.insert(format!("{model}.{column}"));
        }
    }
    options.into_iter().collect()
}

/// Finds the node and column of one row, or says why not.
fn propose(
    graph: &ColumnGraph,
    names: &BTreeMap<String, String>,
    catalog: &CatalogInput,
    input: &str,
    change: Option<&str>,
    to: Option<&String>,
) -> ProposedChange {
    let kind = change.and_then(ChangeKind::parse);
    let mut proposed = ProposedChange {
        input: input.to_owned(),
        change: kind,
        to: to.cloned(),
        node: None,
        model: None,
        column: None,
        current_type: None,
        problem: None,
        note: None,
    };
    if let Some(other) = change.filter(|c| !c.is_empty() && kind.is_none()) {
        proposed.problem = Some(format!(
            "unknown change `{other}`; choose rename, type change or drop"
        ));
        return proposed;
    }
    let Some((model, column)) = input.rsplit_once('.') else {
        proposed.problem = Some(format!("`{input}` is not MODEL.COLUMN"));
        return proposed;
    };
    let node = match find_node(graph, names, model) {
        Ok(node) => node,
        Err(problem) => {
            proposed.problem = Some(problem);
            return proposed;
        }
    };
    proposed.node = Some(node.id.clone());
    // Links name the node by id, which is never ambiguous; the form shows the name
    // when only this node has it.
    if model == node.id
        && let Some(name) = names.get(&node.id)
        && names.values().filter(|n| *n == name).count() == 1
    {
        proposed.input = format!("{name}.{column}");
    }
    proposed.model = Some(
        names
            .get(&node.id)
            .cloned()
            .unwrap_or_else(|| model.to_owned()),
    );
    if node.columns.is_empty() {
        // As `ods lineage impact`: a change to a column of a node whose columns aren't
        // known is followed as a change to all its rows, which reaches every reader.
        proposed.column = Some(column.to_owned());
        proposed.note = Some(format!(
            "the columns of `{model}` are unknown, so this is simulated as a change to all its rows"
        ));
    } else {
        let folded: Vec<&String> = node
            .columns
            .iter()
            .filter(|c| c.as_str() == column || c.eq_ignore_ascii_case(column))
            .collect();
        let exact = node.columns.iter().find(|c| c.as_str() == column);
        match (exact, folded.as_slice()) {
            (Some(c), _) => proposed.column = Some(c.clone()),
            (None, [c]) => proposed.column = Some((*c).clone()),
            _ => {
                proposed.problem = Some(format!(
                    "`{model}` has no column `{column}`; its columns are: {}",
                    node.columns.join(", ")
                ));
                return proposed;
            }
        }
    }
    let column = proposed.column.clone().unwrap_or_default();
    proposed.current_type = catalog
        .nodes
        .iter()
        .find(|n| n.id == node.id)
        .and_then(|n| n.columns.iter().find(|c| c.name == column))
        .and_then(|c| c.data_type.as_ref())
        .map(|(t, _)| t.clone());
    if proposed.change == Some(ChangeKind::Rename) {
        match &proposed.to {
            None => proposed.problem = Some(format!("give `{column}` a new name")),
            Some(to) if to == &column => {
                proposed.problem = Some(format!("`{column}` is already called that"));
            }
            Some(to) if node.columns.iter().any(|c| c == to) => {
                proposed.problem = Some(format!("`{model}` already has a column `{to}`"));
            }
            Some(_) => {}
        }
    }
    proposed
}

/// The node by id or, failing that, by name, if only one has it.
fn find_node<'a>(
    graph: &'a ColumnGraph,
    names: &BTreeMap<String, String>,
    wanted: &str,
) -> Result<&'a NodeLineage, String> {
    if let Some(node) = graph.node(wanted) {
        return Ok(node);
    }
    let matches: Vec<&NodeLineage> = graph
        .nodes()
        .filter(|n| names.get(&n.id).is_some_and(|name| name == wanted))
        .collect();
    match matches.as_slice() {
        [node] => Ok(node),
        [] => Err(format!("no model `{wanted}`")),
        many => Err(format!(
            "`{wanted}` names {} nodes; use one of their ids: {}",
            many.len(),
            many.iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The proposed changes as the engine takes them.
struct Planned {
    changes: Vec<Change>,
    /// Columns that go away (dropped, or the old name of a rename).
    removed: Vec<ColumnRef>,
    /// Renamed columns: old → new name.
    renamed: BTreeMap<ColumnRef, String>,
    /// The nodes being changed.
    changed_nodes: BTreeSet<String>,
}

fn plan(graph: &ColumnGraph, proposed: &[ProposedChange]) -> Planned {
    let mut planned = Planned {
        changes: Vec::new(),
        removed: Vec::new(),
        renamed: BTreeMap::new(),
        changed_nodes: BTreeSet::new(),
    };
    for p in proposed {
        let (Some(id), Some(column), Some(change)) = (&p.node, &p.column, p.change) else {
            continue;
        };
        let Some(node) = graph.node(id) else {
            continue;
        };
        planned.changed_nodes.insert(id.clone());
        let relation = node.relation.clone();
        if node.columns.is_empty() {
            // Followed as a change to all its rows, as `ods lineage impact` does; a
            // drop or rename still goes to `breaks`, which makes its readers unknown.
            if change != ChangeKind::Retype {
                planned
                    .removed
                    .push(ColumnRef::new(relation.clone(), column.clone()));
            }
            planned.changes.push(Change::Rows { relation });
            continue;
        }
        let at = ColumnRef::new(relation.clone(), column.clone());
        match change {
            ChangeKind::Drop => {
                planned.changes.push(Change::Column {
                    column: at.clone(),
                    kind: ColumnChangeKind::Removed,
                });
                planned.removed.push(at);
            }
            ChangeKind::Retype => planned.changes.push(Change::Column {
                column: at,
                kind: ColumnChangeKind::Modified,
            }),
            ChangeKind::Rename => {
                let to = p.to.clone().unwrap_or_default();
                planned.changes.push(Change::Column {
                    column: at.clone(),
                    kind: ColumnChangeKind::Removed,
                });
                planned.changes.push(Change::Column {
                    column: ColumnRef::new(relation, to.clone()),
                    kind: ColumnChangeKind::Added,
                });
                planned.renamed.insert(at.clone(), to);
                planned.removed.push(at);
            }
        }
    }
    planned
}

/// What the engine found, and how to name things, for describing it.
struct Described<'a> {
    graph: &'a ColumnGraph,
    name_of: &'a dyn Fn(&str) -> String,
    planned: &'a Planned,
    breaks: Breaks,
    /// Relations of the opaque nodes the change reaches: their readers are inferred.
    opaque_reached: BTreeSet<RelationName>,
}

impl Described<'_> {
    fn relation_name(&self, relation: &RelationName) -> String {
        self.graph
            .node_for(relation)
            .map_or_else(|| relation.to_string(), |n| (self.name_of)(&n.id))
    }

    fn column_name(&self, c: &ColumnRef) -> String {
        format!("{}.{}", self.relation_name(&c.relation), c.column)
    }

    /// The row of a model being changed.
    fn changed(&self, id: &str, proposed: &[ProposedChange]) -> MustRun {
        let mine: Vec<&ProposedChange> = proposed
            .iter()
            .filter(|p| p.node.as_deref() == Some(id))
            .collect();
        let what: Vec<String> = mine
            .iter()
            .map(|p| {
                let column = p.column.clone().unwrap_or_default();
                match (p.change.unwrap_or(ChangeKind::Drop), &p.to) {
                    (ChangeKind::Rename, Some(to)) => format!("`{column}` is renamed to `{to}`"),
                    (ChangeKind::Retype, Some(to)) => format!("`{column}` becomes {to}"),
                    (ChangeKind::Retype, None) => format!("`{column}` changes type"),
                    _ => format!("`{column}` is dropped"),
                }
            })
            .collect();
        let columns: BTreeSet<String> = mine.iter().filter_map(|p| p.column.clone()).collect();
        MustRun {
            id: id.to_owned(),
            name: (self.name_of)(id),
            verdict: Verdict::Changed,
            lineage: lineage_status(self.graph, id, false),
            columns: Some(columns.into_iter().collect()),
            rows_changed: false,
            how: vec!["changed here".to_owned()],
            reasons: vec![format!("The column being changed: {}.", what.join("; "))],
        }
    }

    /// The verdict on a reached node, and the reason it gives.
    fn verdict(&self, id: &str, opaque: bool) -> (Verdict, Option<String>) {
        if let Some(columns) = self.breaks.broken.get(id) {
            let named: Vec<String> = columns
                .iter()
                .map(|c| {
                    let how = match self.planned.renamed.get(c) {
                        Some(to) => format!("renamed to `{to}`"),
                        None => "dropped".to_owned(),
                    };
                    format!("`{}` ({how})", self.column_name(c))
                })
                .collect();
            let reason = format!(
                "Its SQL names {}, which would no longer exist: its build fails until it is updated.",
                named.join(", ")
            );
            return (Verdict::Breaks, Some(reason));
        }
        if opaque {
            return (
                Verdict::Unknown,
                Some("Its lineage is unknown (e.g. a Python model or SQL that couldn't be parsed): it may use what changed, so it is assumed affected, and may break.".to_owned()),
            );
        }
        if let Some(uncertain) = self.breaks.unknown.get(id) {
            let columns: Vec<String> = uncertain
                .columns
                .iter()
                .map(|c| format!("`{}`", self.column_name(c)))
                .collect();
            let reason = if uncertain.through.is_empty() {
                format!(
                    "How it reads what changed isn't recorded: it may name {}, so it may break; it can't be told.",
                    columns.join(", ")
                )
            } else {
                let through: Vec<String> = uncertain
                    .through
                    .iter()
                    .map(|r| format!("`{}`", self.relation_name(r)))
                    .collect();
                format!(
                    "It reads {}, whose columns are unknown after this change: if {} passed on {}, this breaks too; it can't be told.",
                    through.join(", "),
                    if through.len() == 1 { "it" } else { "they" },
                    columns.join(", ")
                )
            };
            return (Verdict::Unknown, Some(reason));
        }
        if let Some(lost) = self.breaks.dropped.get(id) {
            let lost: Vec<String> = lost.iter().map(|c| format!("`{}`", c.column)).collect();
            let them = if lost.len() == 1 { "it" } else { "them" };
            let reason = format!(
                "It passes {} through `select *`, so it loses {them} too; anything that names {them} breaks.",
                lost.join(", "),
            );
            return (Verdict::LosesColumn, Some(reason));
        }
        (Verdict::Affected, None)
    }

    /// The row of a node the engine reached.
    fn reached(&self, id: &str, node: &NodeImpact) -> MustRun {
        let lineage_node = self.graph.node(id);
        let opaque = lineage_node.is_some_and(NodeLineage::is_opaque);
        let via_opaque = !opaque
            && !node.reasons.is_empty()
            && node.reasons.iter().all(|r| match r {
                ImpactReason::Rows { upstream } => self.opaque_reached.contains(upstream),
                _ => false,
            });
        let (verdict, first) = self.verdict(id, opaque);
        let mut how: BTreeSet<String> = BTreeSet::new();
        let mut reasons: Vec<String> = first.into_iter().collect();
        for reason in &node.reasons {
            let (word, sentence) = self.reason(lineage_node, reason);
            how.insert(word);
            // A verdict's own reason says what matters; the edges only explain an
            // affected node, except a name capture, which is worth knowing anyway.
            let capture = matches!(reason, ImpactReason::NameCapture { .. });
            if let Some(sentence) = sentence.filter(|_| verdict == Verdict::Affected || capture) {
                reasons.push(sentence);
            }
        }
        reasons.dedup();
        let columns = if opaque {
            None
        } else if node.rows_changed {
            lineage_node.map(|n| n.columns.clone())
        } else {
            Some(node.changed_columns.iter().cloned().collect())
        };
        MustRun {
            id: id.to_owned(),
            name: (self.name_of)(id),
            verdict,
            lineage: lineage_status(self.graph, id, via_opaque),
            columns,
            rows_changed: node.rows_changed,
            how: how.into_iter().collect(),
            reasons,
        }
    }

    /// How a node reads what changed (a word), and a sentence saying so.
    fn reason(
        &self,
        node: Option<&NodeLineage>,
        reason: &ImpactReason,
    ) -> (String, Option<String>) {
        match reason {
            ImpactReason::Column {
                upstream,
                change,
                output,
                edge,
            } => {
                let star = node
                    .and_then(|n| n.lineage.as_ref())
                    .is_some_and(|l| l.wildcard_relations.contains(&upstream.relation))
                    && output.as_deref() == Some(upstream.column.as_str())
                    && *edge == EdgeKind::Direct(DirectKind::Identity);
                let target = output
                    .as_deref()
                    .map_or_else(|| "its rows".to_owned(), |o| format!("`{o}`"));
                let sentence = format!(
                    "Reads `{}` ({}) into {target} {}.",
                    self.column_name(upstream),
                    change_word(*change),
                    if star {
                        "through `select *`"
                    } else {
                        edge_phrase(*edge)
                    },
                );
                let word = if star { "select *" } else { edge_word(*edge) };
                (word.to_owned(), Some(sentence))
            }
            ImpactReason::Rows { upstream } if self.opaque_reached.contains(upstream) => (
                "via opaque".to_owned(),
                Some(format!(
                    "Reads `{}`, whose lineage is unknown, so it is assumed affected.",
                    self.relation_name(upstream)
                )),
            ),
            ImpactReason::Rows { upstream } => (
                "rows".to_owned(),
                Some(format!(
                    "Reads `{}`, whose rows may change.",
                    self.relation_name(upstream)
                )),
            ),
            ImpactReason::Wildcard { upstream } => (
                "select *".to_owned(),
                Some(format!(
                    "Selects `*` from `{}`, so it gains `{}`.",
                    self.relation_name(&upstream.relation),
                    upstream.column
                )),
            ),
            ImpactReason::NameCapture { upstream } => (
                "name capture".to_owned(),
                Some(format!(
                    "Uses a column named `{}` from elsewhere; an unqualified reference may now bind to the new `{}`.",
                    upstream.column,
                    self.column_name(upstream)
                )),
            ),
            ImpactReason::Opaque { .. } => {
                let word = if node.is_some_and(|n| n.lineage.is_none()) {
                    "no SQL lineage"
                } else {
                    "unparsed"
                };
                (word.to_owned(), None)
            }
            _ => ("other".to_owned(), None),
        }
    }

    /// Readers of a changed model that don't use what changed, with why.
    fn skipped(&self, impact: &Impact) -> Vec<Skipped> {
        let mut skipped: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for pruned in &impact.pruned {
            if self.planned.changed_nodes.contains(&pruned.node) {
                continue;
            }
            let columns: Vec<String> = pruned
                .unused_changed_columns
                .iter()
                .map(|c| format!("`{c}`"))
                .collect();
            skipped
                .entry(pruned.node.clone())
                .or_default()
                .push(format!(
                    "Reads `{}` but not {}.",
                    self.relation_name(&pruned.upstream),
                    columns.join(", ")
                ));
        }
        skipped
            .into_iter()
            .map(|(id, reasons)| Skipped {
                name: (self.name_of)(&id),
                id,
                reason: reasons.join(" "),
            })
            .collect()
    }
}

/// Runs the engine on valid changes and describes the result.
fn simulate(
    graph: &ColumnGraph,
    catalog: &CatalogInput,
    proposed: &[ProposedChange],
    name_of: &dyn Fn(&str) -> String,
) -> ImpactResult {
    let planned = plan(graph, proposed);
    let impact = graph.impact(&planned.changes);
    let described = Described {
        graph,
        name_of,
        planned: &planned,
        breaks: graph.breaks(&planned.removed),
        opaque_reached: impact
            .nodes
            .keys()
            .filter_map(|id| graph.node(id))
            .filter(|n| n.is_opaque())
            .map(|n| n.relation.clone())
            .collect(),
    };
    let mut must_run: Vec<MustRun> = planned
        .changed_nodes
        .iter()
        .filter(|id| !impact.nodes.contains_key(*id))
        .map(|id| described.changed(id, proposed))
        .chain(
            impact
                .nodes
                .iter()
                .map(|(id, node)| described.reached(id, node)),
        )
        .collect();
    // A changed model that the change also reaches downstream (one change upstream of
    // another) keeps its verdict, and says it is changed too.
    for row in &mut must_run {
        if row.verdict != Verdict::Changed && planned.changed_nodes.contains(&row.id) {
            let changed = described.changed(&row.id, proposed);
            row.reasons.splice(0..0, changed.reasons);
            row.how.insert(0, "changed here".to_owned());
        }
    }
    must_run.sort_by(|a, b| {
        (a.verdict != Verdict::Changed, a.verdict, &a.name).cmp(&(
            b.verdict != Verdict::Changed,
            b.verdict,
            &b.name,
        ))
    });
    let skipped = described.skipped(&impact);
    let not_downstream = graph
        .nodes()
        .count()
        .saturating_sub(must_run.len() + skipped.len());
    let selected: BTreeSet<&str> = must_run.iter().map(|m| m.name.as_str()).collect();
    let mut name_counts: BTreeMap<String, usize> = BTreeMap::new();
    for node in graph.nodes() {
        *name_counts.entry(name_of(&node.id)).or_default() += 1;
    }
    let selector_exact = selected
        .iter()
        .all(|name| name_counts.get(*name).copied().unwrap_or(0) <= 1);
    // ODS's own command, which knows how to run the project's engine: it plans just
    // these nodes, and the readers left out keep their last build.
    let selector = selected
        .into_iter()
        .fold("ods state build".to_owned(), |mut command, name| {
            command.push_str(" -s ");
            command.push_str(name);
            command
        });
    let (trail, trail_cut) = trail(graph, &impact, &planned.changes, &described);
    let tests = tests(catalog, &must_run);
    ImpactResult {
        reached: impact.node_ids().map(str::to_owned).collect(),
        engine_changes: planned.changes.clone(),
        must_run,
        skipped,
        not_downstream,
        selector,
        trail,
        trail_cut,
        selector_exact,
        tests,
    }
}

/// The tests declared on the nodes that must run.
fn tests(catalog: &CatalogInput, must_run: &[MustRun]) -> Vec<TestRun> {
    let run_ids: BTreeSet<&str> = must_run.iter().map(|m| m.id.as_str()).collect();
    let mut tests: Vec<TestRun> = catalog
        .nodes
        .iter()
        .filter(|n| run_ids.contains(n.id.as_str()))
        .flat_map(|n| {
            n.tests.iter().map(|t| TestRun {
                node: n.name.clone(),
                name: t.name.clone(),
                column: t.column.clone(),
            })
        })
        .collect();
    tests.sort_by(|a, b| (&a.node, &a.column, &a.name).cmp(&(&b.node, &b.column, &b.name)));
    tests
}

fn lineage_status(graph: &ColumnGraph, id: &str, via_opaque: bool) -> LineageStatus {
    match graph.node(id) {
        Some(node) if node.is_opaque() => LineageStatus::Opaque,
        _ if via_opaque => LineageStatus::Inferred,
        _ => LineageStatus::Parsed,
    }
}

/// How the trail travels: breadth first from each changed column, one step per use,
/// and whether it was cut at [`MAX_TRAIL`] steps.
fn trail(
    graph: &ColumnGraph,
    impact: &ods_lineage::Impact,
    changes: &[Change],
    names: &Described<'_>,
) -> (Vec<TrailStep>, bool) {
    let mut steps = Vec::new();
    let mut cut = false;
    let mut queue: VecDeque<(ColumnRef, usize)> = changes
        .iter()
        .filter_map(|c| match c {
            Change::Column {
                column,
                kind: ColumnChangeKind::Removed | ColumnChangeKind::Modified,
            } => Some((column.clone(), 1)),
            _ => None,
        })
        .collect();
    let mut seen: BTreeSet<ColumnRef> = BTreeSet::new();
    while let Some((column, depth)) = queue.pop_front() {
        if !seen.insert(column.clone()) {
            continue;
        }
        if steps.len() >= MAX_TRAIL {
            cut = true;
            break;
        }
        for used in graph.uses_of(&column) {
            if !impact.nodes.contains_key(&used.node) {
                continue;
            }
            // One column can feed many readers: the cap holds within its uses too.
            if steps.len() >= MAX_TRAIL {
                cut = true;
                break;
            }
            let Some(node) = graph.node(&used.node) else {
                continue;
            };
            let to = match &used.output {
                Some(output) => {
                    let next = ColumnRef::new(node.relation.clone(), output.clone());
                    let to = names.column_name(&next);
                    queue.push_back((next, depth + 1));
                    to
                }
                None => format!("{} (its rows)", names.relation_name(&node.relation)),
            };
            steps.push(TrailStep {
                depth,
                from: names.column_name(&column),
                to,
                how: edge_word(used.edge).to_owned(),
            });
        }
    }
    (steps, cut)
}

fn change_word(kind: ColumnChangeKind) -> &'static str {
    match kind {
        ColumnChangeKind::Added => "added",
        ColumnChangeKind::Removed => "removed",
        ColumnChangeKind::Modified => "changed",
    }
}

/// How an edge reads its input, to end a sentence: `directly`, `in a filter`.
fn edge_phrase(edge: EdgeKind) -> &'static str {
    match edge {
        EdgeKind::Direct(DirectKind::Identity) => "directly",
        EdgeKind::Direct(DirectKind::Transformation) => "through a transformation",
        EdgeKind::Direct(DirectKind::Aggregation) => "through an aggregation",
        EdgeKind::Indirect(IndirectKind::Join) => "in a join",
        EdgeKind::Indirect(IndirectKind::Filter) => "in a filter",
        EdgeKind::Indirect(IndirectKind::GroupBy) => "in a GROUP BY",
        EdgeKind::Indirect(IndirectKind::Sort) => "in an ORDER BY",
        EdgeKind::Indirect(IndirectKind::Window) => "in a window",
        EdgeKind::Indirect(IndirectKind::Conditional) => "in a condition",
        _ => "",
    }
}

/// How an edge reads its input, for people.
pub(crate) fn edge_word(edge: EdgeKind) -> &'static str {
    match edge {
        EdgeKind::Direct(DirectKind::Identity) => "direct",
        EdgeKind::Direct(DirectKind::Transformation) => "transformation",
        EdgeKind::Direct(DirectKind::Aggregation) => "aggregation",
        EdgeKind::Indirect(IndirectKind::Join) => "join",
        EdgeKind::Indirect(IndirectKind::Filter) => "filter",
        EdgeKind::Indirect(IndirectKind::GroupBy) => "group by",
        EdgeKind::Indirect(IndirectKind::Sort) => "sort",
        EdgeKind::Indirect(IndirectKind::Window) => "window",
        EdgeKind::Indirect(IndirectKind::Conditional) => "condition",
        _ => "other",
    }
}
