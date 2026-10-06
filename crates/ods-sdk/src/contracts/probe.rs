//! `RelationProbe`: runs a few read-only statements against each of a set of nodes'
//! relations (sources, models, seeds, snapshots), through a connection the provider
//! already has, and returns their first rows (ADR-0022 §1, ADR-0030 §5).
//!
//! The request is typed and says nothing about how it runs: a filter on the relations
//! to probe, and statement templates. How the filter is checked and the statements run
//! (e.g. a catalog lookup, or a template rendered by the engine) is the implementation's
//! business.
//!
//! # Semantics
//! - [`probe`](RelationProbe::probe) runs only the request's statements, which the
//!   caller vouches are read-only, and only against the relations of the requested
//!   [targets](ProbeTarget): never against a relation it wasn't asked about.
//! - The report lists every requested target exactly once, in request order.
//! - A relation is probed only if the implementation can show it matches the
//!   [filter](ProbeFilter): its kind is one of the filter's kinds and, when the filter
//!   names a format, the implementation confirmed that format. A relation it can't
//!   show matches (including one whose format it can't tell) is
//!   [`ProbeAnswer::Skipped`], and nothing runs against it.
//! - A probed relation's answer is [`ProbeAnswer::Rows`]: one row per statement, in
//!   statement order. A row holds the statement's [columns](ProbeStatement::columns)
//!   from its first result row, as strings; a column the statement didn't return, or
//!   returned as null, is absent, and a statement that returned no rows gives an empty
//!   row.
//! - A target that names its [relation](ProbeTarget::relation) is probed only when the
//!   relation the implementation finds for it is that one (compared ignoring case, as
//!   warehouses differ on it); another (e.g. the node resolved under another target or
//!   schema) is [`ProbeAnswer::Unknown`], saying which, and nothing runs against it.
//! - A target the implementation didn't recognise, or couldn't probe, is
//!   [`ProbeAnswer::Unknown`] with a reason, never `Rows` or `Skipped`: `Skipped` is only
//!   for a relation it recognised that doesn't match the filter.
//! - `Err` means nothing was read: callers treat every requested target as unknown.
//!   An implementation that can't isolate one relation's failure (e.g. one query for
//!   all of them) fails the whole call.
//! - With a [timeout](ProbeRequest::with_timeout), a call that takes longer fails, and
//!   stops what it started on ODS's side (e.g. the process it ran); a statement the
//!   warehouse already accepted may run on there until the warehouse ends it.
//! - Implementations answer in as few batches as they can, and may look at more
//!   relations than were requested to do so (e.g. to list them), but run nothing
//!   against those.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use ods_core::SchemaVersion;
use serde::Serialize;

use crate::contracts::changes::RequestedSource;
use crate::error::ProviderError;
use crate::provider::{Contract, Provider};

/// The `relation_probe` contract.
pub const RELATION_PROBE: Contract = Contract {
    name: "relation_probe",
    version: SchemaVersion::new(0, 2),
};

/// The placeholder a statement template names its relation with. The implementation
/// replaces it with the relation's name, quoted by its own rules.
pub const PLACEHOLDER: &str = "{relation}";

/// Why a probe request can't be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid relation probe: {0}")]
pub struct InvalidProbe(String);

/// A plain identifier: ASCII letters, digits and `_`, not starting with a digit. Kinds,
/// formats and columns are identifiers, so an implementation can embed them in its own
/// query language without escaping.
fn identifier(what: &str, name: &str) -> Result<String, InvalidProbe> {
    let mut bytes = name.bytes();
    let valid = bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if valid {
        Ok(name.to_owned())
    } else {
        Err(InvalidProbe(format!(
            "{what} `{name}` isn't a plain identifier (letters, digits and `_`)"
        )))
    }
}

/// A statement to run against each relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProbeStatement {
    template: String,
    columns: Vec<String>,
}

impl ProbeStatement {
    /// A statement from `template`, which names its relation once, as [`PLACEHOLDER`],
    /// returning `columns`.
    ///
    /// # Errors
    /// Returns [`InvalidProbe`] if the template doesn't name the placeholder exactly
    /// once, has other braces (so an implementation that renders templates never
    /// sees a second placeholder or its own syntax), or a column isn't a plain
    /// identifier, or there are no columns.
    pub fn new(
        template: impl Into<String>,
        columns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, InvalidProbe> {
        let template = template.into();
        if template.matches(PLACEHOLDER).count() != 1 {
            return Err(InvalidProbe(format!(
                "`{template}` must name `{PLACEHOLDER}` exactly once"
            )));
        }
        if template.replace(PLACEHOLDER, "").contains(['{', '}']) {
            return Err(InvalidProbe(format!(
                "`{template}` has braces other than `{PLACEHOLDER}`"
            )));
        }
        let columns = columns
            .into_iter()
            .map(|c| identifier("column", &c.into()))
            .collect::<Result<Vec<_>, _>>()?;
        if columns.is_empty() {
            return Err(InvalidProbe(format!("`{template}` returns no columns")));
        }
        Ok(Self { template, columns })
    }

    /// The template, with [`PLACEHOLDER`] where the relation goes.
    pub fn template(&self) -> &str {
        &self.template
    }

    /// The columns to return from its first row.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The statement for `relation`, a name the implementation has already quoted.
    pub fn render(&self, relation: &str) -> String {
        self.template.replace(PLACEHOLDER, relation)
    }
}

/// A node whose relation to probe: a source, model, seed or snapshot.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProbeTarget {
    /// Its id, e.g. `source.shop.raw.orders` or `model.shop.orders`.
    pub id: String,
    /// Its name as the project gives it, for messages, e.g. `raw.orders` or `orders`.
    pub name: String,
    /// The relation the caller expects, as the project's artifacts name it (e.g.
    /// `"db"."main"."orders"`), if it knows: the implementation probes nothing else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
}

impl ProbeTarget {
    /// A node to probe.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            relation: None,
        }
    }

    /// The same target, which must be the relation `relation` (see
    /// [`relation`](Self::relation)).
    #[must_use]
    pub fn expecting(mut self, relation: impl Into<String>) -> Self {
        self.relation = Some(relation.into());
        self
    }
}

impl From<&RequestedSource> for ProbeTarget {
    fn from(source: &RequestedSource) -> Self {
        Self::new(source.id.clone(), source.name.clone())
    }
}

/// Which relations the statements run against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProbeFilter {
    kinds: Vec<String>,
    format: Option<String>,
}

impl ProbeFilter {
    /// Relations of these kinds, e.g. `table` or `view`.
    ///
    /// # Errors
    /// Returns [`InvalidProbe`] if there are none, or one isn't a plain identifier.
    pub fn kinds(kinds: impl IntoIterator<Item = impl Into<String>>) -> Result<Self, InvalidProbe> {
        let kinds = kinds
            .into_iter()
            .map(|k| identifier("kind", &k.into()))
            .collect::<Result<Vec<_>, _>>()?;
        if kinds.is_empty() {
            return Err(InvalidProbe("the filter names no relation kind".to_owned()));
        }
        Ok(Self {
            kinds,
            format: None,
        })
    }

    /// Only relations the implementation can confirm are stored in `format`.
    ///
    /// # Errors
    /// Returns [`InvalidProbe`] if `format` isn't a plain identifier.
    pub fn with_format(mut self, format: impl Into<String>) -> Result<Self, InvalidProbe> {
        self.format = Some(identifier("format", &format.into())?);
        Ok(self)
    }

    /// The relation kinds that match.
    pub fn relation_kinds(&self) -> &[String] {
        &self.kinds
    }

    /// The format a relation must be confirmed to have, if any.
    pub fn format(&self) -> Option<&str> {
        self.format.as_deref()
    }
}

/// What to run, and against which relations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ProbeRequest {
    filter: ProbeFilter,
    statements: Vec<ProbeStatement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<Duration>,
}

impl ProbeRequest {
    /// Run `statements`, in order, against each relation `filter` matches.
    ///
    /// # Errors
    /// Returns [`InvalidProbe`] if there are no statements.
    pub fn new(filter: ProbeFilter, statements: Vec<ProbeStatement>) -> Result<Self, InvalidProbe> {
        if statements.is_empty() {
            return Err(InvalidProbe("there is no statement to run".to_owned()));
        }
        Ok(Self {
            filter,
            statements,
            timeout: None,
        })
    }

    /// The same request, failing when it takes longer than `timeout`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// How long a call may take, if limited.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Which relations to probe.
    pub fn filter(&self) -> &ProbeFilter {
        &self.filter
    }

    /// What to run against each, in order.
    pub fn statements(&self) -> &[ProbeStatement] {
        &self.statements
    }
}

/// One statement's first row: column → value, without the columns that were missing
/// or null.
pub type ProbeRow = BTreeMap<String, String>;

/// What probing one target's relation found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProbeAnswer {
    /// It matched the filter: one row per statement, in statement order.
    Rows(Vec<ProbeRow>),
    /// It isn't shown to match the filter, and why. Nothing ran against it.
    Skipped(String),
    /// It couldn't be probed, and why.
    Unknown(String),
}

/// What [`RelationProbe::probe`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ProbeReport {
    /// Every requested target, by id, in request order.
    pub targets: Vec<(String, ProbeAnswer)>,
}

impl ProbeReport {
    /// A report, for implementations to return.
    pub fn new(targets: Vec<(String, ProbeAnswer)>) -> Self {
        Self { targets }
    }
}

/// Runs read-only statements against nodes' relations.
#[async_trait]
pub trait RelationProbe: Provider {
    /// Runs `request` against the relation of each of `targets`, and nothing else.
    ///
    /// # Errors
    /// Returns [`ProviderError`] if the probe couldn't run, or ran out of time; nothing
    /// was read.
    async fn probe(
        &self,
        request: &ProbeRequest,
        targets: &[ProbeTarget],
    ) -> Result<ProbeReport, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_statement_names_its_relation_once_and_nothing_else() {
        let ok = ProbeStatement::new("DESCRIBE DETAIL {relation}", ["id", "format"]).unwrap();
        assert_eq!(ok.render("`c`.`s`.`t`"), "DESCRIBE DETAIL `c`.`s`.`t`");
        assert_eq!(ok.columns(), ["id", "format"]);
        for bad in [
            "select 1",
            "select * from {relation} join {relation}",
            "select '{{ x }}' from {relation}",
            "select {other} from {relation}",
        ] {
            assert!(ProbeStatement::new(bad, ["a"]).is_err(), "{bad}");
        }
        assert!(ProbeStatement::new("select * from {relation}", ["a b"]).is_err());
        assert!(ProbeStatement::new("select * from {relation}", ["1a"]).is_err());
        assert!(ProbeStatement::new("select * from {relation}", Vec::<String>::new()).is_err());
    }

    #[test]
    fn filters_and_requests_are_checked() {
        let filter = ProbeFilter::kinds(["table"])
            .unwrap()
            .with_format("columnar")
            .unwrap();
        assert_eq!(filter.relation_kinds(), ["table"]);
        assert_eq!(filter.format(), Some("columnar"));
        assert!(ProbeFilter::kinds(Vec::<String>::new()).is_err());
        assert!(ProbeFilter::kinds(["ta'ble"]).is_err());
        assert!(filter.clone().with_format("x y").is_err());
        assert!(ProbeRequest::new(filter.clone(), Vec::new()).is_err());
        let statement = ProbeStatement::new("select * from {relation}", ["a"]).unwrap();
        let request = ProbeRequest::new(filter, vec![statement.clone()]).unwrap();
        assert_eq!(request.statements(), [statement]);
        assert_eq!(request.timeout(), None);
        let request = request.with_timeout(Duration::from_secs(30));
        assert_eq!(request.timeout(), Some(Duration::from_secs(30)));
    }
}
