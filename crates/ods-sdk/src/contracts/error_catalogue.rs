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
//! each node's file, the macros its code calls that aren't defined, and, for a check
//! (e.g. a data test), what it tests ([`CheckTarget`]: the kind of test, the column and
//! the node, never its arguments), and which nodes others refer to by name
//! ([`IndexedNode::referable`]). That is the provider's reading of its own project
//! format.

use std::collections::{BTreeMap, BTreeSet};

use ods_core::SchemaVersion;
use ods_core::failure::{ErrorCategory, Suggestion, Symptom, Text, is_code};
use serde::{Deserialize, Serialize};

use crate::contracts::run_events::ErrorSummary;
use crate::provider::{Contract, Provider};

/// The `error_catalogue` contract.
pub const ERROR_CATALOGUE: Contract = Contract {
    name: "error_catalogue",
    // 0.2: an indexed node may say what it checks (`IndexedNode::check`), and a pattern
    // may offer a step for running it again (`PatternMatch::rerun`) (#323).
    // 0.3: an indexed node may say other nodes refer to it by name
    // (`IndexedNode::referable`), for did-you-mean on a reference to a missing node.
    version: SchemaVersion::new(0, 3),
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
    /// A name the pattern read, e.g. a Python exception's type, or the name a reference
    /// to a missing node used (to compare with the project's own names, never shown).
    /// Only identifier-shaped names are kept ([`is_code`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// A step for when what failed runs again, with an argument for the engine (#323).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun: Option<Rerun>,
}

/// A step that runs what failed again with one more argument for the engine, passed
/// through after `--` (#323): e.g. dbt's `--store-failures`, which keeps a failed test's
/// rows in a table to look at. The catalogue knows the engine's argument, not ODS's
/// command or the node: the host builds the command for the node it would run again
/// (e.g. `ods state test --select orders -- --store-failures`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Rerun {
    /// What it does, for people; the host's command follows it.
    pub text: Text,
    /// The engine's argument; always [code](is_code).
    pub passthrough: String,
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
            rerun: None,
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

    /// Offers running it again with the engine's argument `passthrough`, which does
    /// what `text` says. An argument that isn't [code](is_code) is dropped.
    #[must_use]
    pub fn rerun_with(mut self, text: Text, passthrough: &str) -> Self {
        self.rerun = is_code(passthrough).then(|| Rerun {
            text,
            passthrough: passthrough.to_owned(),
        });
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

/// What a check (e.g. a data test) tests, as the project declares it (#323). Names
/// only: a test's arguments (e.g. the values `accepted_values` accepts) are values, so
/// they are never kept. Each name is kept only when it is [code](is_code).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct CheckTarget {
    /// The kind of test, for a generic one (e.g. `not_null`, `unique`,
    /// `accepted_values`, `relationships`, or a package's `dbt_utils.expression_is_true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
    /// The column it tests, when it tests one column by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    /// The node it is declared on, by id, when the project says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Whether the project declares it as a test of its own (a singular test, named by
    /// its file), rather than an instance of a generic test. Only a singular test's
    /// name may be shown: a generic test's may be made of its arguments.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub singular: bool,
}

impl CheckTarget {
    /// A check of `test` kind on `column` of `node`; a name that isn't [code](is_code)
    /// (e.g. a column given as an expression) is left out.
    pub fn new(test: Option<&str>, column: Option<&str>, node: Option<&str>) -> Self {
        let keep = |name: Option<&str>| name.filter(|n| is_code(n)).map(str::to_owned);
        Self {
            test: keep(test),
            column: keep(column),
            node: keep(node),
            singular: false,
        }
    }

    /// Says it is a singular test: one of its own, not a generic test's instance.
    #[must_use]
    pub fn singular(mut self) -> Self {
        self.singular = true;
        self
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
    /// What it checks, when it is a check (e.g. a data test).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckTarget>,
    /// Whether other nodes refer to it by its name (e.g. dbt's models, seeds and
    /// snapshots, which `ref()` names), so a reference to a missing node may have meant
    /// it (0.3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub referable: bool,
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

    /// Says it is a check, of `target`.
    #[must_use]
    pub fn checking(mut self, target: CheckTarget) -> Self {
        self.check = Some(target);
        self
    }

    /// Says other nodes refer to it by its name.
    #[must_use]
    pub fn referable(mut self) -> Self {
        self.referable = true;
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
