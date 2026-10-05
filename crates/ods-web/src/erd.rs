//! The ERD page (#64): the project's entities, keys and relationships, and the view
//! model behind `/erd` and `/api/erd`.
//!
//! The binary builds the [`Erd`] with `ods-erd`, from the project's tests, constraints
//! and the joins its SQL makes, and says how to make each untested relationship tested
//! ([`Suggestion`]): only it knows the project format (ADR-0001). This page draws
//! keys and relationships, never lineage (AGENTS rule 6), and every edge carries its
//! basis and evidence (rule 4); inferred ones are labelled so (rule 3).

use std::collections::BTreeMap;

use ods_erd::{Basis, Cardinality, Erd, Relationship};
use serde::Serialize;

use crate::dashboard::DASHBOARD_SCHEMA_VERSION;

/// How far from the selected entities the page shows by default.
pub const DEFAULT_DEPTH: usize = 1;

/// What the binary hands over: the ERD, or why there is none, and how to make each
/// untested relationship tested.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ErdInput {
    /// The whole ERD, with inferred keys and relationships (the page can hide them).
    pub erd: Result<Erd, String>,
    /// How to test each joined or inferred relationship, in the project's own terms.
    pub suggestions: Vec<Suggestion>,
}

impl ErdInput {
    /// An ERD and its suggestions.
    pub fn new(erd: Result<Erd, String>, suggestions: Vec<Suggestion>) -> Self {
        Self { erd, suggestions }
    }
}

/// How to make one relationship tested: what to add, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Suggestion {
    /// Referencing entity id.
    pub from: String,
    /// Referencing columns.
    pub from_columns: Vec<String>,
    /// Referenced entity id.
    pub to: String,
    /// Referenced columns.
    pub to_columns: Vec<String>,
    /// Where it goes, for people, e.g. `under stg_payments › columns`.
    pub add_under: String,
    /// What to add, ready to paste.
    pub snippet: String,
}

impl Suggestion {
    /// A suggestion for the relationship `from.from_columns` → `to.to_columns`.
    pub fn new(
        from: impl Into<String>,
        from_columns: Vec<String>,
        to: impl Into<String>,
        to_columns: Vec<String>,
        add_under: impl Into<String>,
        snippet: impl Into<String>,
    ) -> Self {
        Self {
            from: from.into(),
            from_columns,
            to: to.into(),
            to_columns,
            add_under: add_under.into(),
            snippet: snippet.into(),
        }
    }

    fn of(&self, r: &Relationship) -> bool {
        self.from == r.from
            && self.to == r.to
            && self.from_columns == r.from_columns
            && self.to_columns == r.to_columns
    }
}

/// The page's query: which entities, and how far around them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErdQuery {
    /// Entity names or ids; empty for all.
    pub select: Vec<String>,
    /// How many relationships away from the selection.
    pub depth: usize,
    /// Whether entities without any relationship are shown too.
    pub all: bool,
}

impl Default for ErdQuery {
    fn default() -> Self {
        Self {
            select: Vec::new(),
            depth: DEFAULT_DEPTH,
            all: false,
        }
    }
}

impl ErdQuery {
    /// From the URL: `select` (repeatable, or several names separated by spaces or
    /// commas), `depth` and `all`.
    pub fn from_pairs(pairs: &[(String, String)]) -> Self {
        let mut query = Self::default();
        for (key, value) in pairs {
            match key.as_str() {
                "select" => query.select.extend(
                    value
                        .split(|c: char| c.is_whitespace() || c == ',')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned),
                ),
                "depth" => {
                    if let Ok(depth) = value.trim().parse::<usize>() {
                        query.depth = depth.min(10);
                    }
                }
                "all" => query.all = matches!(value.as_str(), "1" | "true" | "on"),
                _ => {}
            }
        }
        query.select.sort();
        query.select.dedup();
        query
    }
}

/// A relationship that isn't tested yet, numbered as on the diagram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Missing {
    /// Its number on the diagram, from 1.
    pub number: usize,
    /// Referencing entity's name.
    pub from: String,
    /// Referenced entity's name.
    pub to: String,
    /// Joined in SQL, or only inferred from names.
    pub basis: Basis,
    /// What is known, for people, with `code` in backticks.
    pub summary: String,
    /// How to make it tested, when the project format says.
    pub suggestion: Option<Suggestion>,
}

/// A relationship already tested or declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Known {
    /// `entity.column`, referencing.
    pub from: String,
    /// Referenced entity's name.
    pub to: String,
    /// Tested or declared.
    pub basis: Basis,
}

/// The ERD page's view model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ErdView {
    /// Format version ([`DASHBOARD_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The selection asked for.
    pub select: Vec<String>,
    /// How far around it.
    pub depth: usize,
    /// Whether entities without relationships are shown.
    pub all: bool,
    /// Names asked for that no entity has.
    pub unknown: Vec<String>,
    /// Names asked for that several entities share: their ids pick one.
    pub ambiguous: Vec<String>,
    /// Why there is no ERD, when there is none.
    pub unavailable: Option<String>,
    /// The entities and relationships in scope, inferred ones included: the page
    /// can hide them.
    pub erd: Option<Erd>,
    /// Edge number on the diagram for each relationship in `erd`, in order: `Some`
    /// for joined and inferred ones.
    pub numbers: Vec<Option<usize>>,
    /// Whether each relationship's cardinality is proven by a tested or declared key,
    /// in order. An inferred relationship, or one to a key that is only inferred, proves
    /// nothing, whatever cardinality was worked out for it.
    pub proven: Vec<bool>,
    /// The joined and inferred relationships, with how to test them.
    pub missing: Vec<Missing>,
    /// The tested and declared relationships.
    pub known: Vec<Known>,
    /// How many entities the whole ERD has.
    pub total_entities: usize,
}

/// The view for `query`.
pub fn erd_view(input: Option<&ErdInput>, query: &ErdQuery) -> ErdView {
    let mut view = ErdView {
        schema_version: DASHBOARD_SCHEMA_VERSION,
        select: query.select.clone(),
        depth: query.depth,
        all: query.all,
        unknown: Vec::new(),
        ambiguous: Vec::new(),
        unavailable: None,
        erd: None,
        numbers: Vec::new(),
        proven: Vec::new(),
        missing: Vec::new(),
        known: Vec::new(),
        total_entities: 0,
    };
    let Some(input) = input else {
        view.unavailable = Some("This server wasn't given an ERD.".to_owned());
        return view;
    };
    let whole = match &input.erd {
        Ok(erd) => erd,
        Err(e) => {
            view.unavailable = Some(e.clone());
            return view;
        }
    };
    view.total_entities = whole.entities.len();
    for wanted in query.select.iter().filter(|s| whole.entity(s).is_none()) {
        let shared = whole.entities.iter().filter(|e| &e.name == wanted).count() > 1;
        if shared {
            view.ambiguous.push(wanted.clone());
        } else {
            view.unknown.push(wanted.clone());
        }
    }
    let known_select: Vec<String> = query
        .select
        .iter()
        .filter(|s| whole.entity(s).is_some())
        .cloned()
        .collect();
    // Only unknown names asked for: nothing is in scope, rather than everything.
    let erd = if !query.select.is_empty() && known_select.is_empty() {
        // Focusing on names no entity has keeps nothing.
        whole.focused(&query.select, 0)
    } else {
        let focused = whole.focused(&known_select, query.depth);
        if query.all || !known_select.is_empty() {
            focused
        } else {
            focused.connected_only()
        }
    };
    let names: BTreeMap<&str, &str> = whole
        .entities
        .iter()
        .map(|e| (e.id.as_str(), e.name.as_str()))
        .collect();
    let name = |id: &str| {
        names
            .get(id)
            .map_or_else(|| id.to_owned(), |n| (*n).to_owned())
    };
    let mut number = 0;
    for r in &erd.relationships {
        let target_key_inferred = whole
            .entity(&r.to)
            .and_then(|e| e.primary_key.as_ref())
            .is_some_and(|k| k.basis == Basis::Inferred && k.columns == r.to_columns);
        let proven = r.basis != Basis::Inferred
            && !target_key_inferred
            && r.cardinality != Cardinality::Unknown;
        view.proven.push(proven);
        match r.basis {
            Basis::Joined | Basis::Inferred => {
                number += 1;
                view.numbers.push(Some(number));
                view.missing.push(Missing {
                    number,
                    from: name(&r.from),
                    to: name(&r.to),
                    basis: r.basis,
                    summary: summary(r, proven, &name),
                    suggestion: input.suggestions.iter().find(|s| s.of(r)).cloned(),
                });
            }
            _ => {
                view.numbers.push(None);
                view.known.push(Known {
                    from: format!("{}.{}", name(&r.from), r.from_columns.join(", ")),
                    to: name(&r.to),
                    basis: r.basis,
                });
            }
        }
    }
    view.erd = Some(erd);
    view
}

/// What is known about an untested relationship, for people.
fn summary(r: &Relationship, proven: bool, name: &dyn Fn(&str) -> String) -> String {
    let columns = if r.from_columns == r.to_columns {
        format!("`{}`", r.from_columns.join("`, `"))
    } else {
        format!(
            "`{}` = `{}`",
            r.from_columns.join("`, `"),
            r.to_columns.join("`, `")
        )
    };
    let how = match r.basis {
        Basis::Joined => {
            // `joined in <node id>`: the model whose SQL makes the join.
            let models: Vec<String> = r
                .evidence
                .iter()
                .map(|e| format!("`{}`", name(e.strip_prefix("joined in ").unwrap_or(e))))
                .collect();
            if r.cardinality == Cardinality::Unknown {
                format!(
                    "joined in {}, not tested. Neither side is a known key, so which side references which isn't known; test a key first",
                    models.join(", ")
                )
            } else {
                format!("joined in {}, not tested", models.join(", "))
            }
        }
        _ => "matching column names only: not joined in SQL, not tested".to_owned(),
    };
    format!("On {columns}: {how}. {}", cardinality_sentence(r, proven))
}

/// The cardinality, for people, as far as keys prove it: nothing is claimed for an
/// unproven one (AGENTS rule 3).
pub(crate) fn cardinality_sentence(r: &Relationship, proven: bool) -> String {
    if !proven && r.cardinality != Cardinality::Unknown {
        return "Cardinality unproven: the referenced columns only look like a key; no test or constraint says so.".to_owned();
    }
    match r.cardinality {
        Cardinality::ManyToOne => format!(
            "Many to {}: the referenced columns are a key.",
            if r.optional {
                "zero or one"
            } else {
                "exactly one"
            }
        ),
        Cardinality::OneToOne => "One to one: both sides are keys.".to_owned(),
        _ => "Cardinality unknown: the referenced columns aren't a tested or declared key."
            .to_owned(),
    }
}
