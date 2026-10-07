//! dbt's error patterns (#323, ADR-0025): what dbt's errors, and the errors of the
//! adapters ODS is tested with, mean in ODS's neutral taxonomy.
//!
//! [`DbtErrorCatalogue`] classifies an [`ErrorSummary`], the redacted first line of a
//! failed node's message and its kind, as [`crate::events::error_summary`] makes it.
//! It never sees the raw message or the SQL. Every pattern is grounded in public text:
//! - dbt-core's own messages (Apache-2.0), e.g. `'x' is undefined. This can happen when
//!   calling a macro that does not exist`, `depends on a node named … which was not
//!   found`, `dbt found N package(s) specified in packages.yml, but only M …`;
//! - `DuckDB`'s error kinds and messages (MIT), as dbt-duckdb 1.10 reports them: every one
//!   is recorded from real runs of dbt 1.10, 1.11 and 1.12 in
//!   `fixtures/dbt/jaffle-ods/artifacts/dbt-<version>-errors`;
//! - PostgreSQL's documented messages (`column … does not exist`, `relation … does not
//!   exist`, `permission denied for …`);
//! - Apache Spark's public error conditions (Apache-2.0, `error-conditions.json`:
//!   `[UNRESOLVED_COLUMN…]`, `[TABLE_OR_VIEW_NOT_FOUND]`, `[SCHEMA_NOT_FOUND]`,
//!   `[UNRESOLVED_ROUTINE]`, `[CAST_INVALID_INPUT]`, `[DATATYPE_MISMATCH…]`,
//!   `[CHECK_CONSTRAINT_VIOLATION]`, `[NOT_NULL_CONSTRAINT_VIOLATION]`) and Delta Lake's
//!   (Apache-2.0, `delta-error-classes.json`: `[DELTA_CONCURRENT_…]`,
//!   `[DELTA_NOT_NULL_CONSTRAINT_VIOLATED]`, `[DELTA_VIOLATE_CONSTRAINT_WITH_VALUES]`,
//!   `[DELTA_NEW_NOT_NULL_VIOLATION]`, `[DELTA_NEW_CHECK_CONSTRAINT_VIOLATION]`), which
//!   Databricks reports: the ones its SQL warehouse gave dbt-databricks are recorded in
//!   `fixtures/dbt/jaffle-ods/artifacts/dbt-databricks-errors` (#349);
//!
//! A warehouse's own messages are its plugin's (ADR-0031 §3a): [`ProjectCatalogue`]
//! asks the project's warehouse catalogue first, then this one, and gives what either
//! recognises dbt's steps.
//!
//! Anything else is not recognised: the catalogue gives only the category dbt's (or the
//! adapter's) kind implies, and ODS doesn't guess a cause. Errors close to a symptom
//! get their own: a missing schema (Spark's `[SCHEMA_NOT_FOUND]`) isn't a missing table
//! or view, and a missing SQL function (`[UNRESOLVED_ROUTINE]`, after dbt compiled the
//! query) isn't an undefined macro, so neither's evidence can confirm the other.
//!
//! [`project_index`] describes a project for explanations: each node's files, the
//! macros its code calls that the manifest doesn't define, and what each data test
//! tests (its `test_metadata` name, `column_name` and `attached_node`; never its
//! `kwargs`, which hold values such as the ones `accepted_values` accepts).

use ods_core::failure::{ErrorCategory, Suggestion, Symptom, Text, is_code};
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::error_catalogue::{
    CatalogueInfo, CheckTarget, Classification, ErrorCatalogue, IndexedNode, NameAt, PatternMatch,
    ProjectIndex,
};
use ods_sdk::contracts::run_events::ErrorSummary;
use ods_sdk::{Provider, ProviderInfo};
use std::sync::Arc;

use crate::events::ProjectFailure;
use crate::{Manifest, ResourceType};

/// The catalogue's version: bumped whenever a pattern is added, changed or removed.
/// 3: `dbt-missing-ref` names the missing node when the whole message is at hand
/// ([`DbtErrorCatalogue::classify_project`]). 4: Databricks's unavailable compute,
/// timeouts and missing credentials, Spark's and Delta's constraint violations. 5:
/// missing schemas and SQL functions (`DuckDB`, PostgreSQL, Spark) as their own symptoms;
/// a pattern's kind may be dbt's header kind ([`ErrorSummary::outer_kind`]), which
/// `postgres-invalid-input` and `postgres-connect` need. 6: Delta's violations of a
/// constraint added to a table whose rows break it, as recorded from Databricks. 7:
/// dbt-databricks's own messages move to the Databricks plugin's catalogue
/// ([`ProjectCatalogue`] consults it first, ADR-0031 §3a).
pub const CATALOGUE_VERSION: &str = "7";

/// How a pattern recognises a summary: its kind (lowercased, exactly: the message's own,
/// or the one dbt's header gave around it), and phrases its lowercased message must
/// hold, all of them.
struct Pattern {
    id: &'static str,
    symptom: Symptom,
    /// The summary's kind, if the pattern needs one.
    kind: Option<&'static str>,
    /// Phrases the message holds, all of them.
    all: &'static [&'static str],
}

const fn p(
    id: &'static str,
    symptom: Symptom,
    kind: Option<&'static str>,
    all: &'static [&'static str],
) -> Pattern {
    Pattern {
        id,
        symptom,
        kind,
        all,
    }
}

/// The patterns, most specific first. Messages are lowercase and already redacted:
/// quoted names read `[value removed]`, and so do numbers.
const PATTERNS: &[Pattern] = &[
    // dbt's own compilation and dependency errors.
    p(
        "dbt-undefined-macro",
        Symptom::UnknownMacro,
        None,
        &["is undefined", "macro that does not exist"],
    ),
    p(
        "dbt-missing-ref",
        Symptom::MissingRef,
        None,
        &["depends on a node named", "which was not found"],
    ),
    p(
        "dbt-packages-not-installed",
        Symptom::PackagesMissing,
        None,
        &["specified in packages.yml, but only", "installed in"],
    ),
    // dbt 1.12's wording of the same failure (recorded in `dbt-1.12-errors`).
    p(
        "dbt-packages-expected",
        Symptom::PackagesMissing,
        None,
        &[
            "based on packages specified in packages.yml, but found only",
            "installed in",
        ],
    ),
    p(
        "dbt-profile-not-found",
        Symptom::ProfileNotFound,
        None,
        &["could not find profile named"],
    ),
    p(
        "dbt-target-not-found",
        Symptom::ProfileNotFound,
        None,
        &["does not have a target named"],
    ),
    p(
        "dbt-template-unexpected",
        Symptom::TemplateSyntax,
        Some("compilation error"),
        &["unexpected "],
    ),
    p(
        "dbt-template-expected-token",
        Symptom::TemplateSyntax,
        Some("compilation error"),
        &["expected token"],
    ),
    p(
        "dbt-template-unknown-tag",
        Symptom::TemplateSyntax,
        Some("compilation error"),
        &["unknown tag"],
    ),
    p(
        "dbt-python-model",
        Symptom::PythonException,
        None,
        &["python model failed"],
    ),
    p(
        "dbt-test-failed",
        Symptom::TestFailed,
        None,
        &["configured to fail if"],
    ),
    // `DuckDB`, as dbt-duckdb reports it.
    p(
        "duckdb-values-list-column",
        Symptom::MissingColumn,
        Some("binder error"),
        &["does not have a column named"],
    ),
    p(
        "duckdb-referenced-column",
        Symptom::MissingColumn,
        Some("binder error"),
        &["referenced column", "not found"],
    ),
    p(
        "duckdb-table-missing",
        Symptom::MissingRelation,
        Some("catalog error"),
        &["table with name", "does not exist"],
    ),
    p(
        "duckdb-view-missing",
        Symptom::MissingRelation,
        Some("catalog error"),
        &["view with name", "does not exist"],
    ),
    p(
        "duckdb-schema-missing",
        Symptom::MissingSchema,
        Some("catalog error"),
        &["schema with name", "does not exist"],
    ),
    // Recorded in every `dbt-<version>-errors` (`missing-function`).
    p(
        "duckdb-function-missing",
        Symptom::MissingFunction,
        Some("catalog error"),
        &["function with name", "does not exist"],
    ),
    p(
        "duckdb-conversion",
        Symptom::TypeMismatch,
        Some("conversion error"),
        &[],
    ),
    p(
        "duckdb-constraint",
        Symptom::ConstraintViolation,
        Some("constraint error"),
        &[],
    ),
    p(
        "duckdb-dependent-entries",
        Symptom::DependentObjects,
        Some("dependency error"),
        &["because there are entries that depend on it"],
    ),
    p(
        "duckdb-write-conflict",
        Symptom::LockConflict,
        Some("transactioncontext error"),
        &["conflict"],
    ),
    p(
        "duckdb-file-lock",
        Symptom::LockConflict,
        None,
        &["could not set lock on file"],
    ),
    p(
        "duckdb-permission",
        Symptom::PermissionDenied,
        Some("permission error"),
        &[],
    ),
    p(
        "duckdb-interrupted",
        Symptom::QueryTimeout,
        Some("interrupt error"),
        &[],
    ),
    // PostgreSQL's documented messages (and adapters that share them).
    p(
        "postgres-column-missing",
        Symptom::MissingColumn,
        Some("database error"),
        &["column ", "does not exist"],
    ),
    p(
        "postgres-schema-missing",
        Symptom::MissingSchema,
        Some("database error"),
        &["schema ", "does not exist"],
    ),
    p(
        "postgres-function-missing",
        Symptom::MissingFunction,
        Some("database error"),
        &["function ", "does not exist"],
    ),
    p(
        "postgres-relation-missing",
        Symptom::MissingRelation,
        Some("database error"),
        &["relation ", "does not exist"],
    ),
    p(
        "postgres-permission-denied",
        Symptom::PermissionDenied,
        Some("database error"),
        &["permission denied for"],
    ),
    p(
        "postgres-statement-timeout",
        Symptom::QueryTimeout,
        Some("database error"),
        &["canceling statement due to statement timeout"],
    ),
    // PostgreSQL ends this phrase with a colon, so the summary takes it for the
    // message's kind; dbt's `Database Error` is its outer kind.
    p(
        "postgres-invalid-input",
        Symptom::TypeMismatch,
        Some("database error"),
        &["invalid input syntax for type"],
    ),
    p(
        "postgres-password",
        Symptom::CredentialsMissing,
        Some("database error"),
        &["password authentication failed"],
    ),
    p(
        "postgres-unique",
        Symptom::ConstraintViolation,
        Some("database error"),
        &["violates unique constraint"],
    ),
    p(
        "postgres-not-null",
        Symptom::ConstraintViolation,
        Some("database error"),
        &["violates not-null constraint"],
    ),
    p(
        "postgres-connect",
        Symptom::WarehouseUnavailable,
        Some("database error"),
        &["could not connect to server"],
    ),
    // Apache Spark's error conditions and Delta Lake's error classes (Databricks).
    p(
        "spark-unresolved-column",
        Symptom::MissingColumn,
        None,
        &["[unresolved_column"],
    ),
    p(
        "spark-table-or-view-not-found",
        Symptom::MissingRelation,
        None,
        &["[table_or_view_not_found]"],
    ),
    p(
        "spark-cast-invalid-input",
        Symptom::TypeMismatch,
        None,
        &["[cast_invalid_input]"],
    ),
    p(
        "spark-datatype-mismatch",
        Symptom::TypeMismatch,
        None,
        &["[datatype_mismatch"],
    ),
    p(
        "spark-check-constraint",
        Symptom::ConstraintViolation,
        None,
        &["[check_constraint_violation]"],
    ),
    p(
        "spark-not-null-constraint",
        Symptom::ConstraintViolation,
        None,
        &["[not_null_constraint_violation]"],
    ),
    p(
        "delta-not-null-constraint",
        Symptom::ConstraintViolation,
        None,
        &["[delta_not_null_constraint_violated]"],
    ),
    p(
        "delta-check-constraint",
        Symptom::ConstraintViolation,
        None,
        &["[delta_violate_constraint_with_values]"],
    ),
    // A constraint added to a table whose rows already break it, as a dbt model
    // contract's constraints are (recorded from Databricks).
    p(
        "delta-new-not-null-constraint",
        Symptom::ConstraintViolation,
        None,
        &["[delta_new_not_null_violation]"],
    ),
    p(
        "delta-new-check-constraint",
        Symptom::ConstraintViolation,
        None,
        &["[delta_new_check_constraint_violation]"],
    ),
    p(
        "spark-schema-not-found",
        Symptom::MissingSchema,
        None,
        &["[schema_not_found]"],
    ),
    p(
        "spark-unresolved-routine",
        Symptom::MissingFunction,
        None,
        &["[unresolved_routine]"],
    ),
    p(
        "delta-concurrent-write",
        Symptom::LockConflict,
        None,
        &["[delta_concurrent_"],
    ),
];

/// dbt's error catalogue (see the module docs).
#[derive(Debug, Clone, Copy, Default)]
pub struct DbtErrorCatalogue;

impl DbtErrorCatalogue {
    /// The catalogue.
    pub fn new() -> Self {
        Self
    }
}

impl Provider for DbtErrorCatalogue {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            "dbt",
            "dbt-error-catalogue",
            CATALOGUE_VERSION,
            CapabilitySet::from_iter([Capability::ErrorExplain]),
        )
    }
}

/// The category an error's kind gives, without a pattern.
fn category_of(kind: Option<&str>, message: &str) -> ErrorCategory {
    let kind = kind.unwrap_or_default().to_ascii_lowercase();
    match kind.as_str() {
        "compilation error" | "parsing error" => ErrorCategory::Compilation,
        "dependency error" => ErrorCategory::Dependency,
        "python error" => ErrorCategory::PythonModel,
        // dbt's own, and `DuckDB`'s error kinds, as dbt-duckdb reports them.
        "database error"
        | "binder error"
        | "catalog error"
        | "conversion error"
        | "constraint error"
        | "parser error"
        | "invalid input error"
        | "out of range error"
        | "io error"
        | "transactioncontext error"
        | "transaction error"
        | "serialization error"
        | "not implemented error" => ErrorCategory::Database,
        "permission error" => ErrorCategory::Permission,
        "interrupt error" => ErrorCategory::Timeout,
        // An error class in brackets opens a Spark or Databricks message.
        _ if message.starts_with('[') => ErrorCategory::Database,
        _ => ErrorCategory::Unknown,
    }
}

/// The steps dbt offers for a symptom.
fn steps(found: PatternMatch) -> PatternMatch {
    match found.symptom {
        Symptom::UnknownMacro | Symptom::PackagesMissing => found.suggest(
            Suggestion::new(Text::new().plain("Install the project's packages:"))
                .with_command("dbt deps", &[]),
        ),
        Symptom::TemplateSyntax => found.suggest(Suggestion::new(Text::new().plain(
            "Check the Jinja around the reported line: an unclosed bracket, quote or tag.",
        ))),
        Symptom::ProfileNotFound | Symptom::CredentialsMissing | Symptom::WarehouseUnavailable => found.suggest(
            Suggestion::new(Text::new().plain("Check that dbt can connect with this profile:"))
                .with_command("dbt debug", &[]),
        ),
        // dbt keeps a test's failing rows in a table when asked (`--store-failures`).
        Symptom::TestFailed => found.rerun_with(
            Text::new().plain(
                "See the rows that fail: run the test again, keeping them in a table in the warehouse:",
            ),
            "--store-failures",
        ),
        _ => found,
    }
}

/// Python's built-in exceptions, the only ones an explanation names: any other name
/// (a project's own exception class) could be made of a value (#323 review).
const PYTHON_BUILTIN_EXCEPTIONS: &[&str] = &[
    "ArithmeticError",
    "AssertionError",
    "AttributeError",
    "BufferError",
    "ConnectionError",
    "EOFError",
    "Exception",
    "FileExistsError",
    "FileNotFoundError",
    "FloatingPointError",
    "ImportError",
    "IndexError",
    "KeyError",
    "LookupError",
    "MemoryError",
    "ModuleNotFoundError",
    "NameError",
    "NotImplementedError",
    "OSError",
    "OverflowError",
    "PermissionError",
    "RecursionError",
    "ReferenceError",
    "RuntimeError",
    "SyntaxError",
    "SystemError",
    "TimeoutError",
    "TypeError",
    "UnboundLocalError",
    "UnicodeDecodeError",
    "UnicodeEncodeError",
    "UnicodeError",
    "ValueError",
    "ZeroDivisionError",
];

/// The exception to name: a built-in one only.
fn named_exception(name: Option<&str>) -> Option<String> {
    name.filter(|n| PYTHON_BUILTIN_EXCEPTIONS.contains(n))
        .map(str::to_owned)
}

/// Whether `kind` names a Python exception: one CamelCase word ending in `Error` or
/// `Exception` (`KeyError`, `ZeroDivisionError`), unlike adapters' kinds, which have
/// spaces (`Binder Error`).
fn is_python_exception(kind: &str) -> bool {
    let named = |suffix: &str| kind.len() > suffix.len() && kind.ends_with(suffix);
    kind.starts_with(|c: char| c.is_ascii_uppercase())
        && kind.chars().all(|c| c.is_ascii_alphanumeric())
        && (named("Error") || named("Exception"))
}

/// The exception named in a Python model's summary, `Python model failed: KeyError: …`.
fn python_exception(message: &str) -> Option<String> {
    let rest = message.strip_prefix("Python model failed: ")?;
    let name = rest.split(':').next()?.trim();
    is_python_exception(name).then(|| name.to_owned())
}

impl DbtErrorCatalogue {
    /// Classifies a failure that stopped the whole project ([`project_failure`]) as
    /// [`classify`](ErrorCatalogue::classify) does its summary; a reference to a
    /// missing node also gets the name it used as its
    /// [subject](PatternMatch::subject), for did-you-mean against the project's own
    /// names. The name is compared, never shown (#323).
    ///
    /// [`project_failure`]: crate::events::project_failure
    pub fn classify_project(&self, failure: &ProjectFailure) -> Classification {
        match self.classify(&failure.summary) {
            Classification::Recognised(found) if found.symptom == Symptom::MissingRef => {
                Classification::Recognised(found.about(failure.missing_node.clone()))
            }
            other => other,
        }
    }
}

impl ErrorCatalogue for DbtErrorCatalogue {
    fn catalogue(&self) -> CatalogueInfo {
        CatalogueInfo::new("dbt", CATALOGUE_VERSION, "dbt")
    }

    fn classify(&self, error: &ErrorSummary) -> Classification {
        let kind = error.kind().map(str::to_ascii_lowercase);
        let outer = error.outer_kind().map(str::to_ascii_lowercase);
        let message = error.message().to_ascii_lowercase();
        let found = PATTERNS.iter().find(|pattern| {
            pattern
                .kind
                .is_none_or(|k| kind.as_deref() == Some(k) || outer.as_deref() == Some(k))
                && pattern.all.iter().all(|phrase| message.contains(phrase))
        });
        // A Python exception's name as the error's kind (`KeyError: …`): only a Python
        // model's code raises one into a node's result.
        let python = error.kind().filter(|k| is_python_exception(k));
        match found {
            None if python.is_some() => Classification::Recognised(steps(
                PatternMatch::new("python-exception", Symptom::PythonException)
                    .about(named_exception(python)),
            )),
            Some(pattern) => {
                let mut found = PatternMatch::new(pattern.id, pattern.symptom);
                if pattern.symptom == Symptom::PythonException {
                    found = found.about(named_exception(
                        python_exception(error.message()).as_deref(),
                    ));
                }
                Classification::Recognised(steps(found))
            }
            // The message's kind may be a phrase, not a kind: then dbt's says more.
            None => Classification::NotRecognised {
                category: match category_of(error.kind(), &message) {
                    ErrorCategory::Unknown => category_of(error.outer_kind(), &message),
                    known => known,
                },
            },
        }
    }
}

/// The catalogue for a project: its warehouse's catalogues, nearest first (its plugin's,
/// then the warehouses it is built on: ADR-0031 §3a, §3b), then dbt's. The first that
/// recognises an error wins, so a warehouse's own message is never read by a generic
/// pattern; either way, dbt's steps are offered, since dbt is what runs.
#[derive(Clone, Default)]
pub struct ProjectCatalogue {
    warehouse: Vec<Arc<dyn ErrorCatalogue>>,
}

impl std::fmt::Debug for ProjectCatalogue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ProjectCatalogue")
            .field(&self.catalogue())
            .finish()
    }
}

impl ProjectCatalogue {
    /// dbt's catalogue after `warehouse`'s, nearest first.
    pub fn new(warehouse: Vec<Arc<dyn ErrorCatalogue>>) -> Self {
        Self { warehouse }
    }

    /// What the first warehouse catalogue to recognise the error makes of it, with
    /// dbt's steps.
    fn warehouse_match(&self, error: &ErrorSummary) -> Option<Classification> {
        self.warehouse.iter().find_map(|c| match c.classify(error) {
            Classification::Recognised(found) => Some(Classification::Recognised(steps(found))),
            // Not recognised (or a kind of answer this host doesn't know): the next.
            _ => None,
        })
    }

    /// As [`DbtErrorCatalogue::classify_project`], after the warehouse's catalogue.
    pub fn classify_project(&self, failure: &ProjectFailure) -> Classification {
        self.warehouse_match(&failure.summary)
            .unwrap_or_else(|| DbtErrorCatalogue.classify_project(failure))
    }
}

impl Provider for ProjectCatalogue {
    fn info(&self) -> ProviderInfo {
        DbtErrorCatalogue.info()
    }
}

impl ErrorCatalogue for ProjectCatalogue {
    /// dbt's, with each warehouse catalogue's name and version after it (`7+databricks
    /// 1`), so an explanation says which patterns it came from.
    fn catalogue(&self) -> CatalogueInfo {
        let dbt = DbtErrorCatalogue.catalogue();
        let mut version = dbt.version;
        for w in &self.warehouse {
            let w = w.catalogue();
            version = format!("{version}+{} {}", w.name, w.version);
        }
        CatalogueInfo::new(dbt.name, version, dbt.engine)
    }

    fn classify(&self, error: &ErrorSummary) -> Classification {
        self.warehouse_match(error)
            .unwrap_or_else(|| DbtErrorCatalogue.classify(error))
    }
}

/// Names a model's code may call that are not macros: Jinja's builtins and dbt's Jinja
/// context, from dbt's public reference ("dbt Jinja functions") and Jinja's
/// documentation. Sorted.
const CONTEXT: &[&str] = &[
    "adapter",
    "api",
    "as_bool",
    "as_native",
    "as_number",
    "as_text",
    "builtins",
    "caller",
    "config",
    "context",
    "cycler",
    "database",
    "database_schemas",
    "dbt",
    "dbt_version",
    "debug",
    "dict",
    "diff_of_two_dicts",
    "dispatch",
    "doc",
    "env_var",
    "exceptions",
    "execute",
    "flags",
    "fromjson",
    "fromyaml",
    "graph",
    "invocation_args_dict",
    "invocation_id",
    "is_incremental",
    "joiner",
    "lipsum",
    "list",
    "load_relation",
    "load_result",
    "local_md5",
    "log",
    "loop",
    "metric",
    "model",
    "modules",
    "namespace",
    "print",
    "project_name",
    "range",
    "ref",
    "render",
    "results",
    "return",
    "run_query",
    "run_started_at",
    "schema",
    "schemas",
    "selected_resources",
    "set",
    "set_sql_header",
    "set_strict",
    "should_full_refresh",
    "source",
    "statement",
    "store_raw_result",
    "store_result",
    "super",
    "target",
    "this",
    "thread_id",
    "tojson",
    "toyaml",
    "try_or_compiler_error",
    "var",
    "write",
    "zip",
    "zip_strict",
];

/// A macro's name from its id, `macro.<package>.<name>`.
fn macro_name(id: &str) -> Option<&str> {
    let mut parts = id.splitn(3, '.');
    (parts.next()? == "macro").then_some(())?;
    parts.next()?;
    parts.next()
}

/// The macros a node's code calls, in order, with their lines: a name followed by `(`
/// inside `{{ … }}` or `{% … %}`, outside strings, not a method (after `.`) or a filter
/// (after `|`). A dotted call (`dbt_utils.star(`) is kept whole unless it starts with a
/// context object (`adapter.`, `dbt.`, …).
fn calls(code: &str) -> Vec<NameAt> {
    let mut found = Vec::new();
    let mut rest = code;
    let mut offset = 0;
    while let Some(open) = rest.find(['{']) {
        let after = &rest[open..];
        let close = if after.starts_with("{{") {
            "}}"
        } else if after.starts_with("{%") {
            "%}"
        } else {
            offset += open + 1;
            rest = &rest[open + 1..];
            continue;
        };
        let body_start = open + 2;
        let Some(len) = rest[body_start..].find(close) else {
            break;
        };
        let body = &rest[body_start..body_start + len];
        let line = u32::try_from(code[..offset + body_start].matches('\n').count() + 1).ok();
        found.extend(
            block_calls(body)
                .into_iter()
                .map(|name| NameAt::new(name, line)),
        );
        offset += body_start + len + 2;
        rest = &rest[body_start + len + 2..];
    }
    found
}

fn block_calls(body: &str) -> Vec<String> {
    let chars: Vec<char> = body.chars().collect();
    let mut calls = Vec::new();
    let mut i = 0;
    let mut previous = ' ';
    // The word before, when the character before is a space: `macro name(` defines,
    // and `is name(` tests; neither calls a macro.
    let mut word = String::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' {
            // Skip the string.
            i += 1;
            while i < chars.len() && chars[i] != c {
                i += 1;
            }
            i += 1;
            previous = ' ';
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            let mut j = i;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            let called = chars.get(j) == Some(&'(');
            let keyword = previous == 'a' && matches!(word.as_str(), "macro" | "is" | "call");
            if called && previous != '.' && previous != '|' && !keyword {
                calls.push(name.clone());
            }
            previous = 'a';
            word = name;
            continue;
        }
        if !c.is_whitespace() {
            previous = c;
        }
        i += 1;
    }
    calls
}

/// Whether `name` (possibly dotted) is defined as a macro or is in dbt's context.
/// A dotted call (`cols.append(`, `dbt_utils.star(`) counts only when its namespace is
/// a package that defines macros: anything else is a method on a value, which says
/// nothing about macros.
fn defined(
    name: &str,
    macros: &ProjectIndex,
    packaged: &std::collections::BTreeSet<String>,
    namespaces: &std::collections::BTreeSet<&str>,
) -> bool {
    match name.split_once('.') {
        Some((namespace, local)) => {
            !namespaces.contains(namespace)
                || local.contains('.')
                || packaged.contains(&format!("{namespace}.{local}"))
        }
        None => CONTEXT.binary_search(&name).is_ok() || macros.macros.contains(name),
    }
}

/// The project as explanations need it (see the module docs). `target` is the target
/// directory as the project names it (e.g. `target`), for compiled files.
pub fn project_index(manifest: &Manifest, target: Option<&str>) -> ProjectIndex {
    let names: Vec<&str> = manifest
        .macros
        .keys()
        .filter_map(|id| macro_name(id))
        .collect();
    let packaged = manifest
        .macros
        .keys()
        .filter_map(|id| id.strip_prefix("macro."))
        .map(str::to_owned)
        .collect();
    let namespaces: std::collections::BTreeSet<&str> = manifest
        .macros
        .keys()
        .filter_map(|id| id.strip_prefix("macro.")?.split('.').next())
        .collect();
    let mut index = ProjectIndex::new(names);
    for node in &manifest.nodes {
        let Some(name) = node.name.as_deref() else {
            continue;
        };
        let file = node.original_file_path.as_deref();
        // A generic test compiles to a file named after the test (and so, maybe, its
        // arguments) under its YAML file: no compiled file is named for it.
        let compiled = match (target, node.fqn.first(), file) {
            (Some(target), Some(package), Some(file))
                if matches!(
                    node.resource_type,
                    ResourceType::Model | ResourceType::Snapshot
                ) || (node.resource_type == ResourceType::Test && node.test.is_none()) =>
            {
                Some(format!("{target}/compiled/{package}/{file}"))
            }
            _ => None,
        };
        let undefined: Vec<NameAt> = node
            .raw_code
            .as_deref()
            .map(calls)
            .unwrap_or_default()
            .into_iter()
            .filter(|c| is_code(&c.name) && !defined(&c.name, &index, &packaged, &namespaces))
            .collect();
        let mut indexed = IndexedNode::new(name)
            .in_file(file, compiled.as_deref())
            .in_language(node.language.as_deref())
            .calling_undefined(undefined);
        if node.resource_type == ResourceType::Test {
            indexed = indexed.checking(check_target(node));
        }
        // What `ref()` can name.
        if matches!(
            node.resource_type,
            ResourceType::Model | ResourceType::Seed | ResourceType::Snapshot
        ) {
            indexed = indexed.referable();
        }
        index = index.with_node(node.unique_id.clone(), indexed);
    }
    index
}

/// What a data test tests: a generic test's name (with its package, e.g.
/// `dbt_utils.expression_is_true`), column and the node it is attached to; a singular
/// test's node when it reads exactly one. Its arguments are never read.
fn check_target(node: &crate::ManifestNode) -> CheckTarget {
    match &node.test {
        Some(test) => {
            let name = match &test.namespace {
                Some(namespace) => format!("{namespace}.{}", test.name),
                None => test.name.clone(),
            };
            CheckTarget::new(
                Some(&name),
                test.column_name.as_deref(),
                test.attached_node.as_deref(),
            )
        }
        // No `test_metadata`: a singular test, named by its file.
        None => match node.depends_on.as_slice() {
            [one] => CheckTarget::new(None, None, Some(one)),
            _ => CheckTarget::default(),
        }
        .singular(),
    }
}

#[cfg(test)]
mod tests {
    use ods_sdk::conformance::error_catalogue::{ErrorCatalogueHarness, Sample, run};

    use super::*;
    use crate::events::{error_summary, project_failure};

    /// The dbt minors whose messages are recorded (`capture-errors.sh`): 1.10, and the
    /// two the real-dbt CI job runs.
    const VERSIONS: [&str; 3] = ["1.10", "1.11", "1.12"];

    /// Every recording: the `DuckDB` runs, and dbt-databricks on a SQL warehouse
    /// (`.github/databricks/capture-errors.py`, #349).
    const RECORDINGS: [&str; 4] = ["1.10", "1.11", "1.12", "databricks"];

    /// The messages a recorded run gave: real dbt `version` + `DuckDB`
    /// (`capture-errors.sh`), or `databricks`.
    fn recorded(version: &str) -> Vec<(String, Option<String>, String)> {
        let path = format!(
            "{}/../../fixtures/dbt/jaffle-ods/artifacts/dbt-{version}-errors/errors.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        rows.into_iter()
            .map(|r| {
                (
                    r["name"].as_str().unwrap().to_owned(),
                    r["node"].as_str().map(str::to_owned),
                    r["message"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// A recorded message's summary: a node's, or the project's when no node failed.
    fn summary_of(node: Option<&String>, message: &str) -> ErrorSummary {
        match node {
            Some(_) => error_summary(message).unwrap(),
            None => project_failure(message).unwrap().summary,
        }
    }

    const SENTINEL: &str = "sk_live_SENTINEL_42";

    #[test]
    fn every_recorded_dbt_error_classifies_as_expected() {
        let expected = [
            ("missing-column", Some(Symptom::MissingColumn)),
            ("unknown-macro", Some(Symptom::UnknownMacro)),
            ("unqualified-column", Some(Symptom::MissingColumn)),
            ("missing-relation", Some(Symptom::MissingRelation)),
            ("missing-ref", Some(Symptom::MissingRef)),
            ("template-syntax", Some(Symptom::TemplateSyntax)),
            ("type-mismatch", Some(Symptom::TypeMismatch)),
            ("missing-function", Some(Symptom::MissingFunction)),
            ("packages-missing", Some(Symptom::PackagesMissing)),
            ("profile-missing", Some(Symptom::ProfileNotFound)),
            ("python-exception", Some(Symptom::PythonException)),
            ("dependent-objects", Some(Symptom::DependentObjects)),
            ("test-failure", Some(Symptom::TestFailed)),
            ("accepted-values", Some(Symptom::TestFailed)),
        ];
        for version in VERSIONS {
            let recorded = recorded(version);
            assert_eq!(
                recorded.len(),
                expected.len(),
                "{version}: every scenario is checked"
            );
            for (name, node, message) in &recorded {
                let (_, want) = expected
                    .iter()
                    .find(|(n, _)| n == name)
                    .unwrap_or_else(|| panic!("{version} {name} isn't expected"));
                let summary = summary_of(node.as_ref(), message);
                let got = DbtErrorCatalogue.classify(&summary);
                match (&got, want) {
                    (Classification::Recognised(m), Some(want)) => {
                        assert_eq!(m.symptom, *want, "{version} {name}: {summary:?}");
                        // A failed test can be run again keeping its rows (#323).
                        assert_eq!(
                            m.rerun.as_ref().map(|r| r.passthrough.as_str()),
                            (*want == Symptom::TestFailed).then_some("--store-failures"),
                            "{version} {name}"
                        );
                    }
                    (Classification::NotRecognised { category }, None) => {
                        assert_eq!(*category, ErrorCategory::Database, "{version} {name}");
                    }
                    _ => panic!("{version} {name}: {summary:?} gave {got:?}"),
                }
                let json = serde_json::to_string(&(&summary, &got)).unwrap();
                assert!(!json.contains(SENTINEL), "{version} {name}: {json}");
                // What dbt 1.12 colours in its log never reaches a summary.
                assert!(
                    !json.contains('\u{1b}') && !json.contains("\\u001b"),
                    "{version} {name}: {json}"
                );
            }
        }
    }

    #[test]
    fn every_recorded_databricks_error_classifies_as_expected() {
        // The scenario, the pattern it must reach, and the line Databricks gave.
        let expected = [
            ("missing-column", "spark-unresolved-column", Some(31)),
            ("unresolved-column", "spark-unresolved-column", Some(17)),
            (
                "missing-relation",
                "spark-table-or-view-not-found",
                Some(17),
            ),
            // A table in a schema that doesn't exist: Databricks reports the table, not
            // `[SCHEMA_NOT_FOUND]`, so ODS can't tell the two apart and says what the
            // warehouse said.
            ("missing-schema", "spark-table-or-view-not-found", Some(17)),
            ("missing-function", "spark-unresolved-routine", Some(17)),
            ("type-mismatch", "spark-cast-invalid-input", Some(17)),
            ("datatype-mismatch", "spark-datatype-mismatch", Some(17)),
            ("not-null-constraint", "delta-new-not-null-constraint", None),
            ("check-constraint", "delta-new-check-constraint", None),
            ("test-failure", "dbt-test-failed", None),
        ];
        let recorded = recorded("databricks");
        assert_eq!(recorded.len(), expected.len(), "every scenario is checked");
        for (name, node, message) in &recorded {
            let (_, id, line) = expected
                .iter()
                .find(|(n, _, _)| n == name)
                .unwrap_or_else(|| panic!("{name} isn't expected"));
            let summary = summary_of(node.as_ref(), message);
            let got = DbtErrorCatalogue.classify(&summary);
            let Classification::Recognised(m) = &got else {
                panic!("{name}: {summary:?} gave {got:?}")
            };
            assert_eq!(m.id, *id, "{name}: {summary:?}");
            assert_eq!(summary.line(), *line, "{name}: {summary:?}");
            let json = serde_json::to_string(&(&summary, &got)).unwrap();
            assert!(!json.contains(SENTINEL), "{name}: {json}");
            // Nothing from the warehouse's namespace or the SQL reaches a summary.
            assert!(!json.contains("no_such"), "{name}: {json}");
        }
    }

    /// dbt 1.11 and 1.12 put the failing line one earlier than 1.10 in these models.
    fn column_line(version: &str) -> u32 {
        if version == "1.10" { 25 } else { 24 }
    }

    #[test]
    fn summaries_keep_the_line_and_a_python_exception() {
        for version in VERSIONS {
            let recorded = recorded(version);
            let get = |name: &str| {
                let (_, node, message) = recorded.iter().find(|(n, _, _)| n == name).unwrap();
                summary_of(node.as_ref(), message)
            };
            let column = get("missing-column");
            assert_eq!(column.kind(), Some("Binder Error"));
            assert_eq!(column.line(), Some(column_line(version)), "{version}");
            assert_eq!(
                column.message(),
                "Binder Error: Values list [value removed] does not have a column named [value removed]"
            );
            let python = get("python-exception");
            assert_eq!(
                python.message(),
                "Python model failed: KeyError: [value removed]"
            );
            assert_eq!(python.line(), Some(6));
            let Classification::Recognised(m) = DbtErrorCatalogue.classify(&python) else {
                panic!()
            };
            assert_eq!(m.subject.as_deref(), Some("KeyError"));
            let macro_failure = project_failure(
                &recorded
                    .iter()
                    .find(|(n, _, _)| n == "unknown-macro")
                    .unwrap()
                    .2,
            )
            .unwrap();
            assert_eq!(macro_failure.node_name.as_deref(), Some("stg_payments"));
            assert_eq!(
                macro_failure.file.as_deref(),
                Some("models/staging/stg_payments.sql")
            );
            assert_eq!(macro_failure.summary.kind(), Some("Compilation Error"));
            let missing_ref = project_failure(
                &recorded
                    .iter()
                    .find(|(n, _, _)| n == "missing-ref")
                    .unwrap()
                    .2,
            )
            .unwrap();
            assert_eq!(missing_ref.node_name, None);
            assert_eq!(missing_ref.summary.kind(), Some("Compilation Error"));
            // The name the reference used is the subject, for did-you-mean; the summary
            // still doesn't hold it.
            assert_eq!(missing_ref.missing_node.as_deref(), Some("no_such_model"));
            assert!(!missing_ref.summary.message().contains("no_such_model"));
            let Classification::Recognised(m) = DbtErrorCatalogue.classify_project(&missing_ref)
            else {
                panic!("{version}: {missing_ref:?}")
            };
            assert_eq!(m.id, "dbt-missing-ref", "{version}");
            assert_eq!(m.subject.as_deref(), Some("no_such_model"), "{version}");
            // Only a missing ref gets one: another project failure keeps none.
            let Classification::Recognised(m) = DbtErrorCatalogue.classify_project(&macro_failure)
            else {
                panic!("{version}")
            };
            assert_eq!(m.subject, None, "{version}");
        }
    }

    #[test]
    fn databricks_and_postgres_messages_from_public_docs_are_recognised() {
        for (message, symptom) in [
            (
                "[UNRESOLVED_COLUMN.WITH_SUGGESTION] A column, variable, or function parameter with name `first_name` cannot be resolved. Did you mean one of the following? [`given_name`]",
                Symptom::MissingColumn,
            ),
            (
                "[TABLE_OR_VIEW_NOT_FOUND] The table or view `main`.`orders` cannot be found.",
                Symptom::MissingRelation,
            ),
            (
                "[CAST_INVALID_INPUT] The value 'sk_live_SENTINEL_42' of the type \"STRING\" cannot be cast to \"INT\" because it is malformed.",
                Symptom::TypeMismatch,
            ),
            (
                "[DELTA_CONCURRENT_APPEND] Transaction conflict detected. a concurrent WRITE added data to table x committed at version 830.",
                Symptom::LockConflict,
            ),
            (
                "column \"first_name\" does not exist",
                Symptom::MissingColumn,
            ),
            (
                "relation \"main.orders\" does not exist",
                Symptom::MissingRelation,
            ),
            (
                "permission denied for table orders",
                Symptom::PermissionDenied,
            ),
        ] {
            let summary = error_summary(&format!(
                "Database Error in model customers (models/customers.sql)\n  {message}"
            ))
            .unwrap();
            let got = DbtErrorCatalogue.classify(&summary);
            assert!(
                matches!(&got, Classification::Recognised(m) if m.symptom == symptom),
                "{message}: {summary:?} gave {got:?}"
            );
            assert!(!format!("{summary:?}").contains(SENTINEL));
        }
        let unknown = error_summary(
            "Database Error in model x (models/x.sql)\n  [DELTA_SOMETHING_NEW] Operation failed: 'x'",
        )
        .unwrap();
        assert_eq!(
            DbtErrorCatalogue.classify(&unknown),
            Classification::NotRecognised {
                category: ErrorCategory::Database
            }
        );
    }

    /// A failure as dbt reports a node's: its header, then the engine's message.
    fn node_failure(kind: &str, message: &str) -> ErrorSummary {
        error_summary(&format!(
            "{kind} in model customers (models/customers.sql)\n  {message}"
        ))
        .unwrap()
    }

    /// Messages for the patterns the recorded runs don't reach, each from the public
    /// source the module docs name, with the pattern each must match.
    const WRITTEN: &[(&str, &str, &str)] = &[
        (
            "dbt-target-not-found",
            "Runtime Error",
            "The profile 'jaffle' does not have a target named 'prod'. The valid target names for this profile are: - dev",
        ),
        (
            "dbt-template-unexpected",
            "Compilation Error",
            "unexpected '}'",
        ),
        (
            "dbt-template-expected-token",
            "Compilation Error",
            "expected token 'end of statement block', got 'x'",
        ),
        (
            "dbt-template-unknown-tag",
            "Compilation Error",
            "Encountered unknown tag 'endfor'.",
        ),
        (
            "duckdb-values-list-column",
            "Runtime Error",
            "Binder Error: Values list \"o\" does not have a column named \"x\"",
        ),
        (
            "duckdb-referenced-column",
            "Runtime Error",
            "Binder Error: Referenced column \"x\" not found in FROM clause!",
        ),
        (
            "duckdb-table-missing",
            "Runtime Error",
            "Catalog Error: Table with name orders does not exist!",
        ),
        (
            "duckdb-view-missing",
            "Runtime Error",
            "Catalog Error: View with name orders does not exist!",
        ),
        (
            "duckdb-conversion",
            "Runtime Error",
            "Conversion Error: Could not convert string 'sk_live_SENTINEL_42' to INT32",
        ),
        (
            "duckdb-constraint",
            "Runtime Error",
            "Constraint Error: Duplicate key \"id: 1\" violates primary key constraint.",
        ),
        (
            "duckdb-write-conflict",
            "Runtime Error",
            "TransactionContext Error: Catalog write-write conflict on create with \"orders\"",
        ),
        (
            "duckdb-file-lock",
            "Runtime Error",
            "IO Error: Could not set lock on file \"jaffle.duckdb\": Conflicting lock is held",
        ),
        (
            "duckdb-permission",
            "Runtime Error",
            "Permission Error: File system LocalFileSystem has been disabled by configuration",
        ),
        (
            "duckdb-interrupted",
            "Runtime Error",
            "INTERRUPT Error: Interrupted!",
        ),
        (
            "postgres-statement-timeout",
            "Database Error",
            "canceling statement due to statement timeout",
        ),
        (
            "postgres-invalid-input",
            "Database Error",
            "invalid input syntax for type integer: \"sk_live_SENTINEL_42\"",
        ),
        (
            "postgres-password",
            "Database Error",
            "password authentication failed for user \"sk_live_SENTINEL_42\"",
        ),
        (
            "postgres-unique",
            "Database Error",
            "duplicate key value violates unique constraint \"orders_pkey\"",
        ),
        (
            "postgres-not-null",
            "Database Error",
            "null value in column \"id\" of relation \"orders\" violates not-null constraint",
        ),
        (
            "postgres-connect",
            "Database Error",
            "could not connect to server: Connection refused",
        ),
        (
            "postgres-column-missing",
            "Database Error",
            "column \"first_name\" does not exist",
        ),
        (
            "postgres-relation-missing",
            "Database Error",
            "relation \"main.orders\" does not exist",
        ),
        (
            "postgres-permission-denied",
            "Database Error",
            "permission denied for table orders",
        ),
        (
            "spark-unresolved-column",
            "Database Error",
            "[UNRESOLVED_COLUMN.WITH_SUGGESTION] A column, variable, or function parameter with name `first_name` cannot be resolved.",
        ),
        (
            "spark-table-or-view-not-found",
            "Database Error",
            "[TABLE_OR_VIEW_NOT_FOUND] The table or view `main`.`orders` cannot be found.",
        ),
        (
            "spark-cast-invalid-input",
            "Database Error",
            "[CAST_INVALID_INPUT] The value 'sk_live_SENTINEL_42' of the type \"STRING\" cannot be cast to \"INT\" because it is malformed.",
        ),
        (
            "delta-concurrent-write",
            "Database Error",
            "[DELTA_CONCURRENT_APPEND] Transaction conflict detected. a concurrent WRITE added data to table x committed at version 830.",
        ),
        (
            "spark-datatype-mismatch",
            "Database Error",
            "[DATATYPE_MISMATCH.BINARY_OP_DIFF_TYPES] Cannot resolve \"(a + b)\" due to data type mismatch.",
        ),
        (
            "spark-schema-not-found",
            "Database Error",
            "[SCHEMA_NOT_FOUND] The schema `main`.`staging` cannot be found. Verify the spelling and correctness of the schema and catalog.",
        ),
        (
            "spark-unresolved-routine",
            "Database Error",
            "[UNRESOLVED_ROUTINE] Cannot resolve routine `cents_to_dollars` on search path [`system`.`builtin`, `system`.`session`].",
        ),
        (
            "duckdb-schema-missing",
            "Runtime Error",
            "Catalog Error: Schema with name staging does not exist!",
        ),
        (
            "postgres-schema-missing",
            "Database Error",
            "schema \"staging\" does not exist",
        ),
        (
            "postgres-function-missing",
            "Database Error",
            "function cents_to_dollars(integer) does not exist",
        ),
        (
            "spark-check-constraint",
            "Database Error",
            "[CHECK_CONSTRAINT_VIOLATION] CHECK constraint positive (amount > 0) violated by row with values: - amount : -1",
        ),
        (
            "spark-not-null-constraint",
            "Database Error",
            "[NOT_NULL_CONSTRAINT_VIOLATION] Assigning a NULL is not allowed here.",
        ),
        (
            "delta-not-null-constraint",
            "Database Error",
            "[DELTA_NOT_NULL_CONSTRAINT_VIOLATED] NOT NULL constraint violated for column: order_id.",
        ),
        (
            "delta-check-constraint",
            "Database Error",
            "[DELTA_VIOLATE_CONSTRAINT_WITH_VALUES] CHECK constraint positive (amount > 0) violated by row with values: - amount : -1",
        ),
    ];

    /// A warehouse catalogue that recognises one phrase as an unavailable warehouse.
    struct Warehouse;

    impl Provider for Warehouse {
        fn info(&self) -> ProviderInfo {
            ProviderInfo::new(
                "acme",
                "acme",
                "1",
                CapabilitySet::from_iter([Capability::ErrorExplain]),
            )
        }
    }

    impl ErrorCatalogue for Warehouse {
        fn catalogue(&self) -> CatalogueInfo {
            CatalogueInfo::new("acme", "3", "acme")
        }
        fn classify(&self, error: &ErrorSummary) -> Classification {
            let message = error.message().to_ascii_lowercase();
            if ["could not find profile", "error starting cluster"]
                .iter()
                .any(|p| message.contains(p))
            {
                Classification::Recognised(PatternMatch::new(
                    "acme-compute",
                    Symptom::WarehouseUnavailable,
                ))
            } else {
                Classification::NotRecognised {
                    category: ErrorCategory::Unknown,
                }
            }
        }
    }

    #[test]
    fn the_warehouse_catalogue_is_asked_first_and_gets_dbts_steps() {
        let project = ProjectCatalogue::new(vec![Arc::new(Warehouse)]);
        assert_eq!(
            project.catalogue().version,
            format!("{CATALOGUE_VERSION}+acme 3")
        );
        // A message dbt's own pattern also reads: the warehouse's wins.
        let summary = node_failure("Runtime Error", "Could not find profile named 'x'");
        let Classification::Recognised(found) = project.classify(&summary) else {
            panic!("not recognised")
        };
        assert_eq!(found.id, "acme-compute");
        assert!(!found.suggestions.is_empty(), "dbt's steps: {found:?}");
        // What the warehouse doesn't recognise, dbt's catalogue reads as before.
        let other = node_failure(
            "Compilation Error",
            "'x' is undefined. This can happen when calling a macro that does not exist.",
        );
        assert_eq!(project.classify(&other), DbtErrorCatalogue.classify(&other));
        // Without a warehouse catalogue, it is dbt's.
        let plain = ProjectCatalogue::default();
        assert_eq!(plain.catalogue(), DbtErrorCatalogue.catalogue());
        assert_eq!(
            plain.classify(&summary),
            DbtErrorCatalogue.classify(&summary)
        );
    }

    #[test]
    fn every_pattern_is_reached_by_a_message() {
        let mut reached = std::collections::BTreeSet::new();
        for (id, kind, message) in WRITTEN {
            let summary = node_failure(kind, message);
            let got = DbtErrorCatalogue.classify(&summary);
            let Classification::Recognised(m) = &got else {
                panic!("{id}: {summary:?} gave {got:?}")
            };
            assert_eq!(m.id, *id, "{summary:?}");
            let pattern = PATTERNS.iter().find(|p| p.id == *id).unwrap();
            assert_eq!(m.symptom, pattern.symptom, "{id}");
            let json = serde_json::to_string(&(&summary, &got)).unwrap();
            assert!(!json.contains(SENTINEL), "{id}: {json}");
            reached.insert(m.id.clone());
        }
        for version in RECORDINGS {
            for (_, node, message) in recorded(version) {
                if let Classification::Recognised(m) =
                    DbtErrorCatalogue.classify(&summary_of(node.as_ref(), &message))
                {
                    reached.insert(m.id);
                }
            }
        }
        let missing: Vec<_> = PATTERNS
            .iter()
            .map(|p| p.id)
            .filter(|id| !reached.contains(*id))
            .collect();
        assert!(missing.is_empty(), "no message reaches {missing:?}");
    }

    /// Where a pattern's text comes from, by its id's prefix (see the module docs).
    fn source_of(id: &str) -> &'static str {
        match id.split('-').next().unwrap_or_default() {
            "dbt" => "dbt-core's messages",
            "duckdb" => "DuckDB's errors, via dbt-duckdb",
            "postgres" => "PostgreSQL's documented messages",
            "spark" => "Apache Spark's `error-conditions.json`",
            "delta" => "Delta Lake's `delta-error-classes.json`",
            "databricks" => "dbt-databricks's source (1.12)",
            other => panic!("no source for the prefix {other}: add one here and in the reference"),
        }
    }

    /// The reference's table of patterns, from `PATTERNS`: what the docs page holds
    /// between its markers.
    fn reference_table() -> String {
        let mut seen = std::collections::BTreeSet::new();
        for version in RECORDINGS {
            for (_, node, message) in recorded(version) {
                if let Classification::Recognised(m) =
                    DbtErrorCatalogue.classify(&summary_of(node.as_ref(), &message))
                {
                    seen.insert(m.id);
                }
            }
        }
        let mut out = String::from(
            "| Pattern | Symptom | Kind | The message holds | Source | Recorded from real dbt |\n|---|---|---|---|---|---|\n",
        );
        for p in PATTERNS {
            let symptom = serde_json::to_value(p.symptom).unwrap();
            let phrases = if p.all.is_empty() {
                "(any message)".to_owned()
            } else {
                p.all
                    .iter()
                    .map(|ph| {
                        // A space at either end matters (`column ` isn't `columnar`):
                        // shown as `␠`, where a code span would hide it.
                        let core = ph.trim();
                        let lead = if ph.starts_with(' ') { "␠" } else { "" };
                        let trail = if ph.ends_with(' ') { "␠" } else { "" };
                        format!("`{lead}{core}{trail}`")
                    })
                    .collect::<Vec<_>>()
                    .join(" and ")
            };
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!(
                    "| `{}` | `{}` | {} | {} | {} | {} |\n",
                    p.id,
                    symptom.as_str().unwrap(),
                    p.kind
                        .map_or_else(|| "any".to_owned(), |k| format!("`{k}`")),
                    phrases,
                    source_of(p.id),
                    if seen.contains(p.id) {
                        "yes"
                    } else {
                        "no: from the source named"
                    },
                ),
            );
        }
        out
    }

    /// `docs/reference/error-patterns.md` lists every pattern as the code has it:
    /// `ODS_UPDATE_DOCS=1 cargo test -p ods-provider-dbt reference` rewrites its table.
    #[test]
    fn the_error_pattern_reference_matches_the_catalogue() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/reference/error-patterns.md");
        // A Windows checkout may give the page CRLF line endings.
        let page = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");
        let (begin, end) = ("<!-- patterns:begin -->\n", "<!-- patterns:end -->");
        let start = page.find(begin).expect("the begin marker") + begin.len();
        let stop = page.find(end).expect("the end marker");
        let table = reference_table();
        if std::env::var_os("ODS_UPDATE_DOCS").is_some() {
            std::fs::write(&path, format!("{}{table}{}", &page[..start], &page[stop..])).unwrap();
            return;
        }
        assert_eq!(
            &page[start..stop],
            table,
            "the reference is out of date: ODS_UPDATE_DOCS=1 cargo test -p ods-provider-dbt reference"
        );
        assert!(
            page.contains(&format!("catalogue version **{CATALOGUE_VERSION}**")),
            "the reference names the catalogue's version"
        );
    }

    #[test]
    fn a_missing_schema_or_sql_function_is_its_own_symptom() {
        // Not the symptom it resembles (a missing relation, an undefined macro), whose
        // evidence could otherwise confirm the wrong cause.
        for (message, symptom) in [
            (
                "[SCHEMA_NOT_FOUND] The schema `main`.`staging` cannot be found.",
                Symptom::MissingSchema,
            ),
            (
                "[UNRESOLVED_ROUTINE] Cannot resolve routine `cents_to_dollars` on search path [`system`.`builtin`].",
                Symptom::MissingFunction,
            ),
        ] {
            let got = DbtErrorCatalogue.classify(&node_failure("Database Error", message));
            assert!(
                matches!(&got, Classification::Recognised(m) if m.symptom == symptom),
                "{message}: {got:?}"
            );
        }
    }

    #[test]
    fn dbts_header_kind_is_kept_beside_a_phrase_read_as_the_kind() {
        // The summary takes the phrase before the colon for the message's kind; dbt's
        // header is kept as its outer kind, which patterns can require.
        let summary = node_failure(
            "Database Error",
            "could not connect to server: Connection refused",
        );
        assert_eq!(summary.kind(), Some("could not connect to server"));
        assert_eq!(summary.outer_kind(), Some("Database Error"));
        assert!(matches!(
            DbtErrorCatalogue.classify(&summary),
            Classification::Recognised(m) if m.id == "postgres-connect"
                && m.category == ErrorCategory::Connection
        ));
        // An error no pattern knows still gets dbt's category, not `unknown`.
        let unknown = node_failure("Database Error", "something new happened: details");
        assert_eq!(
            DbtErrorCatalogue.classify(&unknown),
            Classification::NotRecognised {
                category: ErrorCategory::Database
            }
        );
        let binder = node_failure(
            "Runtime Error",
            "Binder Error: Referenced column x not found",
        );
        assert_eq!(binder.kind(), Some("Binder Error"));
        assert_eq!(binder.outer_kind(), Some("Runtime Error"));
    }

    #[test]
    fn unavailable_compute_suggests_checking_the_connection() {
        // A warehouse's catalogue recognises its compute; dbt's steps come with it.
        let summary = node_failure("Runtime Error", "Error starting cluster: terminated");
        let project = ProjectCatalogue::new(vec![Arc::new(Warehouse)]);
        let Classification::Recognised(m) = project.classify(&summary) else {
            panic!()
        };
        assert_eq!(m.suggestions[0].commands, ["dbt debug"]);
    }

    #[test]
    fn the_index_finds_undefined_macro_calls_with_their_lines() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10/manifest.json"
        );
        let mut manifest = crate::Manifest::read(std::path::Path::new(path)).unwrap();
        let payments = manifest
            .nodes
            .iter_mut()
            .find(|n| n.unique_id == "model.jaffle_ods.stg_payments")
            .unwrap();
        // The broken model the unknown-macro scenario compiled.
        let node: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-errors/unknown-macro-node.json"
            ))
            .unwrap(),
        )
        .unwrap();
        payments.raw_code = node["raw_code"].as_str().map(str::to_owned);
        let index = project_index(&manifest, Some("target"));
        assert!(index.macros.contains("cents_to_dollars"));
        let payments = &index.nodes["model.jaffle_ods.stg_payments"];
        assert_eq!(
            payments.undefined_calls,
            vec![NameAt::new("cent_to_dollars", Some(5))]
        );
        assert_eq!(
            payments.compiled_file.as_deref(),
            Some("target/compiled/jaffle_ods/models/staging/stg_payments.sql")
        );
        // `ref`, `config`, loops and filters are not macros; nothing else is undefined.
        let orders = &index.nodes["model.jaffle_ods.orders"];
        assert!(orders.undefined_calls.is_empty(), "{orders:?}");
        // What `ref()` can name (models, seeds, snapshots) is referable; tests and
        // sources aren't (#323: did-you-mean for a missing ref).
        assert!(orders.referable);
        let kind = |id: &str| {
            manifest
                .nodes
                .iter()
                .find(|n| n.unique_id == id)
                .map(|n| n.resource_type)
        };
        for (id, node) in &index.nodes {
            let expected = matches!(
                kind(id),
                Some(ResourceType::Model | ResourceType::Seed | ResourceType::Snapshot)
            );
            assert_eq!(node.referable, expected, "{id}");
        }
        assert!(index.nodes.values().any(|n| n.referable));
        assert!(index.nodes.values().any(|n| !n.referable));
    }

    /// #323: a data test's kind, column and node come from the manifest; its
    /// arguments (here the values `accepted_values` accepts, one a secret) never do,
    /// and a generic test gets no compiled file (dbt names it after its arguments).
    /// The test as each recorded dbt version declared it; a test without
    /// `test_metadata` is singular.
    #[test]
    fn the_index_says_what_a_test_tests_without_its_arguments() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/dbt/jaffle-ods/artifacts"
        );
        for version in VERSIONS {
            let mut manifest: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(format!("{dir}/dbt-1.10/manifest.json")).unwrap(),
            )
            .unwrap();
            // The test as dbt declared it (`capture-errors.sh`, accepted-values), over
            // another test's node for the fields the scenario didn't keep.
            let recorded: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(format!(
                    "{dir}/dbt-{version}-errors/accepted-values-node.json"
                ))
                .unwrap(),
            )
            .unwrap();
            let nodes = manifest["nodes"].as_object_mut().unwrap();
            let mut node = nodes["test.jaffle_ods.not_null_orders_order_id.cf6c17daed"].clone();
            for (key, value) in recorded.as_object().unwrap() {
                node[key] = value.clone();
            }
            let id = recorded["unique_id"].as_str().unwrap().to_owned();
            assert!(id.contains(SENTINEL), "{version}: {id}");
            nodes.insert(id.clone(), node);
            // A singular test: its own SQL file, no `test_metadata`.
            let mut singular = nodes["test.jaffle_ods.unique_orders_order_id.fed79b3a6e"].clone();
            singular.as_object_mut().unwrap().remove("test_metadata");
            singular["name"] = "only_placed_orders".into();
            singular["unique_id"] = "test.jaffle_ods.only_placed_orders".into();
            singular["original_file_path"] = "tests/only_placed_orders.sql".into();
            nodes.insert("test.jaffle_ods.only_placed_orders".to_owned(), singular);
            let file = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(file.path(), manifest.to_string()).unwrap();
            let index = project_index(&crate::Manifest::read(file.path()).unwrap(), Some("target"));

            let test = &index.nodes[&id];
            assert_eq!(
                test.check,
                Some(CheckTarget::new(
                    Some("accepted_values"),
                    Some("status"),
                    Some("model.jaffle_ods.orders")
                )),
                "{version}"
            );
            assert_eq!(test.compiled_file, None, "{version}");
            let check = serde_json::to_string(&test.check).unwrap();
            assert!(!check.contains(SENTINEL), "{version}: {check}");
            // A model isn't a check; a test from dbt's own fixture is, and a generic one.
            assert_eq!(index.nodes["model.jaffle_ods.orders"].check, None);
            let not_null = index.nodes["test.jaffle_ods.not_null_customers_customer_id.5c9bf9911d"]
                .check
                .as_ref()
                .unwrap();
            assert_eq!(not_null.column.as_deref(), Some("customer_id"));
            assert!(!not_null.singular && !test.check.as_ref().unwrap().singular);
            assert_eq!(
                index.nodes["test.jaffle_ods.only_placed_orders"].check,
                Some(CheckTarget::new(None, None, Some("model.jaffle_ods.orders")).singular()),
                "{version}"
            );
        }
    }

    #[test]
    fn a_python_exception_as_the_kind_is_a_python_model_failure() {
        let summary = error_summary(
            "Runtime Error in model customer_segments (models/marts/customer_segments.py)\n  KeyError: 'lifetime_value'",
        )
        .unwrap();
        let Classification::Recognised(m) = DbtErrorCatalogue.classify(&summary) else {
            panic!("{summary:?}");
        };
        assert_eq!(m.symptom, Symptom::PythonException);
        assert_eq!(m.subject.as_deref(), Some("KeyError"));
        // An adapter's kind has a space: not a Python exception.
        assert!(!is_python_exception("Binder Error"));
        assert!(!is_python_exception("Error"));
    }

    #[test]
    fn only_macro_calls_count_not_methods_or_context() {
        // The review's scenario: a method on a list (line 3) and a real undefined
        // macro (line 5); and dbt context functions that aren't macros.
        let code = "{% set cols = [] %}\n{% for c in ['a'] %}\n{% do cols.append(c) %}\n{% endfor %}\nselect {{ cent_to_dollars('x') }}, {{ s.split(',') }}\n{% set x = set_strict([1]) %}{{ zip_strict(a, b) }}{{ load_relation(this) }}\n{% macro local_one(a) %}{% endmacro %}{% if x is divisibleby(3) %}{% endif %}";
        let mut manifest = crate::Manifest::read(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10/manifest.json"
        )))
        .unwrap();
        let node = manifest
            .nodes
            .iter_mut()
            .find(|n| n.unique_id == "model.jaffle_ods.stg_payments")
            .unwrap();
        node.raw_code = Some(code.to_owned());
        let index = project_index(&manifest, None);
        assert_eq!(
            index.nodes["model.jaffle_ods.stg_payments"].undefined_calls,
            vec![NameAt::new("cent_to_dollars", Some(5))]
        );
        assert!(
            CONTEXT.windows(2).all(|w| w[0] < w[1]),
            "sorted for binary_search"
        );
    }

    #[test]
    fn calls_skip_strings_methods_filters_and_context() {
        let code = "{{ config(materialized='table') }}\nselect {{ dbt_utils.star(ref('x')) }},\n  {{ adapter.dispatch('y')() }}, {{ 'no_call(' }}, {{ a | join(', ') }}\n{% if is_incremental() %}{{ my_macro (1) }}{% endif %}";
        let found: Vec<(String, Option<u32>)> =
            calls(code).into_iter().map(|c| (c.name, c.line)).collect();
        assert_eq!(
            found,
            vec![
                ("config".to_owned(), Some(1)),
                ("dbt_utils.star".to_owned(), Some(2)),
                ("ref".to_owned(), Some(2)),
                ("adapter.dispatch".to_owned(), Some(3)),
                ("is_incremental".to_owned(), Some(4)),
                ("my_macro".to_owned(), Some(4)),
            ]
        );
    }

    struct Harness;

    impl ErrorCatalogueHarness for Harness {
        fn catalogue(&self) -> &dyn ErrorCatalogue {
            &DbtErrorCatalogue
        }

        fn samples(&self) -> Vec<Sample> {
            let names = [
                ("missing-column", Some(Symptom::MissingColumn)),
                ("unknown-macro", Some(Symptom::UnknownMacro)),
                ("type-mismatch", Some(Symptom::TypeMismatch)),
                ("python-exception", Some(Symptom::PythonException)),
                ("missing-function", Some(Symptom::MissingFunction)),
            ];
            VERSIONS
                .iter()
                .flat_map(|version| {
                    let recorded = recorded(version);
                    names.iter().map(move |(name, expected)| {
                        let (n, node, message) =
                            recorded.iter().find(|(n, _, _)| n == name).unwrap();
                        let name: &'static str =
                            Box::leak(format!("{version} {n}").into_boxed_str());
                        Sample {
                            name,
                            summary: summary_of(node.as_ref(), message),
                            expected: *expected,
                            sentinel: Some(SENTINEL),
                        }
                    })
                })
                .chain(
                    [
                        ("missing-column", Symptom::MissingColumn),
                        ("type-mismatch", Symptom::TypeMismatch),
                        ("missing-function", Symptom::MissingFunction),
                        ("check-constraint", Symptom::ConstraintViolation),
                    ]
                    .into_iter()
                    .map(|(name, expected)| {
                        let recorded = recorded("databricks");
                        let (_, node, message) =
                            recorded.iter().find(|(n, _, _)| n == name).unwrap();
                        Sample {
                            name: Box::leak(format!("databricks {name}").into_boxed_str()),
                            summary: summary_of(node.as_ref(), message),
                            expected: Some(expected),
                            sentinel: Some(SENTINEL),
                        }
                    }),
                )
                // Every recorded failure is recognised now; one no pattern knows, as
                // dbt-duckdb reports a `DuckDB` error kind no pattern names.
                .chain(std::iter::once(Sample {
                    name: "an error no pattern knows",
                    summary: node_failure(
                        "Runtime Error",
                        "Out of Range Error: Overflow in multiplication of INT32 ('sk_live_SENTINEL_42')",
                    ),
                    expected: None,
                    sentinel: Some(SENTINEL),
                }))
                .collect()
        }
    }

    #[test]
    fn conforms() {
        let report = run(&Harness);
        assert!(report.skipped.is_empty(), "{report:?}");
    }
}
