//! A [`SqlLineageAnalyzer`] built on the Apache-2.0 [`sqlparser`] crate (ADR-0008).
//!
//! The analysis follows the scope-based algorithm popularised by sqlglot: resolve every
//! `FROM` source (tables, CTEs, derived tables, lateral views), qualify each column
//! against those sources and the upstream schemas, expand `*`, and trace every output
//! column to physical input columns through CTEs, subqueries and set operations. On top
//! of that it records the *indirect* inputs (joins, filters, grouping, windows,
//! conditions) that most tools omit, because they decide which rows exist.
//!
//! Anything it can't resolve safely makes the whole query *opaque* (AGENTS.md rule 3):
//! a `select *` over a relation with unknown columns, unsupported table sources,
//! recursive CTEs, or SQL that doesn't parse.

mod analyze;
mod dialect;

use ods_core::{CapabilitySet, RelationName, SchemaVersion};
use ods_sdk::contracts::sql_lineage::{
    AnalyzeRequest, QueryLineage, SQL_LINEAGE_ANALYZER, SqlLineageAnalyzer,
};
use ods_sdk::{Provider, ProviderError, ProviderFactory, ProviderInfo};

pub use dialect::{IdentifierCase, SqlDialect};

/// The `kind` this provider is configured under.
pub const KIND: &str = "sqlparser";

/// Column-level lineage for one SQL dialect.
#[derive(Debug, Clone)]
pub struct SqlparserAnalyzer {
    instance: String,
    dialect: SqlDialect,
}

impl SqlparserAnalyzer {
    /// An analyzer for `dialect`.
    pub fn new(dialect: SqlDialect) -> Self {
        Self {
            instance: KIND.to_owned(),
            dialect,
        }
    }

    /// The dialect it parses.
    pub fn dialect(&self) -> SqlDialect {
        self.dialect
    }
}

impl Provider for SqlparserAnalyzer {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            self.instance.clone(),
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}

impl SqlLineageAnalyzer for SqlparserAnalyzer {
    fn analyzer_version(&self) -> String {
        // Bump the trailing number whenever analysis results change for the same input,
        // so cached lineage is recomputed.
        format!("sqlparser-0.63/{}/2", self.dialect.name())
    }

    fn relation_name(&self, qualified: &str) -> Result<RelationName, ProviderError> {
        self.dialect.relation_name(qualified)
    }

    fn column_name(&self, name: &str) -> String {
        self.dialect.column_name(name)
    }

    fn analyze(&self, request: &AnalyzeRequest<'_>) -> Result<QueryLineage, ProviderError> {
        Ok(analyze::analyze(self.dialect, request.sql, request.schema))
    }
}

/// Creates [`SqlparserAnalyzer`]s from `[providers.<name>] kind = "sqlparser"`.
///
/// Settings: `dialect` (a string, see [`SqlDialect::from_name`]; default `generic`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SqlparserFactory;

impl ProviderFactory<dyn SqlLineageAnalyzer> for SqlparserFactory {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn contract_version(&self) -> SchemaVersion {
        SQL_LINEAGE_ANALYZER.version
    }

    fn create(
        &self,
        instance: &str,
        settings: &toml::Table,
    ) -> Result<Box<dyn SqlLineageAnalyzer>, ProviderError> {
        let invalid = |key: &str, message: String| ProviderError::InvalidSettings {
            instance: instance.to_owned(),
            kind: KIND.to_owned(),
            key: key.to_owned(),
            message,
        };
        let mut dialect = SqlDialect::Generic;
        for (key, value) in settings {
            match key.as_str() {
                "dialect" => {
                    let name = value
                        .as_str()
                        .ok_or_else(|| invalid(key, "must be a string".into()))?;
                    dialect = SqlDialect::from_name(name).ok_or_else(|| {
                        invalid(
                            key,
                            format!(
                                "unknown dialect; expected one of: {}",
                                SqlDialect::ALL
                                    .iter()
                                    .map(|d| d.name())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        )
                    })?;
                }
                _ => return Err(invalid(key, "unknown setting; expected dialect".into())),
            }
        }
        Ok(Box::new(SqlparserAnalyzer {
            instance: instance.to_owned(),
            dialect,
        }))
    }
}
