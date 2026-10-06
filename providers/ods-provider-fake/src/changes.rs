//! In-memory [`ChangeProvider`] and [`RelationProbe`] (ADR-0022).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use ods_core::state::{DataVersion, Exactness};
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::changes::{ChangeProvider, RequestedSource, SourceVersion, VersionReport};
use ods_sdk::contracts::probe::{
    ProbeAnswer, ProbeReport, ProbeRequest, ProbeRow, ProbeTarget, RelationProbe,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::KIND;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the fake knows about one source.
#[derive(Debug, Clone)]
enum Known {
    /// Its version number, bumped by each commit.
    Version(u64),
    /// It can't be read, and why.
    Unreadable(String),
}

/// A change provider over sources with version numbers, which
/// [`commit`](FakeChangeProvider::commit) moves. Versions are `exact`, from `fake`.
#[derive(Debug, Clone)]
pub struct FakeChangeProvider {
    capabilities: CapabilitySet,
    fails: bool,
    sources: Arc<Mutex<BTreeMap<String, Known>>>,
}

impl Default for FakeChangeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeChangeProvider {
    /// A provider that knows no source and advertises `relation_versions`.
    pub fn new() -> Self {
        Self {
            capabilities: CapabilitySet::from([Capability::RelationVersions]),
            fails: false,
            sources: Arc::default(),
        }
    }

    /// Knows `source`, at version 1.
    #[must_use]
    pub fn with_source(self, source: impl Into<String>) -> Self {
        lock(&self.sources).insert(source.into(), Known::Version(1));
        self
    }

    /// Knows `source`, but can't read its version, because of `why`.
    #[must_use]
    pub fn unreadable(self, source: impl Into<String>, why: impl Into<String>) -> Self {
        lock(&self.sources).insert(source.into(), Known::Unreadable(why.into()));
        self
    }

    /// Advertises `capabilities` instead, to test planners' fallbacks.
    #[must_use]
    pub fn advertising(mut self, capabilities: CapabilitySet) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Makes [`versions`](ChangeProvider::versions) fail, as when the warehouse can't
    /// be reached.
    #[must_use]
    pub fn failing(mut self) -> Self {
        self.fails = true;
        self
    }

    /// New data in `source`: its version moves (shared between clones). Returns
    /// whether the source has a version to move.
    pub fn commit(&self, source: &str) -> bool {
        match lock(&self.sources).get_mut(source) {
            Some(Known::Version(n)) => {
                *n += 1;
                true
            }
            _ => false,
        }
    }
}

impl Provider for FakeChangeProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "fake",
            env!("CARGO_PKG_VERSION"),
            self.capabilities.clone(),
        )
    }
}

#[async_trait]
impl ChangeProvider for FakeChangeProvider {
    async fn versions(&self, sources: &[RequestedSource]) -> Result<VersionReport, ProviderError> {
        if self.fails {
            return Err(ProviderError::Other(
                "the fake warehouse can't be reached".to_owned(),
            ));
        }
        let known = lock(&self.sources);
        Ok(VersionReport::new(
            sources
                .iter()
                .map(|s| {
                    let version = match known.get(&s.id) {
                        Some(Known::Version(n)) => SourceVersion::Version(DataVersion::new(
                            n.to_string(),
                            Exactness::Exact,
                            "fake",
                        )),
                        Some(Known::Unreadable(why)) => SourceVersion::Unknown(why.clone()),
                        None => SourceVersion::Unknown("unknown source".to_owned()),
                    };
                    (s.id.clone(), version)
                })
                .collect(),
        ))
    }
}

/// A relation the fake probe knows.
#[derive(Debug, Clone, Default)]
struct FakeRelation {
    kind: String,
    format: Option<String>,
    /// Its name in the warehouse, when set: a target expecting another is unknown.
    named: Option<String>,
    /// Statement template → its first row.
    rows: BTreeMap<String, ProbeRow>,
}

/// A relation probe over nodes' relations with a kind, an optional format, and the
/// first row each statement template returns. A statement it has no row for returns
/// no rows. It confirms a format only when the relation has one. It remembers which
/// relations it ran statements against, in order.
#[derive(Debug, Clone, Default)]
pub struct FakeRelationProbe {
    fails: bool,
    relations: Arc<Mutex<BTreeMap<String, FakeRelation>>>,
    probed: Arc<Mutex<Vec<String>>>,
}

impl FakeRelationProbe {
    /// A probe that knows no relation.
    pub fn new() -> Self {
        Self::default()
    }

    /// The relation of `source` (any node id) is a `kind` (e.g. `table`), stored in
    /// `format` if known.
    #[must_use]
    pub fn with_relation(
        self,
        source: impl Into<String>,
        kind: impl Into<String>,
        format: Option<&str>,
    ) -> Self {
        lock(&self.relations).insert(
            source.into(),
            FakeRelation {
                kind: kind.into(),
                format: format.map(str::to_owned),
                named: None,
                rows: BTreeMap::new(),
            },
        );
        self
    }

    /// `template` returns `row` first for `source`'s relation.
    #[must_use]
    pub fn with_row<'a>(
        self,
        source: &str,
        template: &str,
        row: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Self {
        self.set_row(source, template, row);
        self
    }

    /// Changes what `template` returns first for `source`'s relation, e.g. after a
    /// commit (shared between clones). Does nothing for an unknown source.
    pub fn set_row<'a>(
        &self,
        source: &str,
        template: &str,
        row: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) {
        if let Some(relation) = lock(&self.relations).get_mut(source) {
            relation.rows.insert(
                template.to_owned(),
                row.into_iter()
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect(),
            );
        }
    }

    /// `source`'s relation is called `relation` in the warehouse, e.g. `"db"."main"."orders"`.
    #[must_use]
    pub fn named(self, source: &str, relation: impl Into<String>) -> Self {
        if let Some(found) = lock(&self.relations).get_mut(source) {
            found.named = Some(relation.into());
        }
        self
    }

    /// Makes [`probe`](RelationProbe::probe) fail, as when the warehouse can't be
    /// reached.
    #[must_use]
    pub fn failing(mut self) -> Self {
        self.fails = true;
        self
    }

    /// The ids of the relations statements ran against, in order (shared between
    /// clones).
    pub fn probed(&self) -> Vec<String> {
        lock(&self.probed).clone()
    }

    fn answer(relation: &FakeRelation, request: &ProbeRequest) -> ProbeAnswer {
        let filter = request.filter();
        if !filter.relation_kinds().contains(&relation.kind) {
            return ProbeAnswer::Skipped(format!(
                "a {}, not a {}",
                relation.kind,
                filter.relation_kinds().join(" or ")
            ));
        }
        if let Some(format) = filter.format() {
            match relation.format.as_deref() {
                Some(f) if f == format => {}
                Some(other) => {
                    return ProbeAnswer::Skipped(format!("stored as {other}, not {format}"));
                }
                None => {
                    return ProbeAnswer::Skipped(format!(
                        "its format can't be confirmed as {format}"
                    ));
                }
            }
        }
        ProbeAnswer::Rows(
            request
                .statements()
                .iter()
                .map(|statement| {
                    relation
                        .rows
                        .get(statement.template())
                        .map(|row| {
                            row.iter()
                                .filter(|(column, _)| statement.columns().contains(column))
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect(),
        )
    }
}

impl Provider for FakeRelationProbe {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "fake",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::RelationProbe]),
        )
    }
}

#[async_trait]
impl RelationProbe for FakeRelationProbe {
    async fn probe(
        &self,
        request: &ProbeRequest,
        targets: &[ProbeTarget],
    ) -> Result<ProbeReport, ProviderError> {
        if self.fails {
            return Err(ProviderError::Other(
                "the fake warehouse can't be reached".to_owned(),
            ));
        }
        let relations = lock(&self.relations);
        let mut probed = lock(&self.probed);
        Ok(ProbeReport::new(
            targets
                .iter()
                .map(|t| {
                    let answer = match relations.get(&t.id) {
                        None => ProbeAnswer::Unknown("no such relation".to_owned()),
                        Some(FakeRelation {
                            named: Some(named), ..
                        }) if t
                            .relation
                            .as_ref()
                            .is_some_and(|want| !want.eq_ignore_ascii_case(named)) =>
                        {
                            ProbeAnswer::Unknown(format!(
                                "it is `{named}` here, not `{}`",
                                t.relation.as_deref().unwrap_or_default()
                            ))
                        }
                        Some(relation) => Self::answer(relation, request),
                    };
                    if matches!(answer, ProbeAnswer::Rows(_)) {
                        probed.push(t.id.clone());
                    }
                    (t.id.clone(), answer)
                })
                .collect(),
        ))
    }
}
