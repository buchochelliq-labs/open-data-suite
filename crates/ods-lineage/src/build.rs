//! Analyzing a project in parallel dependency waves, with caching.

use std::collections::{BTreeMap, BTreeSet};

use ods_core::RelationName;
use ods_sdk::ProviderError;
use ods_sdk::contracts::sql_lineage::{AnalyzeRequest, SchemaLookup, SqlLineageAnalyzer};
use rayon::prelude::*;
use serde::Serialize;

use crate::cache::{LineageCache, cache_key};
use crate::graph::{ColumnGraph, NodeLineage};
use crate::project::{LineageNode, LineageProject};

/// Why a project could not be analyzed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// Two nodes share an id.
    #[error("duplicate node id `{0}`")]
    DuplicateId(String),
    /// Two nodes build the same relation.
    #[error("nodes `{0}` and `{1}` both build `{2}`")]
    DuplicateRelation(String, String, RelationName),
    /// The dependencies contain a cycle through these nodes.
    #[error("dependency cycle through: {}", .0.join(", "))]
    Cycle(Vec<String>),
    /// The analyzer itself failed (not the SQL: unparseable SQL is an opaque result).
    #[error("analyzer failed on `{node}`: {source}")]
    Analyzer {
        /// The node being analyzed.
        node: String,
        /// The analyzer's error.
        source: ProviderError,
    },
}

/// What a build did, for diagnostics and benchmarks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct BuildStats {
    /// Models analyzed now.
    pub analyzed: usize,
    /// Models whose result came from the cache.
    pub cached: usize,
    /// Nodes whose lineage is unknown: SQL that couldn't be analyzed, models without
    /// SQL, and nodes with upstreams but no lineage (e.g. snapshots).
    pub opaque: usize,
    /// Dependency waves (the critical path length).
    pub waves: usize,
    /// Dependencies that name no node in the project.
    pub missing_dependencies: BTreeSet<String>,
}

struct Known<'a>(&'a BTreeMap<RelationName, Vec<String>>);

impl SchemaLookup for Known<'_> {
    fn columns(&self, relation: &RelationName) -> Option<Vec<String>> {
        self.0.get(relation).cloned()
    }
}

/// Analyzes every model in `project` and builds its [`ColumnGraph`].
///
/// Models are analyzed after everything they depend on, so a model's output columns
/// are known when its consumers resolve `select *`. Independent models run in parallel.
///
/// # Errors
/// Returns [`BuildError`] for an inconsistent project or a failing analyzer.
pub fn build(
    project: &LineageProject,
    analyzer: &dyn SqlLineageAnalyzer,
    cache: &dyn LineageCache,
) -> Result<(ColumnGraph, BuildStats), BuildError> {
    let (by_id, relations) = index(project)?;
    let mut stats = BuildStats::default();
    let waves = waves(&by_id, &mut stats)?;
    stats.waves = waves.len();
    let version = analyzer.analyzer_version();

    // Columns known so far, by relation. Seeded from declared/catalog columns and
    // overwritten by analyzed outputs, which reflect the SQL being analyzed now.
    let mut known: BTreeMap<RelationName, Vec<String>> = project
        .nodes
        .iter()
        .filter_map(|n| n.columns.clone().map(|c| (n.relation.clone(), c)))
        .collect();
    let mut done: BTreeMap<String, NodeLineage> = BTreeMap::new();

    for wave in waves {
        let results: Vec<Result<Analyzed, BuildError>> = wave
            .par_iter()
            .map(|id| {
                analyze_node(
                    by_id[id.as_str()],
                    &by_id,
                    &known,
                    &version,
                    analyzer,
                    cache,
                )
            })
            .collect();
        for result in results {
            let Analyzed(node, hit) = result?;
            if node.lineage.is_some() {
                if hit {
                    stats.cached += 1;
                } else {
                    stats.analyzed += 1;
                }
            }
            if node.is_opaque() {
                stats.opaque += 1;
            }
            if !node.columns.is_empty() {
                known.insert(node.relation.clone(), node.columns.clone());
            }
            done.insert(node.id.clone(), node);
        }
    }
    debug_assert_eq!(relations.len(), done.len());
    Ok((ColumnGraph::from_nodes(done), stats))
}

type Index<'a> = (
    BTreeMap<&'a str, &'a LineageNode>,
    BTreeMap<&'a RelationName, &'a str>,
);

fn index(project: &LineageProject) -> Result<Index<'_>, BuildError> {
    let mut by_id = BTreeMap::new();
    let mut relations: BTreeMap<&RelationName, &str> = BTreeMap::new();
    for node in &project.nodes {
        if by_id.insert(node.id.as_str(), node).is_some() {
            return Err(BuildError::DuplicateId(node.id.clone()));
        }
        if let Some(other) = relations.insert(&node.relation, node.id.as_str()) {
            return Err(BuildError::DuplicateRelation(
                other.to_owned(),
                node.id.clone(),
                node.relation.clone(),
            ));
        }
    }
    Ok((by_id, relations))
}

/// Groups node ids into waves: every node's dependencies are in earlier waves.
fn waves(
    by_id: &BTreeMap<&str, &LineageNode>,
    stats: &mut BuildStats,
) -> Result<Vec<Vec<String>>, BuildError> {
    let mut remaining: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (id, node) in by_id {
        let deps = node
            .depends_on
            .iter()
            .filter(|d| {
                let known = by_id.contains_key(d.as_str());
                if !known {
                    stats.missing_dependencies.insert((*d).clone());
                }
                known && d.as_str() != *id
            })
            .map(String::as_str)
            .collect();
        remaining.insert(id, deps);
    }
    let mut waves = Vec::new();
    while !remaining.is_empty() {
        let ready: Vec<&str> = remaining
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(id, _)| *id)
            .collect();
        if ready.is_empty() {
            return Err(BuildError::Cycle(
                remaining.keys().map(|id| (*id).to_owned()).collect(),
            ));
        }
        for id in &ready {
            remaining.remove(id);
        }
        for deps in remaining.values_mut() {
            for id in &ready {
                deps.remove(id);
            }
        }
        waves.push(ready.into_iter().map(str::to_owned).collect());
    }
    Ok(waves)
}

/// A node result, and whether it came from the cache.
struct Analyzed(NodeLineage, bool);

fn analyze_node(
    node: &LineageNode,
    by_id: &BTreeMap<&str, &LineageNode>,
    known: &BTreeMap<RelationName, Vec<String>>,
    version: &str,
    analyzer: &dyn SqlLineageAnalyzer,
    cache: &dyn LineageCache,
) -> Result<Analyzed, BuildError> {
    let declared = node.columns.clone().unwrap_or_default();
    let depends_on: Vec<RelationName> = node
        .depends_on
        .iter()
        .filter_map(|d| by_id.get(d.as_str()))
        .map(|dep| dep.relation.clone())
        .collect();
    let Some(sql) = &node.sql else {
        return Ok(Analyzed(
            NodeLineage {
                id: node.id.clone(),
                relation: node.relation.clone(),
                kind: node.kind,
                columns: declared,
                lineage: None,
                cache_key: None,
                depends_on,
            },
            false,
        ));
    };
    let upstream: BTreeMap<&RelationName, Option<&[String]>> = node
        .depends_on
        .iter()
        .filter_map(|d| by_id.get(d.as_str()))
        .map(|dep| (&dep.relation, known.get(&dep.relation).map(Vec::as_slice)))
        .collect();
    let key = cache_key(version, sql, upstream.iter().map(|(r, c)| (*r, *c)));
    let (lineage, hit) = if let Some(cached) = cache.get(&key) {
        (cached, true)
    } else {
        let lineage = analyzer
            .analyze(&AnalyzeRequest {
                sql,
                schema: &Known(known),
            })
            .map_err(|source| BuildError::Analyzer {
                node: node.id.clone(),
                source,
            })?;
        // Only cache results whose inputs are all covered by the key: a query that reads
        // a relation outside its declared dependencies depends on schemas the key
        // doesn't capture.
        let covered = lineage
            .relations_read
            .iter()
            .all(|r| upstream.contains_key(r));
        if covered {
            cache.put(key.clone(), lineage.clone());
        }
        (lineage, false)
    };
    let columns = if lineage.opaque {
        declared
    } else {
        lineage.outputs.iter().map(|o| o.name.clone()).collect()
    };
    Ok(Analyzed(
        NodeLineage {
            id: node.id.clone(),
            relation: node.relation.clone(),
            kind: node.kind,
            columns,
            lineage: Some(lineage),
            cache_key: Some(key),
            depends_on,
        },
        hit,
    ))
}
