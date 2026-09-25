//! Entity-relationship models for OpenDataSuite (#60, #62, #63).
//!
//! An ERD says which columns identify rows (keys) and which columns point at rows of
//! another entity (relationships). It is **not** lineage (AGENTS.md rule 6): a
//! dependency edge between two models says nothing about keys, and a relationship
//! needs no dependency. This crate never reads lineage.
//!
//! Every key and relationship carries its [`Basis`] and evidence (rule 4):
//! - [`Basis::Declared`]: a constraint in the project (e.g. a model contract);
//! - [`Basis::Tested`]: a data test asserts it (uniqueness, not-null, relationships);
//! - [`Basis::Inferred`]: only a naming convention suggests it. Inference is opt-in and
//!   always labelled, never presented as fact (rule 3).
//!
//! The project format's specifics (test names, constraint syntax) are mapped to
//! [`Fact`]s by the caller, so this crate stays provider-neutral (rule 1).

mod build;
mod render;

use serde::{Deserialize, Serialize};

pub use build::{BuildOptions, build};

/// Version of the JSON form of [`Erd`].
pub const SCHEMA_VERSION: u32 = 1;

/// What kind of relation an entity is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EntityKind {
    /// A model materialized as a table (or incremental table).
    Table,
    /// A model materialized as a view.
    View,
    /// Loaded from a file in the project.
    Seed,
    /// A history-keeping snapshot.
    Snapshot,
    /// An external table the project reads.
    Source,
}

/// A column of an input entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ColumnInput {
    /// The column name.
    pub name: String,
    /// Its data type, if known.
    pub data_type: Option<String>,
    /// What it means, if documented.
    pub description: Option<String>,
}

impl ColumnInput {
    /// A column.
    pub fn new(name: impl Into<String>, data_type: Option<String>) -> Self {
        Self {
            name: name.into(),
            data_type,
            description: None,
        }
    }

    /// With its documented meaning.
    #[must_use]
    pub fn with_description(mut self, description: Option<String>) -> Self {
        self.description = description;
        self
    }
}

/// An entity to draw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct EntityInput {
    /// Stable id, e.g. the project's node id.
    pub id: String,
    /// Display name, e.g. `orders`.
    pub name: String,
    /// What it is.
    pub kind: EntityKind,
    /// Its columns, in order. May be empty if unknown.
    pub columns: Vec<ColumnInput>,
    /// What one row is, if documented.
    pub description: Option<String>,
    /// The fully qualified name to query it by, as the warehouse expects it.
    pub relation: Option<String>,
}

impl EntityInput {
    /// An entity.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: EntityKind,
        columns: Vec<ColumnInput>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            columns,
            description: None,
            relation: None,
        }
    }

    /// With its documented meaning.
    #[must_use]
    pub fn with_description(mut self, description: Option<String>) -> Self {
        self.description = description;
        self
    }

    /// With the name to query it by.
    #[must_use]
    pub fn with_relation(mut self, relation: Option<String>) -> Self {
        self.relation = relation;
        self
    }
}

/// Why a fact holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Basis {
    /// Only a naming convention suggests it.
    Inferred,
    /// The project's SQL joins on it (`a.x = b.y`); direction and cardinality come from
    /// the keys on either side.
    Joined,
    /// A data test asserts it.
    Tested,
    /// A constraint declares it.
    Declared,
}

/// A statement about keys, with the evidence behind it (e.g. a test or constraint id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "fact")]
#[non_exhaustive]
pub enum Fact {
    /// No two rows share these columns' values.
    Unique {
        /// Entity id.
        entity: String,
        /// The columns, together.
        columns: Vec<String>,
        /// Declared or tested.
        basis: Basis,
        /// E.g. a test id.
        evidence: String,
    },
    /// The column is never null.
    NotNull {
        /// Entity id.
        entity: String,
        /// The column.
        column: String,
        /// Declared or tested.
        basis: Basis,
        /// E.g. a test id.
        evidence: String,
    },
    /// The columns are the primary key.
    PrimaryKey {
        /// Entity id.
        entity: String,
        /// The columns.
        columns: Vec<String>,
        /// Declared or tested.
        basis: Basis,
        /// E.g. a constraint.
        evidence: String,
    },
    /// The project's SQL joins these columns (`left[i] = right[i]`). Which side
    /// references which is worked out from their keys.
    Joined {
        /// One entity id.
        left: String,
        /// Its columns.
        left_columns: Vec<String>,
        /// The other entity id.
        right: String,
        /// Its matching columns.
        right_columns: Vec<String>,
        /// E.g. the model whose SQL makes the join.
        evidence: String,
    },
    /// Every value of `columns` exists in `to_columns` of `to`.
    ForeignKey {
        /// Entity id holding the reference.
        entity: String,
        /// Referencing columns.
        columns: Vec<String>,
        /// Referenced entity id.
        to: String,
        /// Referenced columns.
        to_columns: Vec<String>,
        /// Declared or tested.
        basis: Basis,
        /// E.g. a test id.
        evidence: String,
    },
}

/// A key of an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Key {
    /// The columns, in the entity's spelling.
    pub columns: Vec<String>,
    /// How we know.
    pub basis: Basis,
    /// Evidence, sorted.
    pub evidence: Vec<String>,
}

/// A column in the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Column {
    /// The column name.
    pub name: String,
    /// Its data type, if known.
    pub data_type: Option<String>,
    /// What it means, if documented.
    pub description: Option<String>,
    /// Part of the primary key.
    pub primary_key: bool,
    /// Part of a relationship to another entity.
    pub foreign_key: bool,
    /// Asserted never null.
    pub not_null: bool,
}

/// An entity with its keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Entity {
    /// Stable id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What it is.
    pub kind: EntityKind,
    /// What one row is, if documented.
    pub description: Option<String>,
    /// The fully qualified name to query it by.
    pub relation: Option<String>,
    /// Its columns.
    pub columns: Vec<Column>,
    /// The primary key, if one is known.
    pub primary_key: Option<Key>,
    /// Other unique keys (including unique columns that may be null).
    pub unique_keys: Vec<Key>,
}

/// How many rows on each side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Cardinality {
    /// Many referencing rows per referenced row.
    ManyToOne,
    /// At most one referencing row per referenced row (the reference is unique).
    OneToOne,
    /// Neither side is a known key: joining may multiply rows on both sides.
    Unknown,
}

/// `from.from_columns` references `to.to_columns`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Relationship {
    /// Referencing entity id.
    pub from: String,
    /// Referencing columns.
    pub from_columns: Vec<String>,
    /// Referenced entity id.
    pub to: String,
    /// Referenced columns.
    pub to_columns: Vec<String>,
    /// How many on each side.
    pub cardinality: Cardinality,
    /// Whether the reference may be null (no not-null evidence on the referencing
    /// columns).
    pub optional: bool,
    /// How we know.
    pub basis: Basis,
    /// Evidence, sorted.
    pub evidence: Vec<String>,
}

/// An entity-relationship model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Erd {
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Entities, sorted by id.
    pub entities: Vec<Entity>,
    /// Relationships, sorted.
    pub relationships: Vec<Relationship>,
    /// Facts that couldn't be applied, and why.
    pub diagnostics: Vec<String>,
}

impl Erd {
    /// The entity with this id, or else the only one with this name.
    pub fn entity(&self, id_or_name: &str) -> Option<&Entity> {
        self.entities
            .iter()
            .find(|e| e.id == id_or_name)
            .or_else(|| {
                let mut named = self.entities.iter().filter(|e| e.name == id_or_name);
                match (named.next(), named.next()) {
                    (Some(entity), None) => Some(entity),
                    _ => None,
                }
            })
    }

    /// Only the entities in `focus` (ids or names) and those within `depth`
    /// relationships of them, in either direction; relationships between kept
    /// entities only. An empty focus keeps everything.
    #[must_use]
    pub fn focused(&self, focus: &[String], depth: usize) -> Self {
        if focus.is_empty() {
            return self.clone();
        }
        let mut keep: std::collections::BTreeSet<String> = focus
            .iter()
            .filter_map(|f| self.entity(f).map(|e| e.id.clone()))
            .collect();
        for _ in 0..depth {
            let next: Vec<String> = self
                .relationships
                .iter()
                .filter_map(|r| {
                    if keep.contains(&r.from) {
                        Some(r.to.clone())
                    } else if keep.contains(&r.to) {
                        Some(r.from.clone())
                    } else {
                        None
                    }
                })
                .collect();
            let before = keep.len();
            keep.extend(next);
            if keep.len() == before {
                break;
            }
        }
        Self {
            schema_version: self.schema_version,
            entities: self
                .entities
                .iter()
                .filter(|e| keep.contains(&e.id))
                .cloned()
                .collect(),
            relationships: self
                .relationships
                .iter()
                .filter(|r| keep.contains(&r.from) && keep.contains(&r.to))
                .cloned()
                .collect(),
            diagnostics: self.diagnostics.clone(),
        }
    }

    /// Only entities that take part in at least one relationship.
    #[must_use]
    pub fn connected_only(&self) -> Self {
        let used: std::collections::BTreeSet<&str> = self
            .relationships
            .iter()
            .flat_map(|r| [r.from.as_str(), r.to.as_str()])
            .collect();
        Self {
            entities: self
                .entities
                .iter()
                .filter(|e| used.contains(e.id.as_str()))
                .cloned()
                .collect(),
            ..self.clone()
        }
    }
}
