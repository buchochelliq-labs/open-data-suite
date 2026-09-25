//! Observed lineage: checking static analysis against what a platform recorded, and
//! filling in nodes the analyzer can't read (e.g. Python models).

use std::collections::{BTreeMap, BTreeSet};

use ods_core::{ColumnRef, Confidence, DirectKind, EdgeKind, IndirectKind, RelationName};
use ods_sdk::contracts::observed_lineage::ObservedLineage;
use ods_sdk::contracts::sql_lineage::{OutputColumn, QueryLineage};
use serde::Serialize;

use crate::graph::ColumnGraph;
use crate::project::NodeKind;

/// How a model's static lineage compares with what was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Agreement {
    /// Every observed edge was predicted, and every predicted edge was observed.
    Agrees,
    /// Every observed edge was predicted; some predicted edges weren't seen, which is
    /// expected when not every path ran.
    Covers,
    /// Some observed edges were not predicted: the analyzer missed something.
    Misses,
    /// Its SQL couldn't be analyzed, but the platform recorded lineage for it: a
    /// candidate for [`ColumnGraph::with_observed`].
    OpaqueObserved,
    /// Nothing was observed for it (never ran within retention, or not recorded).
    NotObserved,
}

/// One observed edge that static analysis didn't predict, or the reverse.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EdgeDiff {
    /// The upstream column.
    pub source: ColumnRef,
    /// The model's output column.
    pub output: String,
}

/// One model's comparison.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ModelComparison {
    /// The node id.
    pub node: String,
    /// The verdict.
    pub agreement: Agreement,
    /// Observed edges predicted as `Direct`.
    pub matched: usize,
    /// Observed edges whose source is a row-shaping (`Indirect`) input of the model:
    /// platforms differ on whether they report these, so they count as agreement.
    pub matched_indirect: usize,
    /// Observed, not predicted.
    pub missing: Vec<EdgeDiff>,
    /// Predicted `Direct`, not observed.
    pub unobserved: Vec<EdgeDiff>,
    /// Share of predicted direct edges that were observed.
    pub precision: Option<f64>,
    /// Share of observed edges that were predicted (directly or as row inputs).
    pub recall: Option<f64>,
}

/// The result of [`ColumnGraph::compare_observed`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Comparison {
    /// One entry per model, by id.
    pub models: Vec<ModelComparison>,
    /// Totals over models with observed lineage.
    pub matched: usize,
    /// See [`ModelComparison::matched_indirect`].
    pub matched_indirect: usize,
    /// See [`ModelComparison::missing`].
    pub missing: usize,
    /// See [`ModelComparison::unobserved`].
    pub unobserved: usize,
    /// Observed target relations that are not in the project (other jobs, notebooks).
    pub outside_project: usize,
}

impl Comparison {
    /// Share of predicted direct edges that were observed, over all observed models.
    pub fn precision(&self) -> Option<f64> {
        ratio(self.matched, self.matched + self.unobserved)
    }

    /// Share of observed edges that were predicted, over all observed models.
    pub fn recall(&self) -> Option<f64> {
        ratio(
            self.matched + self.matched_indirect,
            self.matched + self.matched_indirect + self.missing,
        )
    }
}

#[expect(clippy::cast_precision_loss, reason = "edge counts are far below 2^52")]
fn ratio(part: usize, whole: usize) -> Option<f64> {
    (whole > 0).then(|| part as f64 / whole as f64)
}

/// What [`ColumnGraph::with_observed`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Stitched {
    /// Opaque nodes that now carry observed lineage.
    pub nodes: Vec<String>,
    /// Whether impact uses it (`true`) or still treats those nodes as opaque.
    pub trusted: bool,
}

impl ColumnGraph {
    /// Compares each model's static lineage with `observed`. Names must already be
    /// normalized like the graph's (see [`ObservedLineage::normalized`]).
    pub fn compare_observed(&self, observed: &ObservedLineage) -> Comparison {
        let mut by_target: BTreeMap<&RelationName, BTreeSet<(&ColumnRef, &str)>> = BTreeMap::new();
        for (source, target) in &observed.column_edges {
            by_target
                .entry(&target.relation)
                .or_default()
                .insert((source, target.column.as_str()));
        }
        let targets = observed.targets();
        let mut comparison = Comparison {
            outside_project: targets
                .iter()
                .filter(|t| self.node_for(t).is_none())
                .count(),
            ..Comparison::default()
        };
        for node in self.nodes() {
            if node.kind != NodeKind::Model {
                continue;
            }
            let seen = by_target.get(&node.relation);
            let observed_any = targets.contains(&node.relation);
            let model = match (&node.lineage, observed_any) {
                (_, false) => empty(&node.id, Agreement::NotObserved),
                (Some(lineage), true) if !lineage.opaque => {
                    compare_one(&node.id, lineage, seen.unwrap_or(&BTreeSet::new()))
                }
                (_, true) => empty(&node.id, Agreement::OpaqueObserved),
            };
            comparison.matched += model.matched;
            comparison.matched_indirect += model.matched_indirect;
            comparison.missing += model.missing.len();
            comparison.unobserved += model.unobserved.len();
            comparison.models.push(model);
        }
        comparison
    }

    /// A copy of the graph in which opaque nodes (no SQL, or SQL the analyzer couldn't
    /// read, such as Python models) take their lineage from `observed`, marked
    /// [`Confidence::Observed`].
    ///
    /// Observed lineage only covers paths that ran, so by default the stitched nodes
    /// stay opaque for impact: every change to what they read still makes them run,
    /// and the lineage is only shown. With `trust`, impact uses it and may skip them;
    /// that is the caller's explicit choice (AGENTS.md rule 3). Relations a node
    /// declares but wasn't observed reading keep making it run.
    #[must_use]
    pub fn with_observed(&self, observed: &ObservedLineage, trust: bool) -> (Self, Stitched) {
        let mut nodes = self.nodes.clone();
        let mut stitched = Stitched {
            nodes: Vec::new(),
            trusted: trust,
        };
        for node in nodes.values_mut() {
            if !node.is_opaque() {
                continue;
            }
            let Some(lineage) = observed_lineage(&node.relation, observed, trust) else {
                continue;
            };
            for output in &lineage.outputs {
                if !node.columns.contains(&output.name) {
                    node.columns.push(output.name.clone());
                }
            }
            node.lineage = Some(lineage);
            stitched.nodes.push(node.id.clone());
        }
        (Self::from_nodes(nodes), stitched)
    }
}

fn empty(node: &str, agreement: Agreement) -> ModelComparison {
    ModelComparison {
        node: node.to_owned(),
        agreement,
        matched: 0,
        matched_indirect: 0,
        missing: Vec::new(),
        unobserved: Vec::new(),
        precision: None,
        recall: None,
    }
}

fn compare_one(
    node: &str,
    lineage: &QueryLineage,
    seen: &BTreeSet<(&ColumnRef, &str)>,
) -> ModelComparison {
    let predicted: BTreeSet<(&ColumnRef, &str)> = lineage
        .outputs
        .iter()
        .flat_map(|o| {
            o.inputs
                .iter()
                .filter(|(_, edge)| matches!(edge, EdgeKind::Direct(_)))
                .map(|(c, _)| (c, o.name.as_str()))
        })
        .collect();
    // Row-shaping inputs, overall or attached to one output (a CASE condition).
    let indirect: BTreeSet<&ColumnRef> = lineage
        .row_inputs
        .iter()
        .map(|(c, _)| c)
        .chain(lineage.outputs.iter().flat_map(|o| {
            o.inputs
                .iter()
                .filter(|(_, edge)| matches!(edge, EdgeKind::Indirect(_)))
                .map(|(c, _)| c)
        }))
        .collect();
    let mut model = empty(node, Agreement::Agrees);
    for &(source, output) in seen {
        if predicted.contains(&(source, output)) {
            model.matched += 1;
        } else if indirect.contains(source) {
            model.matched_indirect += 1;
        } else {
            model.missing.push(EdgeDiff {
                source: source.clone(),
                output: output.to_owned(),
            });
        }
    }
    model.unobserved = predicted
        .difference(seen)
        .map(|&(source, output)| EdgeDiff {
            source: source.clone(),
            output: output.to_owned(),
        })
        .collect();
    model.agreement = if !model.missing.is_empty() {
        Agreement::Misses
    } else if model.unobserved.is_empty() {
        Agreement::Agrees
    } else {
        Agreement::Covers
    };
    model.precision = ratio(model.matched, predicted.len());
    model.recall = ratio(model.matched + model.matched_indirect, seen.len());
    model
}

/// `relation`'s lineage as observed, or `None` if nothing was.
fn observed_lineage(
    relation: &RelationName,
    observed: &ObservedLineage,
    trust: bool,
) -> Option<QueryLineage> {
    let mut outputs: BTreeMap<&str, BTreeSet<(ColumnRef, EdgeKind)>> = BTreeMap::new();
    for (source, target) in &observed.column_edges {
        if &target.relation == relation {
            // Platforms don't say how a column is derived; transformation is the
            // weakest direct claim.
            outputs
                .entry(target.column.as_str())
                .or_default()
                .insert((source.clone(), EdgeKind::Direct(DirectKind::Transformation)));
        }
    }
    let row_inputs: BTreeSet<(ColumnRef, IndirectKind)> = observed
        .row_inputs
        .iter()
        .filter(|(_, target)| target == relation)
        .map(|(source, _)| (source.clone(), IndirectKind::Filter))
        .collect();
    let relations_read: BTreeSet<RelationName> = observed
        .relation_edges
        .iter()
        .filter(|(_, target)| target == relation)
        .map(|(source, _)| source.clone())
        .collect();
    if relations_read.is_empty() {
        return None;
    }
    let outputs = outputs
        .into_iter()
        .map(|(name, inputs)| {
            let digest = format!(
                "observed:{}",
                inputs
                    .iter()
                    .map(|(c, _)| c.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            OutputColumn::new(name, inputs, digest, Confidence::Observed)
        })
        .collect();
    let mut lineage = QueryLineage::new(
        outputs,
        row_inputs,
        relations_read,
        "observed",
        vec![
            "lineage observed at run time; paths that didn't run are missing".to_owned(),
            "the platform doesn't say how inputs are used: direct inputs are reported as \
             transformations, row inputs as filters"
                .to_owned(),
        ],
    );
    lineage.confidence = Confidence::Observed;
    lineage.opaque = !trust;
    Some(lineage)
}
