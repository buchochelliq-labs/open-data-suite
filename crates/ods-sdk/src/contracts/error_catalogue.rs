//! `ErrorCatalogue`: a provider's patterns for its engine's errors, which classify a
//! failed node's error into ODS's neutral taxonomy (#323, ADR-0025).
//!
//! A provider with the [`error_explain`](ods_core::Capability::ErrorExplain) capability
//! implements it. The host explains a failure from the classification and its own
//! evidence (`ods_state::explain_failure`); core never matches an engine's text
//! (AGENTS.md rule 1).
//!
//! # Semantics
//! - [`classify`](ErrorCatalogue::classify) reads only an [`ErrorSummary`]: the error's
//!   kind and first line, with values and SQL already removed (ADR-0024). It never sees
//!   the raw message or SQL, so a pattern can't match on (or leak) a value.
//! - It is pure and deterministic: the same summary always gets the same answer, with
//!   no I/O.
//! - An error no pattern recognises is [`Classification::NotRecognised`], with only the
//!   coarse category the engine's own kind gives ([`ErrorCategory::Unknown`] if none).
//!   A catalogue never guesses (rule 3).
//! - A pattern's [suggestions](PatternMatch::suggestions) are the engine's own steps
//!   (e.g. a command that installs packages); commands hold no values.
//! - [`CatalogueInfo::version`] changes whenever a pattern is added, changed or
//!   removed, so an explanation says which catalogue made it.
//!
//! A provider may also describe the project for explanations, as a [`ProjectIndex`]:
//! each node's file and the macros its code calls that aren't defined. That is the
//! provider's reading of its own project format.

use std::collections::{BTreeMap, BTreeSet};

use ods_core::SchemaVersion;
use ods_core::failure::{ErrorCategory, Suggestion, Symptom, is_code};
use serde::{Deserialize, Serialize};

use crate::contracts::run_events::ErrorSummary;
use crate::provider::{Contract, Provider};

/// The `error_catalogue` contract.
pub const ERROR_CATALOGUE: Contract = Contract {
    name: "error_catalogue",
    version: SchemaVersion::new(0, 1),
};

/// A catalogue's name and version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CatalogueInfo {
    /// Its name, e.g. the provider's kind.
    pub name: String,
    /// Its version; changes with any pattern.
    pub version: String,
    /// The engine whose errors it reads, as people name it: who "said" a message.
    pub engine: String,
}

impl CatalogueInfo {
    /// A catalogue's description.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        engine: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            engine: engine.into(),
        }
    }
}

/// A pattern that recognised an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PatternMatch {
    /// The pattern's stable id in its catalogue, e.g. `missing-column`.
    pub id: String,
    /// What the error means.
    pub symptom: Symptom,
    /// Its category; usually the symptom's, unless the engine's kind says better.
    pub category: ErrorCategory,
    /// The engine's own steps to try, most useful first.
    pub suggestions: Vec<Suggestion>,
    /// A name the pattern read from the summary, e.g. a Python exception's type. Only
    /// identifier-shaped names are kept ([`is_code`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

impl PatternMatch {
    /// A match of pattern `id` as `symptom`, in the symptom's category.
    pub fn new(id: impl Into<String>, symptom: Symptom) -> Self {
        Self {
            id: id.into(),
            symptom,
            category: symptom.category(),
            suggestions: Vec::new(),
            subject: None,
        }
    }

    /// Sets the name it is about, if identifier-shaped.
    #[must_use]
    pub fn about(mut self, subject: Option<String>) -> Self {
        self.subject = subject.filter(|s| is_code(s));
        self
    }

    /// Sets the category.
    #[must_use]
    pub fn in_category(mut self, category: ErrorCategory) -> Self {
        self.category = category;
        self
    }

    /// Adds a step to try.
    #[must_use]
    pub fn suggest(mut self, suggestion: Suggestion) -> Self {
        self.suggestions.push(suggestion);
        self
    }
}

/// What a catalogue made of an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum Classification {
    /// A pattern recognised it.
    Recognised(PatternMatch),
    /// No pattern did. The category comes from the engine's own kind only.
    NotRecognised {
        /// The engine's kind, as a category; `unknown` if it gave none.
        category: ErrorCategory,
    },
}

impl Classification {
    /// The category.
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::Recognised(m) => m.category,
            Self::NotRecognised { category } => *category,
        }
    }
}

/// A name in a node's code, and the line it is on (1-based) when known.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NameAt {
    /// The name.
    pub name: String,
    /// Its line in the node's source file.
    pub line: Option<u32>,
}

impl NameAt {
    /// A name at a line.
    pub fn new(name: impl Into<String>, line: Option<u32>) -> Self {
        Self {
            name: name.into(),
            line,
        }
    }
}

/// What explanations need to know about one node.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct IndexedNode {
    /// Its display name.
    pub name: String,
    /// Its source file, relative to the project.
    pub file: Option<String>,
    /// The compiled file the engine runs, relative to the project, if it has one.
    pub compiled_file: Option<String>,
    /// Macros its code calls that the project and its installed packages don't
    /// define, in the order they appear.
    pub undefined_calls: Vec<NameAt>,
    /// The language its code is in (e.g. `sql`, `python`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

impl IndexedNode {
    /// A node called `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Sets its source and compiled files.
    #[must_use]
    pub fn in_file(mut self, file: Option<&str>, compiled_file: Option<&str>) -> Self {
        self.file = file.map(str::to_owned);
        self.compiled_file = compiled_file.map(str::to_owned);
        self
    }

    /// Sets the language its code is in.
    #[must_use]
    pub fn in_language(mut self, language: Option<&str>) -> Self {
        self.language = language.map(str::to_ascii_lowercase);
        self
    }

    /// Sets the macros it calls that aren't defined.
    #[must_use]
    pub fn calling_undefined(mut self, calls: Vec<NameAt>) -> Self {
        self.undefined_calls = calls;
        self
    }
}

/// The project, as explanations need it: its nodes and macros. Built by the provider
/// from its project format; everything in it is a name from the project's own code,
/// never a value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProjectIndex {
    /// Node id → what is known about it.
    pub nodes: BTreeMap<String, IndexedNode>,
    /// Every macro the project and its installed packages define, by name.
    pub macros: BTreeSet<String>,
}

impl ProjectIndex {
    /// An index of a project that defines `macros`, with no nodes yet.
    pub fn new<I, S>(macros: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            nodes: BTreeMap::new(),
            macros: macros.into_iter().map(Into::into).collect(),
        }
    }

    /// Adds a node.
    #[must_use]
    pub fn with_node(mut self, id: impl Into<String>, node: IndexedNode) -> Self {
        self.nodes.insert(id.into(), node);
        self
    }
}

/// A provider's error patterns.
pub trait ErrorCatalogue: Provider {
    /// Its name, version and engine.
    fn catalogue(&self) -> CatalogueInfo;

    /// Classifies a failed node's error summary (see the module docs).
    fn classify(&self, error: &ErrorSummary) -> Classification;
}
