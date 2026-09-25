//! Export as `OpenLineage` events with column-lineage facets.
//!
//! `OpenLineage` is the open standard catalogs ingest (`OpenMetadata`, `DataHub`,
//! `Marquez` and others), so one export reaches all of them. Each model becomes a static
//! `JobEvent` (lineage by design, not tied to a run), or, for sinks that only accept runs,
//! a `COMPLETE` `RunEvent` with a deterministic run id.
//!
//! Facet: `ColumnLineageDatasetFacet` 1-2-0. Direct inputs go in `fields`; row-shaping
//! inputs go in `dataset`, as the spec says, or are also copied into every field for
//! consumers that ignore `dataset` ([`IndirectPlacement::DatasetAndFields`]).

use std::collections::BTreeMap;

use ods_core::{ColumnRef, EdgeKind, RelationName};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::graph::{ColumnGraph, NodeLineage};

/// The `OpenLineage` event schema this module writes.
pub const EVENT_SCHEMA: &str = "https://openlineage.io/spec/2-0-2/OpenLineage.json";
/// The column-lineage facet schema this module writes.
pub const FACET_SCHEMA: &str = "https://openlineage.io/spec/facets/1-2-0/ColumnLineageDatasetFacet.json#/$defs/ColumnLineageDatasetFacet";

/// Where row-shaping (indirect, dataset-wide) inputs are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndirectPlacement {
    /// In the facet's `dataset` array, as the spec defines.
    #[default]
    Dataset,
    /// In `dataset` and also in every field's `inputFields`, for consumers that only read
    /// fields.
    DatasetAndFields,
}

/// Which event type to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EventKind {
    /// Static `JobEvent`s: lineage by design.
    #[default]
    Job,
    /// `COMPLETE` `RunEvent`s with deterministic run ids, for sinks that only take runs.
    Run,
}

/// How to name things and shape the events.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExportOptions {
    /// Dataset namespace, e.g. `unitycatalog://adb-123.azuredatabricks.net`.
    pub dataset_namespace: String,
    /// Job namespace, e.g. the project name.
    pub job_namespace: String,
    /// The `producer` URI.
    pub producer: String,
    /// `eventTime`, RFC 3339. Passed in so exports are reproducible.
    pub event_time: String,
    /// Where indirect inputs go.
    pub indirect: IndirectPlacement,
    /// Which event type.
    pub events: EventKind,
}

impl ExportOptions {
    /// Options with the defaults: job events, indirect inputs in `dataset`.
    pub fn new(
        dataset_namespace: impl Into<String>,
        job_namespace: impl Into<String>,
        event_time: impl Into<String>,
    ) -> Self {
        Self {
            dataset_namespace: dataset_namespace.into(),
            job_namespace: job_namespace.into(),
            producer: concat!(
                "https://github.com/buchochelliq-labs/open-data-suite/tree/v",
                env!("CARGO_PKG_VERSION")
            )
            .to_owned(),
            event_time: event_time.into(),
            indirect: IndirectPlacement::Dataset,
            events: EventKind::Job,
        }
    }

    /// Sets where indirect inputs go.
    #[must_use]
    pub fn with_indirect(mut self, indirect: IndirectPlacement) -> Self {
        self.indirect = indirect;
        self
    }

    /// Sets the event type.
    #[must_use]
    pub fn with_events(mut self, events: EventKind) -> Self {
        self.events = events;
        self
    }
}

fn transformation(edge: EdgeKind) -> Value {
    let (kind, subtype) = match edge {
        EdgeKind::Direct(direct) => ("DIRECT", direct.openlineage_subtype()),
        EdgeKind::Indirect(indirect) => ("INDIRECT", indirect.openlineage_subtype()),
    };
    json!({ "type": kind, "subtype": subtype, "description": "", "masking": false })
}

fn dataset(namespace: &str, relation: &RelationName) -> Value {
    json!({ "namespace": namespace, "name": relation.to_string() })
}

/// `inputFields` entries: one per input column, with every way it is used.
fn input_fields(
    namespace: &str,
    edges: impl IntoIterator<Item = (ColumnRef, EdgeKind)>,
) -> Vec<Value> {
    let mut by_column: BTreeMap<ColumnRef, Vec<EdgeKind>> = BTreeMap::new();
    for (column, edge) in edges {
        by_column.entry(column).or_default().push(edge);
    }
    by_column
        .into_iter()
        .map(|(column, mut edges)| {
            edges.sort();
            edges.dedup();
            json!({
                "namespace": namespace,
                "name": column.relation.to_string(),
                "field": column.column,
                "transformations": edges.into_iter().map(transformation).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn column_lineage_facet(node: &NodeLineage, options: &ExportOptions) -> Option<Value> {
    let lineage = node.lineage.as_ref().filter(|l| !l.opaque)?;
    let ns = options.dataset_namespace.as_str();
    let row_edges: Vec<(ColumnRef, EdgeKind)> = lineage
        .row_inputs
        .iter()
        .map(|(c, k)| (c.clone(), EdgeKind::Indirect(*k)))
        .collect();
    let fields: serde_json::Map<String, Value> = lineage
        .outputs
        .iter()
        .map(|output| {
            let mut edges: Vec<(ColumnRef, EdgeKind)> = output.inputs.iter().cloned().collect();
            if options.indirect == IndirectPlacement::DatasetAndFields {
                edges.extend(row_edges.iter().cloned());
            }
            (
                output.name.clone(),
                json!({ "inputFields": input_fields(ns, edges) }),
            )
        })
        .collect();
    Some(json!({
        "_producer": options.producer,
        "_schemaURL": FACET_SCHEMA,
        "fields": fields,
        "dataset": input_fields(ns, row_edges),
    }))
}

/// A UUID (version 5 layout) derived from `name`, so the same model always gets the
/// same run id.
fn stable_uuid(name: &str) -> String {
    let hash = Sha256::digest(name.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&hash[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

/// One event per node that has SQL. Opaque models still get table-level lineage
/// (inputs and output) but no column facet, so nothing is claimed that isn't known.
/// Events are sorted by node id.
pub fn events(graph: &ColumnGraph, options: &ExportOptions) -> Vec<Value> {
    let ns = options.dataset_namespace.as_str();
    graph
        .nodes()
        .filter_map(|node| {
            let lineage = node.lineage.as_ref()?;
            let inputs: Vec<Value> = lineage
                .relations_read
                .iter()
                .map(|r| dataset(ns, r))
                .collect();
            let mut output = dataset(ns, &node.relation);
            if let Some(facet) = column_lineage_facet(node, options) {
                output["facets"] = json!({ "columnLineage": facet });
            }
            let job = json!({ "namespace": options.job_namespace, "name": node.id });
            let mut event = json!({
                "eventTime": options.event_time,
                "producer": options.producer,
                "job": job,
                "inputs": inputs,
                "outputs": [output],
            });
            match options.events {
                EventKind::Job => {
                    event["schemaURL"] = json!(format!("{EVENT_SCHEMA}#/$defs/JobEvent"));
                }
                EventKind::Run => {
                    event["schemaURL"] = json!(format!("{EVENT_SCHEMA}#/$defs/RunEvent"));
                    event["eventType"] = json!("COMPLETE");
                    let key = format!(
                        "{}/{}/{}",
                        options.job_namespace,
                        node.id,
                        node.cache_key.as_deref().unwrap_or_default()
                    );
                    event["run"] = json!({ "runId": stable_uuid(&key) });
                }
            }
            Some(event)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_uuids_have_the_v5_layout() {
        let id = stable_uuid("jaffle/model.jaffle.orders");
        assert_eq!(id, stable_uuid("jaffle/model.jaffle.orders"));
        assert_ne!(id, stable_uuid("jaffle/model.jaffle.customers"));
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "5");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
    }
}
