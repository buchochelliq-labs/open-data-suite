//! Dialects, parsing and identifier normalization.

use ods_core::RelationName;
use ods_sdk::ProviderError;
use sqlparser::ast::{Ident, ObjectName, Statement};
use sqlparser::dialect::{
    BigQueryDialect, DatabricksDialect, Dialect, DuckDbDialect, GenericDialect, PostgreSqlDialect,
    RedshiftSqlDialect, SnowflakeDialect, SparkSqlDialect,
};
use sqlparser::parser::{Parser, ParserError};

/// How a dialect folds identifier case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentifierCase {
    /// Identifiers are case-insensitive, quoted or not: fold everything to lower case.
    Insensitive,
    /// Unquoted identifiers fold to lower case; quoted ones keep their case.
    LowerUnquoted,
    /// Unquoted identifiers fold to upper case; quoted ones keep their case.
    UpperUnquoted,
}

/// A supported SQL dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SqlDialect {
    /// Databricks SQL.
    Databricks,
    /// Apache Spark SQL.
    Spark,
    /// `DuckDB`.
    DuckDb,
    /// Snowflake.
    Snowflake,
    /// `BigQuery`.
    BigQuery,
    /// `PostgreSQL`.
    Postgres,
    /// Amazon Redshift.
    Redshift,
    /// A permissive generic dialect.
    Generic,
}

impl SqlDialect {
    /// Every dialect.
    pub const ALL: [SqlDialect; 8] = [
        SqlDialect::Databricks,
        SqlDialect::Spark,
        SqlDialect::DuckDb,
        SqlDialect::Snowflake,
        SqlDialect::BigQuery,
        SqlDialect::Postgres,
        SqlDialect::Redshift,
        SqlDialect::Generic,
    ];

    /// The dialect's configuration name.
    pub fn name(self) -> &'static str {
        match self {
            SqlDialect::Databricks => "databricks",
            SqlDialect::Spark => "spark",
            SqlDialect::DuckDb => "duckdb",
            SqlDialect::Snowflake => "snowflake",
            SqlDialect::BigQuery => "bigquery",
            SqlDialect::Postgres => "postgres",
            SqlDialect::Redshift => "redshift",
            SqlDialect::Generic => "generic",
        }
    }

    /// The dialect called `name` (as used by dbt adapter types, e.g. `databricks`).
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        let alias = match name.as_str() {
            "postgresql" => "postgres",
            "sparksql" => "spark",
            other => other,
        };
        Self::ALL.into_iter().find(|d| d.name() == alias)
    }

    /// How identifiers fold.
    pub fn case(self) -> IdentifierCase {
        match self {
            SqlDialect::Databricks
            | SqlDialect::Spark
            | SqlDialect::DuckDb
            | SqlDialect::BigQuery => IdentifierCase::Insensitive,
            SqlDialect::Snowflake => IdentifierCase::UpperUnquoted,
            SqlDialect::Postgres | SqlDialect::Redshift | SqlDialect::Generic => {
                IdentifierCase::LowerUnquoted
            }
        }
    }

    /// Parsers to try, in order: the dialect itself, then more permissive relatives.
    fn parsers(self) -> Vec<Box<dyn Dialect>> {
        let own: Box<dyn Dialect> = match self {
            SqlDialect::Databricks => Box::new(DatabricksDialect {}),
            SqlDialect::Spark => Box::new(SparkSqlDialect {}),
            SqlDialect::DuckDb => Box::new(DuckDbDialect {}),
            SqlDialect::Snowflake => Box::new(SnowflakeDialect {}),
            SqlDialect::BigQuery => Box::new(BigQueryDialect {}),
            SqlDialect::Postgres => Box::new(PostgreSqlDialect {}),
            SqlDialect::Redshift => Box::new(RedshiftSqlDialect {}),
            SqlDialect::Generic => Box::new(GenericDialect {}),
        };
        let mut parsers = vec![own];
        // sqlparser 0.63's Databricks dialect misses some Spark syntax (e.g. `DIV`).
        if self == SqlDialect::Databricks {
            parsers.push(Box::new(SparkSqlDialect {}));
        }
        if self != SqlDialect::Generic {
            parsers.push(Box::new(GenericDialect {}));
        }
        parsers
    }

    /// Parses `sql`, falling back to more permissive dialects.
    pub(crate) fn parse(self, sql: &str) -> Result<Vec<Statement>, ParserError> {
        let mut first_error = None;
        for dialect in self.parsers() {
            match Parser::parse_sql(dialect.as_ref(), sql) {
                Ok(statements) => return Ok(statements),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or_else(|| ParserError::ParserError("no parser".into())))
    }

    /// Normalizes one identifier.
    pub(crate) fn ident(self, ident: &Ident) -> String {
        match (self.case(), ident.quote_style) {
            (IdentifierCase::Insensitive, _) | (IdentifierCase::LowerUnquoted, None) => {
                ident.value.to_lowercase()
            }
            (IdentifierCase::UpperUnquoted, None) => ident.value.to_uppercase(),
            (_, Some(_)) => ident.value.clone(),
        }
    }

    /// Normalizes an object name's identifier parts (function parts are rendered as-is).
    pub(crate) fn object_name(self, name: &ObjectName) -> Vec<String> {
        name.0
            .iter()
            .map(|part| match part.as_ident() {
                Some(ident) => self.ident(ident),
                None => part.to_string(),
            })
            .collect()
    }

    pub(crate) fn relation_name(self, qualified: &str) -> Result<RelationName, ProviderError> {
        let invalid = |e: String| ProviderError::Other(format!("invalid relation name: {e}"));
        let dialect = self.parsers().remove(0);
        let name = Parser::new(dialect.as_ref())
            .try_with_sql(qualified)
            .and_then(|mut p| p.parse_object_name(false))
            .map_err(|e| invalid(e.to_string()))?;
        RelationName::new(self.object_name(&name)).map_err(invalid)
    }

    pub(crate) fn column_name(self, name: &str) -> String {
        match self.case() {
            IdentifierCase::Insensitive | IdentifierCase::LowerUnquoted => name.to_lowercase(),
            IdentifierCase::UpperUnquoted => name.to_uppercase(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_alias() {
        for dialect in SqlDialect::ALL {
            assert_eq!(SqlDialect::from_name(dialect.name()), Some(dialect));
        }
        assert_eq!(
            SqlDialect::from_name("PostgreSQL"),
            Some(SqlDialect::Postgres)
        );
        assert_eq!(SqlDialect::from_name("oracle"), None);
    }

    #[test]
    fn relation_names_follow_dialect_case_rules() {
        let quoted = r#""Jaffle"."Main"."Orders""#;
        assert_eq!(
            SqlDialect::DuckDb
                .relation_name(quoted)
                .unwrap()
                .to_string(),
            "jaffle.main.orders"
        );
        assert_eq!(
            SqlDialect::Postgres
                .relation_name(quoted)
                .unwrap()
                .to_string(),
            "Jaffle.Main.Orders"
        );
        assert_eq!(
            SqlDialect::Snowflake
                .relation_name("db.sch.orders")
                .unwrap()
                .to_string(),
            "DB.SCH.ORDERS"
        );
        assert_eq!(
            SqlDialect::Databricks
                .relation_name("`Cat`.`Sch`.`T`")
                .unwrap()
                .to_string(),
            "cat.sch.t"
        );
    }
}
