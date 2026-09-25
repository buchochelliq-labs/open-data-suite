//! Column-level lineage for OpenDataSuite (#74, ADR-0008).
//!
//! This module turns a project (models with rendered SQL, plus sources and seeds) into a
//! [`ColumnGraph`], using any [`SqlLineageAnalyzer`](ods_sdk::contracts::sql_lineage::SqlLineageAnalyzer).
//! The graph answers the question State and CI care about: **given these column
//! changes, which downstream models must run, and why?** ([`ColumnGraph::impact`]).
//!
//! It is fast by construction:
//! - models are analyzed in dependency waves, each wave in parallel;
//! - results are cached under a content-addressed key (analyzer version, SQL, and the
//!   upstream schemas it was resolved against), so unchanged models are never
//!   re-analyzed ([`LineageCache`]).
//!
//! It is conservative (AGENTS.md rule 3): a model whose SQL can't be analyzed is
//! *opaque*, and every change to anything it reads impacts it.
//!
//! It is open: every type serializes, and [`openlineage`] exports the graph as `OpenLineage`
//! column-lineage facets, which catalogs such as `OpenMetadata`, `DataHub` and `Marquez`
//! read (ADR-0008).

mod build;
mod cache;
mod diff;
pub mod export;
mod graph;
mod impact;
pub mod openlineage;
mod project;

pub use build::{BuildError, BuildStats, build};
pub use cache::{LineageCache, MemoryCache, cache_key};
pub use diff::{Change, ColumnChangeKind, diff};
pub use export::{GraphDocument, GraphFilter};
pub use graph::{ColumnGraph, ColumnUse, NodeLineage};
pub use impact::{Impact, ImpactReason, NodeImpact, Pruned};
pub use project::{LineageNode, LineageProject, NodeKind};
