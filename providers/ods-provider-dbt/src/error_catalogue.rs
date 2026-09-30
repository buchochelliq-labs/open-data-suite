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
//!   is recorded from a real run in `fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-errors`;
//! - PostgreSQL's documented messages (`column … does not exist`, `relation … does not
//!   exist`, `permission denied for …`);
//! - Apache Spark's public error conditions (`[UNRESOLVED_COLUMN…]`,
//!   `[TABLE_OR_VIEW_NOT_FOUND]`, `[CAST_INVALID_INPUT]`, `[DATATYPE_MISMATCH…]`) and
//!   Delta Lake's (`[DELTA_CONCURRENT_…]`), which Databricks reports.
//!
//! Anything else is not recognised: the catalogue gives only the category dbt's (or the
//! adapter's) kind implies, and ODS doesn't guess a cause.
//!
//! [`project_index`] describes a project for explanations: each node's files and the
//! macros its code calls that the manifest doesn't define.

use ods_core::failure::{ErrorCategory, Suggestion, Symptom, Text, is_code};
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::error_catalogue::{
    CatalogueInfo, Classification, ErrorCatalogue, IndexedNode, NameAt, PatternMatch, ProjectIndex,
};
use ods_sdk::contracts::run_events::ErrorSummary;
use ods_sdk::{Provider, ProviderInfo};

use crate::{Manifest, ResourceType};

/// The catalogue's version: bumped whenever a pattern is added, changed or removed.
pub const CATALOGUE_VERSION: &str = "1";

/// How a pattern recognises a summary: its kind (lowercased, exactly), and phrases its
/// lowercased message must hold, all of them.
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
        None,
        &["column ", "does not exist"],
    ),
    p(
        "postgres-relation-missing",
        Symptom::MissingRelation,
        None,
        &["relation ", "does not exist"],
    ),
    p(
        "postgres-permission-denied",
        Symptom::PermissionDenied,
        None,
        &["permission denied for"],
    ),
    p(
        "postgres-statement-timeout",
        Symptom::QueryTimeout,
        None,
        &["canceling statement due to statement timeout"],
    ),
    p(
        "postgres-invalid-input",
        Symptom::TypeMismatch,
        None,
        &["invalid input syntax for type"],
    ),
    p(
        "postgres-dependent-objects",
        Symptom::DependentObjects,
        None,
        &["because other objects depend on it"],
    ),
    p(
        "postgres-password",
        Symptom::CredentialsMissing,
        None,
        &["password authentication failed"],
    ),
    p(
        "postgres-unique",
        Symptom::ConstraintViolation,
        None,
        &["violates unique constraint"],
    ),
    p(
        "postgres-not-null",
        Symptom::ConstraintViolation,
        None,
        &["violates not-null constraint"],
    ),
    p(
        "postgres-connect",
        Symptom::WarehouseUnavailable,
        None,
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
            Suggestion::new(Text::new().plain("Install the project's packages, then retry:"))
                .with_command("dbt deps"),
        ),
        Symptom::TemplateSyntax => found.suggest(Suggestion::new(Text::new().plain(
            "Check the Jinja around the reported line: an unclosed bracket, quote or tag.",
        ))),
        Symptom::ProfileNotFound | Symptom::CredentialsMissing => found.suggest(
            Suggestion::new(Text::new().plain("Check that dbt can connect with this profile:"))
                .with_command("dbt debug"),
        ),
        _ => found,
    }
}

/// The exception named in a Python model's summary, `Python model failed: KeyError: …`.
fn python_exception(message: &str) -> Option<String> {
    let rest = message.strip_prefix("Python model failed: ")?;
    let name = rest.split(':').next()?.trim();
    is_code(name).then(|| name.to_owned())
}

impl ErrorCatalogue for DbtErrorCatalogue {
    fn catalogue(&self) -> CatalogueInfo {
        CatalogueInfo::new("dbt", CATALOGUE_VERSION, "dbt")
    }

    fn classify(&self, error: &ErrorSummary) -> Classification {
        let kind = error.kind().map(str::to_ascii_lowercase);
        let message = error.message().to_ascii_lowercase();
        let found = PATTERNS.iter().find(|pattern| {
            pattern.kind.is_none_or(|k| kind.as_deref() == Some(k))
                && pattern.all.iter().all(|phrase| message.contains(phrase))
        });
        match found {
            Some(pattern) => {
                let mut found = PatternMatch::new(pattern.id, pattern.symptom);
                if pattern.symptom == Symptom::PythonException {
                    found = found.about(python_exception(error.message()));
                }
                Classification::Recognised(steps(found))
            }
            None => Classification::NotRecognised {
                category: category_of(error.kind(), &message),
            },
        }
    }
}

/// Names a model's code may call that are not macros: Jinja's and dbt's context.
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
    "cycler",
    "dbt",
    "dbt_version",
    "dict",
    "diff_of_two_dicts",
    "doc",
    "env_var",
    "exceptions",
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
    "return",
    "run_query",
    "run_started_at",
    "schema",
    "selected_resources",
    "set_sql_header",
    "should_full_refresh",
    "source",
    "statement",
    "store_raw_result",
    "store_result",
    "super",
    "target",
    "this",
    "tojson",
    "toyaml",
    "try_or_compiler_error",
    "var",
    "write",
    "zip",
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
            if called && previous != '.' && previous != '|' {
                calls.push(name);
            }
            previous = 'a';
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
fn defined(
    name: &str,
    macros: &ProjectIndex,
    packaged: &std::collections::BTreeSet<String>,
) -> bool {
    match name.split_once('.') {
        Some((namespace, local)) => {
            CONTEXT.contains(&namespace) || packaged.contains(&format!("{namespace}.{local}"))
        }
        None => CONTEXT.contains(&name) || macros.macros.contains(name),
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
    let mut index = ProjectIndex::new(names);
    for node in &manifest.nodes {
        let Some(name) = node.name.as_deref() else {
            continue;
        };
        let file = node.original_file_path.as_deref();
        let compiled = match (target, node.fqn.first(), file) {
            (Some(target), Some(package), Some(file))
                if matches!(
                    node.resource_type,
                    ResourceType::Model | ResourceType::Snapshot | ResourceType::Test
                ) =>
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
            .filter(|c| is_code(&c.name) && !defined(&c.name, &index, &packaged))
            .collect();
        index = index.with_node(
            node.unique_id.clone(),
            IndexedNode::new(name)
                .in_file(file, compiled.as_deref())
                .calling_undefined(undefined),
        );
    }
    index
}

#[cfg(test)]
mod tests {
    use ods_sdk::conformance::error_catalogue::{ErrorCatalogueHarness, Sample, run};

    use super::*;
    use crate::events::{error_summary, project_failure};

    /// The messages real dbt 1.10 + `DuckDB` runs gave (`capture-errors.sh`).
    fn recorded() -> Vec<(String, Option<String>, String)> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/dbt/jaffle-ods/artifacts/dbt-1.10-errors/errors.json"
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
            ("missing-function", None),
            ("packages-missing", Some(Symptom::PackagesMissing)),
            ("profile-missing", Some(Symptom::ProfileNotFound)),
            ("python-exception", Some(Symptom::PythonException)),
            ("dependent-objects", Some(Symptom::DependentObjects)),
            ("test-failure", Some(Symptom::TestFailed)),
        ];
        let recorded = recorded();
        assert_eq!(recorded.len(), expected.len(), "every scenario is checked");
        for (name, node, message) in &recorded {
            let (_, want) = expected
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("{name} isn't expected"));
            let summary = summary_of(node.as_ref(), message);
            let got = DbtErrorCatalogue.classify(&summary);
            match (&got, want) {
                (Classification::Recognised(m), Some(want)) => {
                    assert_eq!(m.symptom, *want, "{name}: {summary:?}");
                }
                (Classification::NotRecognised { category }, None) => {
                    assert_eq!(*category, ErrorCategory::Database, "{name}");
                }
                _ => panic!("{name}: {summary:?} gave {got:?}"),
            }
            let json = serde_json::to_string(&(&summary, &got)).unwrap();
            assert!(!json.contains(SENTINEL), "{name}: {json}");
        }
    }

    #[test]
    fn summaries_keep_the_line_and_a_python_exception() {
        let recorded = recorded();
        let get = |name: &str| {
            let (_, node, message) = recorded.iter().find(|(n, _, _)| n == name).unwrap();
            summary_of(node.as_ref(), message)
        };
        let column = get("missing-column");
        assert_eq!(column.kind(), Some("Binder Error"));
        assert_eq!(column.line(), Some(25));
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
                ("missing-function", None),
            ];
            let recorded = recorded();
            names
                .iter()
                .map(|(name, expected)| {
                    let (n, node, message) = recorded.iter().find(|(n, _, _)| n == name).unwrap();
                    let name: &'static str = Box::leak(n.clone().into_boxed_str());
                    Sample {
                        name,
                        summary: summary_of(node.as_ref(), message),
                        expected: *expected,
                        sentinel: Some(SENTINEL),
                    }
                })
                .collect()
        }
    }

    #[test]
    fn conforms() {
        let report = run(&Harness);
        assert!(report.skipped.is_empty(), "{report:?}");
    }
}
