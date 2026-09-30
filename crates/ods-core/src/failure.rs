//! Why a node failed, in plain language (#323, ADR-0025).
//!
//! The vocabulary every engine's errors are explained in: a [category](ErrorCategory),
//! a [symptom](Symptom) when a provider's pattern recognised the error, how sure ODS is
//! ([`Confidence`]), and an [`ErrorExplanation`] people read: a headline, the evidence
//! behind it, where, what to try, and the impact downstream.
//!
//! Nothing here matches an engine's text: providers own their error patterns (AGENTS.md
//! rule 1). What this module guarantees:
//! - **Confidence is derived, never asserted.** [`ExplanationBuilder::build`] decides
//!   it: no recognised pattern is [`Confidence::NotRecognised`], a pattern that ODS's
//!   own evidence confirms is [`Confidence::KnownPatternWithEvidence`], and a pattern
//!   alone is [`Confidence::KnownPattern`]. An unrecognised error carries no symptom
//!   and no confirming evidence: ODS doesn't guess a cause (rule 3).
//! - **No values** (rule 9). Text is built from fixed wording, numbers, and
//!   [code spans](Text::code) that only hold identifier-shaped names (node, column and
//!   macro names, paths, run ids). The engine's own message is kept only in its
//!   redacted form ([`EngineMessage`]), and commands only hold the same characters.

use serde::{Deserialize, Serialize};

use crate::SchemaVersion;

/// The version of [`ErrorExplanation`] as serialized (e.g. in `--output json`).
pub const EXPLANATION_SCHEMA_VERSION: SchemaVersion = SchemaVersion::new(1, 0);

/// What kind of failure it was, for people: the chip on a failed node. Coarse on
/// purpose, so an engine's error kinds map onto it without guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCategory {
    /// The code couldn't be turned into a query (templating, macros, parsing).
    Compilation,
    /// A reference, package or other dependency couldn't be resolved.
    Dependency,
    /// The warehouse rejected or failed the query.
    Database,
    /// Not allowed: missing grants, privileges or access.
    Permission,
    /// It ran out of time or waited on a lock or a conflicting write.
    Timeout,
    /// A Python model raised an exception.
    PythonModel,
    /// A check (data test) on it failed.
    TestFailure,
    /// The project's configuration, profile or credentials.
    Configuration,
    /// The engine itself failed.
    Internal,
    /// The engine didn't say what kind of error it was.
    Unknown,
}

impl ErrorCategory {
    /// Every category, for documentation and tests.
    pub const ALL: [ErrorCategory; 10] = [
        Self::Compilation,
        Self::Dependency,
        Self::Database,
        Self::Permission,
        Self::Timeout,
        Self::PythonModel,
        Self::TestFailure,
        Self::Configuration,
        Self::Internal,
        Self::Unknown,
    ];

    /// How people read it, e.g. `database error`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Compilation => "compilation error",
            Self::Dependency => "dependency or ref",
            Self::Database => "database error",
            Self::Permission => "permission",
            Self::Timeout => "timeout or lock",
            Self::PythonModel => "python model",
            Self::TestFailure => "test failure",
            Self::Configuration => "configuration or profile",
            Self::Internal => "internal error",
            Self::Unknown => "error",
        }
    }

    /// What an unrecognised error of this category is said to be: what happened, never
    /// why.
    pub fn neutral_headline(self) -> &'static str {
        match self {
            Self::Compilation => "The model's code couldn't be compiled",
            Self::Dependency => "A dependency couldn't be resolved",
            Self::Database => "The warehouse rejected the query",
            Self::Permission => "The warehouse refused access",
            Self::Timeout => "The query timed out or waited too long",
            Self::PythonModel => "The Python model raised an error",
            Self::TestFailure => "A test on this node failed",
            Self::Configuration => "The project's configuration couldn't be used",
            Self::Internal => "The engine failed",
            Self::Unknown => "The node failed",
        }
    }
}

/// What a recognised error means, whatever engine reported it. A provider's pattern
/// names one; ODS then looks for its own evidence of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Symptom {
    /// A column the query reads doesn't exist.
    MissingColumn,
    /// A table or view the query reads doesn't exist.
    MissingRelation,
    /// A macro or function the code calls isn't defined.
    UnknownMacro,
    /// The code refers to a node the project doesn't have.
    MissingRef,
    /// The template's syntax is invalid.
    TemplateSyntax,
    /// The project's packages aren't installed.
    PackagesMissing,
    /// Access was denied.
    PermissionDenied,
    /// A value's type doesn't fit (a cast or a comparison).
    TypeMismatch,
    /// A constraint (e.g. not null, unique, primary key) was violated.
    ConstraintViolation,
    /// Another object depends on the one being replaced.
    DependentObjects,
    /// The query ran out of time.
    QueryTimeout,
    /// A lock or a concurrent write got in the way.
    LockConflict,
    /// The warehouse or cluster couldn't be reached or wasn't running.
    WarehouseUnavailable,
    /// The profile or target named isn't defined.
    ProfileNotFound,
    /// Credentials are missing or were rejected.
    CredentialsMissing,
    /// A Python model raised an exception.
    PythonException,
    /// A data test failed.
    TestFailed,
}

impl Symptom {
    /// Every symptom, for documentation and tests.
    pub const ALL: [Symptom; 17] = [
        Self::MissingColumn,
        Self::MissingRelation,
        Self::UnknownMacro,
        Self::MissingRef,
        Self::TemplateSyntax,
        Self::PackagesMissing,
        Self::PermissionDenied,
        Self::TypeMismatch,
        Self::ConstraintViolation,
        Self::DependentObjects,
        Self::QueryTimeout,
        Self::LockConflict,
        Self::WarehouseUnavailable,
        Self::ProfileNotFound,
        Self::CredentialsMissing,
        Self::PythonException,
        Self::TestFailed,
    ];

    /// How people read it, e.g. `missing column`.
    pub fn label(self) -> &'static str {
        match self {
            Self::MissingColumn => "missing column",
            Self::MissingRelation => "missing table or view",
            Self::UnknownMacro => "unknown macro",
            Self::MissingRef => "missing ref",
            Self::TemplateSyntax => "template syntax",
            Self::PackagesMissing => "packages not installed",
            Self::PermissionDenied => "permission denied",
            Self::TypeMismatch => "type mismatch",
            Self::ConstraintViolation => "constraint violated",
            Self::DependentObjects => "dependent objects",
            Self::QueryTimeout => "query timeout",
            Self::LockConflict => "lock or concurrent write",
            Self::WarehouseUnavailable => "warehouse unavailable",
            Self::ProfileNotFound => "profile or target not found",
            Self::CredentialsMissing => "credentials",
            Self::PythonException => "python exception",
            Self::TestFailed => "test failed",
        }
    }

    /// The category it belongs to when the engine doesn't say better.
    pub fn category(self) -> ErrorCategory {
        match self {
            Self::MissingColumn
            | Self::MissingRelation
            | Self::TypeMismatch
            | Self::ConstraintViolation
            | Self::DependentObjects => ErrorCategory::Database,
            Self::UnknownMacro | Self::TemplateSyntax => ErrorCategory::Compilation,
            Self::MissingRef | Self::PackagesMissing => ErrorCategory::Dependency,
            Self::PermissionDenied => ErrorCategory::Permission,
            Self::QueryTimeout | Self::LockConflict | Self::WarehouseUnavailable => {
                ErrorCategory::Timeout
            }
            Self::ProfileNotFound | Self::CredentialsMissing => ErrorCategory::Configuration,
            Self::PythonException => ErrorCategory::PythonModel,
            Self::TestFailed => ErrorCategory::TestFailure,
        }
    }

    /// What a recognised error with no evidence is said to be, in plain language.
    pub fn headline(self) -> &'static str {
        match self {
            Self::MissingColumn => "A column this model reads doesn't exist",
            Self::MissingRelation => "A table or view this model reads doesn't exist",
            Self::UnknownMacro => "The model calls a macro that isn't defined",
            Self::MissingRef => "The model refers to a node the project doesn't have",
            Self::TemplateSyntax => "The model's template has a syntax error",
            Self::PackagesMissing => "The project's packages aren't installed",
            Self::PermissionDenied => "The warehouse denied access",
            Self::TypeMismatch => "A value's type doesn't fit where it is used",
            Self::ConstraintViolation => "The data broke a constraint",
            Self::DependentObjects => "Other objects depend on the relation being replaced",
            Self::QueryTimeout => "The query ran out of time",
            Self::LockConflict => "A lock or a concurrent write got in the way",
            Self::WarehouseUnavailable => "The warehouse couldn't be reached",
            Self::ProfileNotFound => "The profile or target isn't defined",
            Self::CredentialsMissing => "Credentials are missing or were rejected",
            Self::PythonException => "The Python model raised an exception",
            Self::TestFailed => "A test on this node failed",
        }
    }
}

/// How sure ODS is of an explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Confidence {
    /// A recognised error, confirmed by ODS's own evidence.
    KnownPatternWithEvidence,
    /// A recognised error, with nothing of ODS's own to confirm it.
    KnownPattern,
    /// ODS doesn't recognise it, and doesn't guess a cause.
    NotRecognised,
}

impl Confidence {
    /// How people read it, e.g. `known pattern + evidence`.
    pub fn label(self) -> &'static str {
        match self {
            Self::KnownPatternWithEvidence => "known pattern + evidence",
            Self::KnownPattern => "known pattern",
            Self::NotRecognised => "not recognised",
        }
    }
}

/// Where a piece of evidence comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EvidenceSource {
    /// This run's plan: what was built and why.
    Plan,
    /// Code fingerprints: what changed since the last successful build.
    Fingerprint,
    /// Column-level lineage.
    ColumnLineage,
    /// The project's compiled description: its nodes, macros and files.
    Manifest,
    /// The versions of upstream data.
    SourceVersions,
    /// Earlier runs' journals and recorded state.
    RunHistory,
    /// This run's stats for the node.
    RunStats,
}

impl EvidenceSource {
    /// How people read it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Fingerprint => "fingerprints",
            Self::ColumnLineage => "column lineage",
            Self::Manifest => "manifest",
            Self::SourceVersions => "source versions",
            Self::RunHistory => "run history",
            Self::RunStats => "run stats",
        }
    }
}

/// The characters a [code span](Text::code) or a command may hold: those of
/// identifiers, paths, ids and ODS's own command lines. Anything else (quotes, spaces
/// inside a name, `;`, `$(`) could carry a value, so it never gets in.
fn code_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '/' | '$' | '@' | ':' | '+' | '=')
}

/// The longest code span or command kept.
const MAX_CODE_CHARS: usize = 200;

/// Text for people with code spans marked by backticks, e.g. ``Column lineage:
/// `stg_customers` now outputs `given_name` ``. Only fixed wording and numbers are
/// plain; names go in code spans, which hold only identifier-shaped text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Text(String);

impl Text {
    /// Empty text.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds plain wording. Backticks and control characters are dropped, so plain
    /// text can't open a code span.
    #[must_use]
    pub fn plain(mut self, words: &str) -> Self {
        self.0
            .extend(words.chars().filter(|c| *c != '`' && !c.is_control()));
        self
    }

    /// Adds a code span: a name, path or id. Text that isn't identifier-shaped (see
    /// the module docs) reads `[name hidden]` instead.
    #[must_use]
    pub fn code(mut self, name: &str) -> Self {
        if is_code(name) {
            self.0.push('`');
            self.0.push_str(name);
            self.0.push('`');
        } else {
            self.0.push_str("[name hidden]");
        }
        self
    }

    /// The text with its markup, e.g. for JSON.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether it is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The text in parts: `(text, is_code)`, in order, for renderers.
    pub fn parts(&self) -> Vec<(&str, bool)> {
        self.0
            .split('`')
            .enumerate()
            .filter(|(_, part)| !part.is_empty())
            .map(|(i, part)| (part, i % 2 == 1))
            .collect()
    }

    /// The text without markup.
    pub fn plain_text(&self) -> String {
        self.0.replace('`', "")
    }
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `name` may be shown as code: 1 to 200 identifier-shaped characters.
pub fn is_code(name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= MAX_CODE_CHARS && name.chars().all(code_char)
}

/// One reason ODS gives for an explanation, or one fact it knows about the failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct EvidenceItem {
    /// Where it comes from.
    pub source: EvidenceSource,
    /// What it says.
    pub text: Text,
    /// Whether it confirms the recognised pattern (e.g. lineage shows the column is
    /// gone), rather than adding context (e.g. how long the node ran).
    pub confirms: bool,
}

impl EvidenceItem {
    /// Context: a fact about the failure that confirms nothing.
    pub fn context(source: EvidenceSource, text: Text) -> Self {
        Self {
            source,
            text,
            confirms: false,
        }
    }

    /// Evidence that confirms the recognised pattern.
    pub fn confirming(source: EvidenceSource, text: Text) -> Self {
        Self {
            source,
            text,
            confirms: true,
        }
    }
}

/// Where the failure is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Location {
    /// The node's source file, relative to the project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The line in the source file, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The node's compiled file, relative to the project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compiled_file: Option<String>,
    /// The line the engine reported, in the code it ran (the compiled code as it was
    /// submitted, which an engine may wrap: it isn't a line of the source file).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_line: Option<u32>,
}

impl Location {
    /// A location in `file`, if it can be shown (see [`is_code`]).
    pub fn in_file(file: &str) -> Self {
        Self {
            file: is_code(file).then(|| file.to_owned()),
            ..Self::default()
        }
    }

    /// Sets the source line.
    #[must_use]
    pub fn at_line(mut self, line: Option<u32>) -> Self {
        self.line = line;
        self
    }

    /// Sets the compiled file, and the line the engine reported in the code it ran.
    #[must_use]
    pub fn compiled(mut self, file: Option<&str>, reported_line: Option<u32>) -> Self {
        self.compiled_file = file.filter(|f| is_code(f)).map(str::to_owned);
        self.reported_line = reported_line;
        self
    }

    /// Whether it says nothing.
    pub fn is_empty(&self) -> bool {
        self.file.is_none() && self.compiled_file.is_none() && self.reported_line.is_none()
    }
}

/// Something to try, with commands to copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Suggestion {
    /// What to do.
    pub text: Text,
    /// Commands that do it, in order; each is safe to show and copy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

impl Suggestion {
    /// A suggestion.
    pub fn new(text: Text) -> Self {
        Self {
            text,
            commands: Vec::new(),
        }
    }

    /// Adds a command. Only words of identifier-shaped characters, separated by single
    /// spaces, and `&&` between commands are kept; any other command is dropped.
    #[must_use]
    pub fn with_command(mut self, command: &str) -> Self {
        let safe = !command.is_empty()
            && command.chars().count() <= MAX_CODE_CHARS
            && command
                .split(' ')
                .all(|word| word == "&&" || (!word.is_empty() && word.chars().all(code_char)));
        if safe {
            self.commands.push(command.to_owned());
        }
        self
    }
}

/// What the failure did downstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct Impact {
    /// The nodes it blocked (skipped because it failed), by id, sorted.
    pub blocked: Vec<String>,
    /// Of those, the ones that keep an earlier successful build (AGENTS.md rule 5).
    pub kept: Vec<String>,
}

/// The engine's own message, as ODS keeps it: its kind and first line with values and
/// SQL removed, and where the full text is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct EngineMessage {
    /// Who said it, e.g. the engine's name.
    pub engine: String,
    /// The error's kind, if the engine gave one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The redacted first line.
    pub message: String,
    /// Where the full message is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details_at: Option<String>,
}

impl EngineMessage {
    /// A message. Every field must already be redacted (e.g. an engine's error summary);
    /// this only drops control characters.
    pub fn new(engine: &str, kind: Option<&str>, message: &str, details_at: Option<&str>) -> Self {
        let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>();
        Self {
            engine: clean(engine),
            kind: kind.map(clean),
            message: clean(message),
            details_at: details_at.map(clean),
        }
    }
}

/// A column a node reads from an upstream node that, as column lineage shows, doesn't
/// produce it: the lineage evidence for a [missing column](Symptom::MissingColumn).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct MissingColumn {
    /// The upstream node, by id.
    pub upstream: String,
    /// The column the node reads from it.
    pub column: String,
    /// The upstream's columns whose values come straight from a column of that name
    /// (so it was probably renamed to them), sorted.
    pub renamed_to: Vec<String>,
}

impl MissingColumn {
    /// A missing column.
    pub fn new(
        upstream: impl Into<String>,
        column: impl Into<String>,
        renamed_to: Vec<String>,
    ) -> Self {
        let mut renamed_to = renamed_to;
        renamed_to.sort();
        renamed_to.dedup();
        Self {
            upstream: upstream.into(),
            column: column.into(),
            renamed_to,
        }
    }
}

/// Which pattern recognised the error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct PatternRef {
    /// The catalogue's name.
    pub catalogue: String,
    /// Its version.
    pub version: String,
    /// The pattern's stable id within it.
    pub id: String,
}

impl PatternRef {
    /// A pattern reference.
    pub fn new(
        catalogue: impl Into<String>,
        version: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            catalogue: catalogue.into(),
            version: version.into(),
            id: id.into(),
        }
    }
}

/// A failed node, explained: what went wrong, why ODS thinks so, where, what to try,
/// and what it blocked. Made only by [`ExplanationBuilder`], which derives the
/// confidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ErrorExplanation {
    schema_version: SchemaVersion,
    node: String,
    category: ErrorCategory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    symptom: Option<Symptom>,
    confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pattern: Option<PatternRef>,
    headline: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<Text>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    evidence: Vec<EvidenceItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    location: Option<Location>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    suggestions: Vec<Suggestion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    impact: Option<Impact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    engine_message: Option<EngineMessage>,
}

impl ErrorExplanation {
    /// The failed node's id.
    pub fn node(&self) -> &str {
        &self.node
    }

    /// Its category.
    pub fn category(&self) -> ErrorCategory {
        self.category
    }

    /// The recognised symptom; `None` when not recognised.
    pub fn symptom(&self) -> Option<Symptom> {
        self.symptom
    }

    /// How sure ODS is.
    pub fn confidence(&self) -> Confidence {
        self.confidence
    }

    /// The pattern that recognised it.
    pub fn pattern(&self) -> Option<&PatternRef> {
        self.pattern.as_ref()
    }

    /// The headline.
    pub fn headline(&self) -> &Text {
        &self.headline
    }

    /// A sentence under the headline.
    pub fn detail(&self) -> Option<&Text> {
        self.detail.as_ref()
    }

    /// Why ODS thinks so; for an unrecognised error, what ODS knows.
    pub fn evidence(&self) -> &[EvidenceItem] {
        &self.evidence
    }

    /// Where.
    pub fn location(&self) -> Option<&Location> {
        self.location.as_ref()
    }

    /// What to try, most useful first.
    pub fn suggestions(&self) -> &[Suggestion] {
        &self.suggestions
    }

    /// What it blocked.
    pub fn impact(&self) -> Option<&Impact> {
        self.impact.as_ref()
    }

    /// The engine's own (redacted) message.
    pub fn engine_message(&self) -> Option<&EngineMessage> {
        self.engine_message.as_ref()
    }

    /// The label of its chip, e.g. `database error · missing column`.
    pub fn chip(&self) -> String {
        match self.symptom {
            Some(symptom) => format!("{} · {}", self.category.label(), symptom.label()),
            None => self.category.label().to_owned(),
        }
    }
}

/// Builds an [`ErrorExplanation`], deriving its confidence (see the module docs).
#[derive(Debug, Clone)]
pub struct ExplanationBuilder {
    explanation: ErrorExplanation,
}

impl ExplanationBuilder {
    /// An explanation of `node`'s failure, of `category`, not (yet) recognised.
    pub fn new(node: impl Into<String>, category: ErrorCategory) -> Self {
        Self {
            explanation: ErrorExplanation {
                schema_version: EXPLANATION_SCHEMA_VERSION,
                node: node.into(),
                category,
                symptom: None,
                confidence: Confidence::NotRecognised,
                pattern: None,
                headline: Text::new().plain(category.neutral_headline()),
                detail: None,
                evidence: Vec::new(),
                location: None,
                suggestions: Vec::new(),
                impact: None,
                engine_message: None,
            },
        }
    }

    /// Marks it recognised by `pattern` as `symptom`, with the symptom's headline.
    #[must_use]
    pub fn recognised(mut self, pattern: PatternRef, symptom: Symptom) -> Self {
        self.explanation.pattern = Some(pattern);
        self.explanation.symptom = Some(symptom);
        self.explanation.headline = Text::new().plain(symptom.headline());
        self
    }

    /// Whether a pattern recognised it.
    pub fn is_recognised(&self) -> bool {
        self.explanation.pattern.is_some()
    }

    /// Replaces the headline. Only a recognised error's headline can say more than its
    /// category's neutral one.
    #[must_use]
    pub fn headline(mut self, headline: Text) -> Self {
        if self.is_recognised() {
            self.explanation.headline = headline;
        }
        self
    }

    /// Sets the sentence under the headline.
    #[must_use]
    pub fn detail(mut self, detail: Text) -> Self {
        self.explanation.detail = Some(detail);
        self
    }

    /// Adds evidence, keeping the first of any two that say the same.
    #[must_use]
    pub fn evidence(mut self, item: EvidenceItem) -> Self {
        if !self
            .explanation
            .evidence
            .iter()
            .any(|e| e.text == item.text)
        {
            self.explanation.evidence.push(item);
        }
        self
    }

    /// Sets where.
    #[must_use]
    pub fn location(mut self, location: Location) -> Self {
        self.explanation.location = (!location.is_empty()).then_some(location);
        self
    }

    /// Adds something to try, unless an earlier suggestion says the same.
    #[must_use]
    pub fn suggest(mut self, suggestion: Suggestion) -> Self {
        if !self
            .explanation
            .suggestions
            .iter()
            .any(|s| s.text == suggestion.text)
        {
            self.explanation.suggestions.push(suggestion);
        }
        self
    }

    /// Sets the impact downstream; blocked and kept nodes are sorted.
    #[must_use]
    pub fn impact(mut self, mut blocked: Vec<String>, mut kept: Vec<String>) -> Self {
        blocked.sort();
        blocked.dedup();
        kept.sort();
        kept.dedup();
        kept.retain(|k| blocked.contains(k));
        self.explanation.impact = (!blocked.is_empty()).then_some(Impact { blocked, kept });
        self
    }

    /// Sets the engine's own message.
    #[must_use]
    pub fn engine_message(mut self, message: EngineMessage) -> Self {
        self.explanation.engine_message = Some(message);
        self
    }

    /// The explanation. Its confidence: not recognised without a pattern (and then no
    /// evidence confirms anything), known pattern + evidence when some evidence confirms
    /// the pattern, known pattern otherwise.
    pub fn build(mut self) -> ErrorExplanation {
        let e = &mut self.explanation;
        e.confidence = if e.pattern.is_none() {
            e.symptom = None;
            for item in &mut e.evidence {
                item.confirms = false;
            }
            Confidence::NotRecognised
        } else if e.evidence.iter().any(|i| i.confirms) {
            Confidence::KnownPatternWithEvidence
        } else {
            Confidence::KnownPattern
        };
        // Confirming evidence first: it is what the confidence rests on.
        e.evidence.sort_by_key(|i| !i.confirms);
        self.explanation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern() -> PatternRef {
        PatternRef::new("fake", "1", "missing-column")
    }

    #[test]
    fn confidence_is_derived_from_the_pattern_and_the_evidence() {
        let bare = ExplanationBuilder::new("model.a", ErrorCategory::Database).build();
        assert_eq!(bare.confidence(), Confidence::NotRecognised);
        assert_eq!(bare.headline().as_str(), "The warehouse rejected the query");
        assert_eq!(bare.chip(), "database error");

        let known = ExplanationBuilder::new("model.a", ErrorCategory::Database)
            .recognised(pattern(), Symptom::MissingColumn)
            .evidence(EvidenceItem::context(
                EvidenceSource::RunStats,
                Text::new().plain("It failed after 2s."),
            ))
            .build();
        assert_eq!(known.confidence(), Confidence::KnownPattern);
        assert_eq!(known.chip(), "database error · missing column");

        let confirmed = ExplanationBuilder::new("model.a", ErrorCategory::Database)
            .recognised(pattern(), Symptom::MissingColumn)
            .evidence(EvidenceItem::context(
                EvidenceSource::RunStats,
                Text::new().plain("context"),
            ))
            .evidence(EvidenceItem::confirming(
                EvidenceSource::ColumnLineage,
                Text::new().plain("gone"),
            ))
            .build();
        assert_eq!(confirmed.confidence(), Confidence::KnownPatternWithEvidence);
        assert!(
            confirmed.evidence()[0].confirms,
            "confirming evidence first"
        );
    }

    #[test]
    fn an_unrecognised_error_never_gets_a_cause() {
        let e = ExplanationBuilder::new("model.a", ErrorCategory::Database)
            .headline(Text::new().plain("A guessed cause"))
            .evidence(EvidenceItem::confirming(
                EvidenceSource::ColumnLineage,
                Text::new().plain("a guess"),
            ))
            .build();
        assert_eq!(e.confidence(), Confidence::NotRecognised);
        assert_eq!(e.symptom(), None);
        assert_eq!(e.headline().as_str(), "The warehouse rejected the query");
        assert!(e.evidence().iter().all(|i| !i.confirms));
    }

    #[test]
    fn code_spans_and_commands_hold_only_identifier_shaped_text() {
        let t = Text::new()
            .plain("reads `")
            .code("stg_customers")
            .plain(" and ")
            .code("'sk_live_1'");
        assert_eq!(t.as_str(), "reads `stg_customers` and [name hidden]");
        assert_eq!(
            t.parts(),
            vec![
                ("reads ", false),
                ("stg_customers", true),
                (" and [name hidden]", false)
            ]
        );
        let s = Suggestion::new(Text::new().plain("x"))
            .with_command("ods lineage impact --column stg_customers.first_name=removed")
            .with_command("ods state retry --failed")
            .with_command("dbt deps && ods state retry --failed")
            .with_command("echo 'sk_live_1'")
            .with_command("rm -rf / ; true")
            .with_command("a  b");
        assert_eq!(
            s.commands,
            vec![
                "ods lineage impact --column stg_customers.first_name=removed",
                "ods state retry --failed",
                "dbt deps && ods state retry --failed"
            ]
        );
    }

    #[test]
    fn impact_keeps_only_blocked_nodes_sorted() {
        let e = ExplanationBuilder::new("model.a", ErrorCategory::Database)
            .impact(
                vec!["model.c".into(), "model.b".into()],
                vec!["model.x".into(), "model.c".into()],
            )
            .build();
        let impact = e.impact().unwrap();
        assert_eq!(impact.blocked, vec!["model.b", "model.c"]);
        assert_eq!(impact.kept, vec!["model.c"]);
        let none = ExplanationBuilder::new("model.a", ErrorCategory::Database)
            .impact(Vec::new(), Vec::new())
            .build();
        assert!(none.impact().is_none());
    }

    #[test]
    fn serializes_in_snake_case_with_its_version() {
        let e = ExplanationBuilder::new("model.a", ErrorCategory::PythonModel)
            .recognised(pattern(), Symptom::PythonException)
            .build();
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["category"], "python_model");
        assert_eq!(json["symptom"], "python_exception");
        assert_eq!(json["confidence"], "known_pattern");
        assert_eq!(json["schema_version"]["major"], 1);
        let back: ErrorExplanation = serde_json::from_value(json).unwrap();
        assert_eq!(back, e);
    }
}
