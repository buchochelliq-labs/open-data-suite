//! In-memory [`RelationPrivileges`] (ADR-0030 §4c).

use std::collections::BTreeMap;

use async_trait::async_trait;
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::privileges::{Access, PrivilegeReport, RelationPrivileges};
use ods_sdk::contracts::probe::ProbeTarget;
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::KIND;

/// What a login may do on each of a set of relations, as set up. A relation it wasn't
/// told about is unknown.
#[derive(Debug, Clone, Default)]
pub struct FakeRelationPrivileges {
    fails: bool,
    login: Option<String>,
    access: BTreeMap<String, Access>,
}

impl FakeRelationPrivileges {
    /// A login with no known access to anything.
    pub fn new() -> Self {
        Self::default()
    }

    /// The login is called `login`.
    #[must_use]
    pub fn with_login(mut self, login: impl Into<String>) -> Self {
        self.login = Some(login.into());
        self
    }

    /// The login can only read the relation of `target` (any node id).
    #[must_use]
    pub fn read_only(mut self, target: impl Into<String>) -> Self {
        self.access.insert(target.into(), Access::ReadOnly);
        self
    }

    /// The login holds `beyond` (e.g. `MODIFY`) on the relation of `target`.
    #[must_use]
    pub fn elevated<'a>(
        mut self,
        target: impl Into<String>,
        beyond: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        self.access.insert(
            target.into(),
            Access::Elevated(beyond.into_iter().map(str::to_owned).collect()),
        );
        self
    }

    /// What the login may do on `target`'s relation can't be told, because `why`.
    #[must_use]
    pub fn unknown(mut self, target: impl Into<String>, why: impl Into<String>) -> Self {
        self.access
            .insert(target.into(), Access::Unknown(why.into()));
        self
    }

    /// Makes [`privileges`](RelationPrivileges::privileges) fail, as when the catalog
    /// can't be read.
    #[must_use]
    pub fn failing(mut self) -> Self {
        self.fails = true;
        self
    }
}

impl Provider for FakeRelationPrivileges {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            "fake",
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::RelationPrivileges]),
        )
    }
}

#[async_trait]
impl RelationPrivileges for FakeRelationPrivileges {
    async fn privileges(&self, targets: &[ProbeTarget]) -> Result<PrivilegeReport, ProviderError> {
        if self.fails {
            return Err(ProviderError::Other(
                "the fake catalog can't be read".to_owned(),
            ));
        }
        Ok(PrivilegeReport::new(
            self.login.clone(),
            targets
                .iter()
                .map(|t| {
                    let access = self
                        .access
                        .get(&t.id)
                        .cloned()
                        .unwrap_or_else(|| Access::Unknown("no such relation".to_owned()));
                    (t.id.clone(), access)
                })
                .collect(),
        ))
    }
}
