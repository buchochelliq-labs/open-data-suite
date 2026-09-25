//! The neutral input: nodes, their relations, SQL and dependencies.

use ods_core::RelationName;
use serde::{Deserialize, Serialize};

/// What a node is. Only models carry SQL to analyze.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NodeKind {
    /// A transformation defined by SQL.
    Model,
    /// A table loaded from a file.
    Seed,
    /// A history-tracking table built by the tool.
    Snapshot,
    /// An external input table.
    Source,
}

/// One node of the project graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LineageNode {
    /// Stable identifier, e.g. `model.jaffle.orders`.
    pub id: String,
    /// The relation the node builds or reads.
    pub relation: RelationName,
    /// What the node is.
    pub kind: NodeKind,
    /// Rendered SQL for models; `None` for nodes without SQL, or models whose SQL isn't
    /// available (those are opaque).
    pub sql: Option<String>,
    /// Known columns in order (e.g. from a warehouse catalog), normalized.
    pub columns: Option<Vec<String>>,
    /// Ids of the nodes this one depends on.
    pub depends_on: Vec<String>,
}

impl LineageNode {
    /// A node.
    pub fn new(id: impl Into<String>, relation: RelationName, kind: NodeKind) -> Self {
        Self {
            id: id.into(),
            relation,
            kind,
            sql: None,
            columns: None,
            depends_on: Vec::new(),
        }
    }

    /// Sets the SQL.
    #[must_use]
    pub fn with_sql(mut self, sql: impl Into<String>) -> Self {
        self.sql = Some(sql.into());
        self
    }

    /// Sets the known columns.
    #[must_use]
    pub fn with_columns<I, S>(mut self, columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.columns = Some(columns.into_iter().map(Into::into).collect());
        self
    }

    /// Sets the dependencies.
    #[must_use]
    pub fn with_depends_on<I, S>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.depends_on = ids.into_iter().map(Into::into).collect();
        self
    }
}

/// A whole project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LineageProject {
    /// All nodes, in any order.
    pub nodes: Vec<LineageNode>,
}

impl LineageProject {
    /// A project from nodes.
    pub fn new(nodes: Vec<LineageNode>) -> Self {
        Self { nodes }
    }
}
