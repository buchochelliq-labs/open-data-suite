//! A warehouse plugin's requests out of process (ADR-0031 §3d): `link`, `classify` and
//! `observed_lineage` in one batch call each, and `probe` and `privileges` over the
//! plugin's own connection.
//!
//! The SDK's link, catalogue and lineage contracts are synchronous and ask about one
//! thing at a time, while a plugin is asked once about everything. So the host asks
//! first, then serves the contract from the answers: an [`AnsweredLinker`],
//! [`AnsweredCatalogue`] or [`AnsweredLineage`]. The same types serve the answers a
//! command recorded, for the dashboard and MCP, which never start a plugin.
//!
//! Every answer is checked as a script's is: an answer about something not asked, or
//! about one thing twice, fails the call, and what a failed call was asked about has no
//! link, dbt's explanation, or is *unknown* (AGENTS rule 3).

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use ods_core::failure::ErrorCategory;
use ods_core::{Capability, CapabilitySet};
use ods_sdk::contracts::error_catalogue::{CatalogueInfo, Classification, ErrorCatalogue};
use ods_sdk::contracts::observed_lineage::{ObservedLineage, ObservedLineageSource};
use ods_sdk::contracts::privileges::{
    Access, PrivilegeReport, RELATION_PRIVILEGES, RelationPrivileges,
};
use ods_sdk::contracts::probe::{
    ProbeAnswer, ProbeReport, ProbeRequest, ProbeTarget, RELATION_PROBE, RelationProbe,
};
use ods_sdk::contracts::relation_link::{NoRelationLink, RelationLink, RelationLinker};
use ods_sdk::contracts::run_events::ErrorSummary;
use ods_sdk::protocol::{
    ClassifyRequest, ClassifyResponse, LinkOutcome, LinkRequest, LinkResponse,
    ObservedLineageRequest, ObservedLineageResponse, PrivilegesRequest, PrivilegesResponse,
    ProbeCall, ProbeResponse, WarehouseSettings,
};
use ods_sdk::{Provider, ProviderError, ProviderInfo};

use crate::{Described, KIND, PluginProcess};

/// The links a plugin gave, by relation.
pub type Links = BTreeMap<String, Result<RelationLink, NoRelationLink>>;

impl PluginProcess {
    /// Asks it for the links to `relations`, in one call. A relation it didn't answer
    /// about, or whose link isn't an `https://` URL without a user part or fragment, has
    /// none, saying why: a link a page shows must be safe to follow.
    ///
    /// # Errors
    /// The call failed, or it answered about a relation it wasn't asked about, or about
    /// one twice: every relation has no link.
    pub async fn links(
        &self,
        relations: &[String],
        settings: WarehouseSettings,
    ) -> Result<Links, ProviderError> {
        let answer: LinkResponse = self
            .process
            .ask(
                &LinkRequest::new(relations.to_vec(), settings),
                |r: &LinkResponse| r.protocol,
            )
            .await?;
        let asked: BTreeSet<&str> = relations.iter().map(String::as_str).collect();
        let mut links = Links::new();
        for answer in answer.links {
            self.asked(&asked, &answer.relation, "relation")?;
            let link = match answer.outcome {
                LinkOutcome::Link(link) => self.safe(link),
                LinkOutcome::NoLink(why) => Err(why),
                _ => Err(self.not_answered("its answer is of a kind this ods doesn't know")),
            };
            if links.insert(answer.relation.clone(), link).is_some() {
                return Err(self.twice(&answer.relation));
            }
        }
        for relation in relations {
            links
                .entry(relation.clone())
                .or_insert_with(|| Err(self.not_answered("it didn't answer about it")));
        }
        Ok(links)
    }

    /// Asks it to classify `errors`, in one call: one classification each, in order.
    ///
    /// # Errors
    /// The call failed, or it gave another number of classifications.
    pub async fn classify(
        &self,
        errors: &[ErrorSummary],
    ) -> Result<Vec<Classification>, ProviderError> {
        let answer: ClassifyResponse = self
            .process
            .ask(
                &ClassifyRequest::new(errors.to_vec()),
                |r: &ClassifyResponse| r.protocol,
            )
            .await?;
        if answer.classifications.len() != errors.len() {
            return Err(ProviderError::Other(format!(
                "the plugin `{}` classified {} errors, asked about {}",
                self.name,
                answer.classifications.len(),
                errors.len()
            )));
        }
        Ok(answer.classifications)
    }

    /// Asks it to read the lineage exported at `export`, an absolute path.
    ///
    /// # Errors
    /// The call failed.
    pub async fn observed_lineage(
        &self,
        export: &str,
        settings: WarehouseSettings,
    ) -> Result<ObservedLineage, ProviderError> {
        let answer: ObservedLineageResponse = self
            .process
            .ask(
                &ObservedLineageRequest::new(export, settings),
                |r: &ObservedLineageResponse| r.protocol,
            )
            .await?;
        Ok(answer.lineage)
    }

    /// Its probes, over its own connection, with its warehouse's `settings`: also its
    /// login check when it implements `relation_privileges`.
    pub fn probe(
        self: &std::sync::Arc<Self>,
        described: &Described,
        settings: WarehouseSettings,
    ) -> Option<PluginProbe> {
        described.provides(&RELATION_PROBE).then(|| PluginProbe {
            plugin: self.clone(),
            settings,
            privileges: described.provides(&RELATION_PRIVILEGES),
        })
    }

    /// `link` if it is safe for a page to show: an `https://` URL with a host, and no
    /// user part or fragment, and a label.
    fn safe(&self, link: RelationLink) -> Result<RelationLink, NoRelationLink> {
        let safe = url::Url::parse(&link.url).is_ok_and(|u| {
            u.scheme() == "https"
                && u.host().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.fragment().is_none()
        });
        if safe && !link.label.trim().is_empty() {
            Ok(link)
        } else {
            Err(self.not_answered(
                "its link isn't an `https://` URL with a label and no user part or fragment",
            ))
        }
    }

    fn not_answered(&self, why: &str) -> NoRelationLink {
        NoRelationLink::NotAnswered {
            plugin: self.name.clone(),
            why: why.to_owned(),
        }
    }

    /// Whether `id` is something it was asked about.
    fn asked(&self, asked: &BTreeSet<&str>, id: &str, what: &str) -> Result<(), ProviderError> {
        if asked.contains(id) {
            Ok(())
        } else {
            Err(ProviderError::Other(format!(
                "the plugin `{}` answered about the {what} `{id}`, which it wasn't asked about",
                self.name
            )))
        }
    }

    fn twice(&self, id: &str) -> ProviderError {
        ProviderError::Other(format!(
            "the plugin `{}` answered about `{id}` twice",
            self.name
        ))
    }
}

/// Links a plugin gave, served one at a time.
#[derive(Debug, Clone)]
pub struct AnsweredLinker {
    plugin: String,
    links: Links,
}

impl AnsweredLinker {
    /// The links the plugin `plugin` gave (or a command recorded).
    pub fn new(plugin: impl Into<String>, links: Links) -> Self {
        Self {
            plugin: plugin.into(),
            links,
        }
    }

    /// The links, by relation.
    pub fn links(&self) -> &Links {
        &self.links
    }
}

impl Provider for AnsweredLinker {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            self.plugin.clone(),
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::RelationLink]),
        )
    }
}

impl RelationLinker for AnsweredLinker {
    fn link(&self, relation: &str) -> Result<RelationLink, NoRelationLink> {
        self.links.get(relation).cloned().unwrap_or_else(|| {
            Err(NoRelationLink::NotAnswered {
                plugin: self.plugin.clone(),
                why: "no command that runs it has asked about this relation yet".to_owned(),
            })
        })
    }
}

/// The classifications a plugin gave, served one error at a time. An error it wasn't
/// asked about isn't recognised, so the next catalogue (dbt's) explains it.
#[derive(Debug, Clone)]
pub struct AnsweredCatalogue {
    plugin: String,
    catalogue: CatalogueInfo,
    answers: Vec<(ErrorSummary, Classification)>,
}

impl AnsweredCatalogue {
    /// The classifications of the plugin `plugin`'s catalogue `catalogue`.
    pub fn new(
        plugin: impl Into<String>,
        catalogue: CatalogueInfo,
        answers: Vec<(ErrorSummary, Classification)>,
    ) -> Self {
        Self {
            plugin: plugin.into(),
            catalogue,
            answers,
        }
    }

    /// The classifications, by error.
    pub fn answers(&self) -> &[(ErrorSummary, Classification)] {
        &self.answers
    }
}

impl Provider for AnsweredCatalogue {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            self.plugin.clone(),
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::from([Capability::ErrorExplain]),
        )
    }
}

impl ErrorCatalogue for AnsweredCatalogue {
    fn catalogue(&self) -> CatalogueInfo {
        self.catalogue.clone()
    }

    fn classify(&self, error: &ErrorSummary) -> Classification {
        self.answers
            .iter()
            .find(|(asked, _)| asked == error)
            .map_or(
                // Not asked about: the next catalogue says what it is.
                Classification::NotRecognised {
                    category: ErrorCategory::Unknown,
                },
                |(_, classification)| classification.clone(),
            )
    }
}

/// The lineage a plugin read from an export.
#[derive(Debug, Clone)]
pub struct AnsweredLineage {
    plugin: String,
    lineage: ObservedLineage,
}

impl AnsweredLineage {
    /// What the plugin `plugin` read.
    pub fn new(plugin: impl Into<String>, lineage: ObservedLineage) -> Self {
        Self {
            plugin: plugin.into(),
            lineage,
        }
    }
}

impl Provider for AnsweredLineage {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            self.plugin.clone(),
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}

impl ObservedLineageSource for AnsweredLineage {
    fn observed_lineage(&self) -> Result<ObservedLineage, ProviderError> {
        Ok(self.lineage.clone())
    }
}

/// A plugin's probes, over its own connection, and, when it implements
/// `relation_privileges`, what that same login may do: one connection, so the login
/// checked is the login probed (ADR-0030 §4c).
#[derive(Debug, Clone)]
pub struct PluginProbe {
    plugin: std::sync::Arc<PluginProcess>,
    settings: WarehouseSettings,
    privileges: bool,
}

impl PluginProbe {
    /// Whether it reports its login's privileges.
    pub fn reports_privileges(&self) -> bool {
        self.privileges
    }

    /// The plugin's name.
    pub fn plugin(&self) -> &str {
        &self.plugin.name
    }
}

impl Provider for PluginProbe {
    fn info(&self) -> ProviderInfo {
        let mut capabilities = CapabilitySet::from([Capability::RelationProbe]);
        if self.privileges {
            capabilities.insert(Capability::RelationPrivileges);
        }
        ProviderInfo::new(
            KIND,
            self.plugin.name.clone(),
            env!("CARGO_PKG_VERSION"),
            capabilities,
        )
    }
}

/// Every one of `targets`, in order, with its answer from `answered`: an id not asked
/// about, or answered twice, fails the call; one not answered gets `missing`.
fn in_order<T>(
    plugin: &PluginProcess,
    targets: &[ProbeTarget],
    answered: impl IntoIterator<Item = (String, T)>,
    missing: impl Fn() -> T,
) -> Result<Vec<(String, T)>, ProviderError> {
    let asked: BTreeSet<&str> = targets.iter().map(|t| t.id.as_str()).collect();
    let mut by_id: BTreeMap<String, T> = BTreeMap::new();
    for (id, answer) in answered {
        plugin.asked(&asked, &id, "target")?;
        if by_id.contains_key(&id) {
            return Err(plugin.twice(&id));
        }
        by_id.insert(id, answer);
    }
    Ok(targets
        .iter()
        .map(|t| {
            let answer = by_id.remove(&t.id).unwrap_or_else(&missing);
            (t.id.clone(), answer)
        })
        .collect())
}

#[async_trait]
impl RelationProbe for PluginProbe {
    async fn probe(
        &self,
        request: &ProbeRequest,
        targets: &[ProbeTarget],
    ) -> Result<ProbeReport, ProviderError> {
        let call = ProbeCall::new(request.clone(), targets.to_vec(), self.settings.clone());
        let answer: ProbeResponse = self
            .plugin
            .process
            .ask(&call, |r: &ProbeResponse| r.protocol)
            .await?;
        let name = self.plugin.name.clone();
        let targets = in_order(
            &self.plugin,
            targets,
            answer.targets.into_iter().map(|a| (a.id, a.answer)),
            || ProbeAnswer::Unknown(format!("the plugin `{name}` didn't answer about it")),
        )?;
        Ok(ProbeReport::new(targets))
    }
}

#[async_trait]
impl RelationPrivileges for PluginProbe {
    async fn privileges(&self, targets: &[ProbeTarget]) -> Result<PrivilegeReport, ProviderError> {
        if !self.privileges {
            return Err(ProviderError::Other(format!(
                "the plugin `{}` doesn't report its login's privileges",
                self.plugin.name
            )));
        }
        let request = PrivilegesRequest::new(targets.to_vec(), self.settings.clone());
        let answer: PrivilegesResponse = self
            .plugin
            .process
            .ask(&request, |r: &PrivilegesResponse| r.protocol)
            .await?;
        let name = self.plugin.name.clone();
        let targets = in_order(
            &self.plugin,
            targets,
            answer.targets.into_iter().map(|a| (a.id, a.access)),
            || Access::Unknown(format!("the plugin `{name}` didn't answer about it")),
        )?;
        Ok(PrivilegeReport::new(answer.login, targets))
    }
}
