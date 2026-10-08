//! The Semantic layer (#352): the semantic models and metrics the project declares,
//! read-only. A placeholder for later work, and it says so: ODS reads the definitions
//! and shows what each is defined on; it never queries, validates or serves a metric,
//! and lineage doesn't go through metrics yet.
//!
//! The binary hands over the definitions ([`SemanticInput`]) as neutral facts; the
//! models they are defined on are named and linked from the Catalog.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::catalog::{NodeLink, node_href};
use crate::dashboard::{DASHBOARD_SCHEMA_VERSION, Dashboard};

// ------------------------------------------------------------------------- inputs

/// The project's semantic layer, filled in by the binary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SemanticInput {
    /// Where the definitions were read from, for people; `None` when nothing could be
    /// read.
    pub source: Option<String>,
    /// The semantic models, sorted by id.
    pub models: Vec<SemanticModelInput>,
    /// The metrics, sorted by id.
    pub metrics: Vec<MetricInput>,
}

impl SemanticInput {
    /// The definitions read from `source`.
    pub fn new(
        source: impl Into<String>,
        mut models: Vec<SemanticModelInput>,
        mut metrics: Vec<MetricInput>,
    ) -> Self {
        models.sort_by(|a, b| a.id.cmp(&b.id));
        metrics.sort_by(|a, b| a.id.cmp(&b.id));
        Self {
            source: Some(source.into()),
            models,
            metrics,
        }
    }
}

/// A semantic model: the entities, measures and dimensions declared on a model.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SemanticModelInput {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its label, if declared.
    pub label: Option<String>,
    /// Its description, if declared.
    pub description: Option<String>,
    /// The ids of the nodes it is defined on.
    pub defined_on: Vec<String>,
    /// Its entities.
    pub entities: Vec<SemanticField>,
    /// Its measures.
    pub measures: Vec<SemanticField>,
    /// Its dimensions.
    pub dimensions: Vec<SemanticField>,
}

impl SemanticModelInput {
    /// A semantic model with nothing declared but its id and name.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            label: None,
            description: None,
            defined_on: Vec::new(),
            entities: Vec::new(),
            measures: Vec::new(),
            dimensions: Vec::new(),
        }
    }
}

/// An entity, measure or dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SemanticField {
    /// Its name.
    pub name: String,
    /// What kind it is, as the project declares it: an entity's or dimension's type, a
    /// measure's aggregation; `None` when it doesn't say.
    pub kind: Option<String>,
    /// Its description, if declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl SemanticField {
    /// The field `name`, of kind `kind`.
    pub fn new(name: impl Into<String>, kind: Option<String>) -> Self {
        Self {
            name: name.into(),
            kind,
            description: None,
        }
    }
}

/// A metric, as declared: what it is computed from, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MetricInput {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its label, if declared.
    pub label: Option<String>,
    /// Its description, if declared.
    pub description: Option<String>,
    /// Its type, as declared, e.g. `simple` or `ratio`.
    pub kind: Option<String>,
    /// How it is computed, for people, e.g. `order_total` or `revenue / orders`.
    pub computed_from: Option<String>,
    /// The ids of the semantic models and metrics it reads.
    pub reads: Vec<String>,
}

impl MetricInput {
    /// A metric with nothing declared but its id and name.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            label: None,
            description: None,
            kind: None,
            computed_from: None,
            reads: Vec::new(),
        }
    }
}

// --------------------------------------------------------------------- view models

/// The Semantic layer page's view model, and `/api/catalog/semantic`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SemanticView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Where the definitions were read from, for people.
    pub source: Option<String>,
    /// The semantic models.
    pub models: Vec<SemanticModelView>,
    /// The metrics.
    pub metrics: Vec<MetricView>,
}

/// A semantic model on the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct SemanticModelView {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its label, if declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Its description, if declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The models it is defined on, linked when the Catalog has them.
    pub defined_on: Vec<NodeLink>,
    /// Its entities.
    pub entities: Vec<SemanticField>,
    /// Its measures.
    pub measures: Vec<SemanticField>,
    /// Its dimensions.
    pub dimensions: Vec<SemanticField>,
}

/// A metric on the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct MetricView {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its label, if declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Its description, if declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Its type, as declared.
    pub kind: Option<String>,
    /// How it is computed, for people.
    pub computed_from: Option<String>,
    /// The semantic models it reads, directly or through the metrics it reads, by name.
    pub semantic_models: Vec<String>,
    /// The dimensions of those semantic models: what it can be sliced by, sorted.
    pub dimensions: Vec<String>,
    /// The models those semantic models are defined on: what a change reaches it
    /// through.
    pub depends_on: Vec<NodeLink>,
}

impl Dashboard {
    /// The Semantic layer page (#352).
    pub fn semantic(&self) -> SemanticView {
        let input = &self.semantic;
        let names: BTreeMap<&str, &str> = self
            .catalog
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.name.as_str()))
            .collect();
        // A model the Catalog has links to its page; any other node is only named.
        let link = |id: &str| NodeLink {
            id: id.to_owned(),
            name: names
                .get(id)
                .map_or_else(|| id.to_owned(), |n| (*n).to_owned()),
            href: names.contains_key(id).then(|| node_href(id)),
        };
        let models: BTreeMap<&str, &SemanticModelInput> =
            input.models.iter().map(|m| (m.id.as_str(), m)).collect();
        let metrics: BTreeMap<&str, &MetricInput> =
            input.metrics.iter().map(|m| (m.id.as_str(), m)).collect();
        SemanticView {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            source: input.source.clone(),
            models: input
                .models
                .iter()
                .map(|m| SemanticModelView {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    label: m.label.clone(),
                    description: m.description.clone(),
                    defined_on: m.defined_on.iter().map(|id| link(id)).collect(),
                    entities: m.entities.clone(),
                    measures: m.measures.clone(),
                    dimensions: m.dimensions.clone(),
                })
                .collect(),
            metrics: input
                .metrics
                .iter()
                .map(|m| {
                    let reached = reached(m, &metrics);
                    let semantic: Vec<&SemanticModelInput> = reached
                        .iter()
                        .filter_map(|id| models.get(id.as_str()).copied())
                        .collect();
                    let dimensions: BTreeSet<&str> = semantic
                        .iter()
                        .flat_map(|s| s.dimensions.iter().map(|d| d.name.as_str()))
                        .collect();
                    let depends_on: BTreeSet<&str> = semantic
                        .iter()
                        .flat_map(|s| s.defined_on.iter().map(String::as_str))
                        .collect();
                    MetricView {
                        id: m.id.clone(),
                        name: m.name.clone(),
                        label: m.label.clone(),
                        description: m.description.clone(),
                        kind: m.kind.clone(),
                        computed_from: m.computed_from.clone(),
                        semantic_models: semantic.iter().map(|s| s.name.clone()).collect(),
                        dimensions: dimensions.into_iter().map(str::to_owned).collect(),
                        depends_on: depends_on.into_iter().map(link).collect(),
                    }
                })
                .collect(),
        }
    }
}

/// Everything `metric` reads, through the metrics it reads, sorted; a cycle, which a
/// project can't declare, ends where it repeats.
fn reached(metric: &MetricInput, metrics: &BTreeMap<&str, &MetricInput>) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut pending: Vec<&str> = metric.reads.iter().map(String::as_str).collect();
    while let Some(id) = pending.pop() {
        if !seen.insert(id.to_owned()) {
            continue;
        }
        if let Some(read) = metrics.get(id) {
            pending.extend(read.reads.iter().map(String::as_str));
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric(id: &str, reads: &[&str]) -> MetricInput {
        let mut m = MetricInput::new(id, id);
        m.reads = reads.iter().map(|r| (*r).to_owned()).collect();
        m
    }

    #[test]
    fn a_metric_reaches_through_the_metrics_it_reads_and_cycles_end() {
        let ratio = metric("m.ratio", &["m.a", "m.b"]);
        let a = metric("m.a", &["sm.orders"]);
        let b = metric("m.b", &["sm.customers", "m.ratio"]);
        let metrics: BTreeMap<&str, &MetricInput> =
            [("m.a", &a), ("m.b", &b), ("m.ratio", &ratio)].into();
        let reached: Vec<String> = reached(&ratio, &metrics).into_iter().collect();
        assert_eq!(
            reached,
            ["m.a", "m.b", "m.ratio", "sm.customers", "sm.orders"]
        );
    }
}
