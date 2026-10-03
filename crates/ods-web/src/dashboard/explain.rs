//! Failed nodes and failed tests, explained on the Run page and in the Runs side panel
//! (#323, ADR-0025). A failed test is shown under each node it checks; one that checks
//! no node the run shows is listed on its own on the Run page. A test is keyed by its
//! [handle](ods_core::failure::check_handle), never by its id.
//!
//! The binary hands in an [`Explainer`]: a provider's error catalogue and what the
//! project says now (its index, parents and missing columns from column lineage). The
//! pages explain a run's failed nodes when they are shown, from its journal, the
//! states before and after it, and earlier runs: nothing is stored, and nothing here
//! reads an engine's text (the catalogue classifies the redacted summary).

use std::collections::BTreeMap;
use std::sync::Arc;

use ods_core::failure::{ErrorExplanation, MissingColumn};
use ods_core::state::StateSnapshot;
use ods_sdk::contracts::error_catalogue::{Classification, ErrorCatalogue, ProjectIndex};
use ods_sdk::contracts::run_events::RunSummary;
use ods_state::{
    FailureFacts, FailureStage, explain_failure, failed_checks, failed_nodes, plan_from_states,
};

/// A run's failures, explained.
#[derive(Debug, Clone, Default)]
pub(crate) struct Explained {
    /// Each failed node's explanation, by node id.
    pub(crate) nodes: BTreeMap<String, ErrorExplanation>,
    /// Each failed test's explanation, under every node it checks, by node id.
    pub(crate) tests: BTreeMap<String, Vec<ErrorExplanation>>,
    /// Failed tests the engine didn't say the nodes of.
    pub(crate) unattached: Vec<ErrorExplanation>,
}

/// What explains failed nodes, beyond their runs: set by the binary, which picks the
/// provider.
#[derive(Clone)]
#[non_exhaustive]
pub struct Explainer {
    catalogue: Arc<dyn ErrorCatalogue>,
    index: Option<Arc<ProjectIndex>>,
    missing: Arc<BTreeMap<String, Vec<MissingColumn>>>,
    parents: Arc<BTreeMap<String, Vec<String>>>,
    /// The engine invocation that wrote the project's artifacts: a run with that id is
    /// the one they describe.
    invocation: Option<String>,
    /// The state database a retry names, when it isn't the default.
    state_db: Option<String>,
}

impl std::fmt::Debug for Explainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Explainer")
            .field("catalogue", &self.catalogue.catalogue())
            .field("index", &self.index.as_ref().map(|i| i.nodes.len()))
            .field("missing", &self.missing.len())
            .finish_non_exhaustive()
    }
}

impl Explainer {
    /// Explains with `catalogue`'s patterns.
    pub fn new(catalogue: Arc<dyn ErrorCatalogue>) -> Self {
        Self {
            catalogue,
            index: None,
            missing: Arc::default(),
            parents: Arc::default(),
            invocation: None,
            state_db: None,
        }
    }

    /// Adds the project as the provider describes it.
    #[must_use]
    pub fn with_index(mut self, index: ProjectIndex) -> Self {
        self.index = Some(Arc::new(index));
        self
    }

    /// Adds, per node, the columns it reads that its upstreams don't produce (column
    /// lineage).
    #[must_use]
    pub fn with_missing_columns(mut self, missing: BTreeMap<String, Vec<MissingColumn>>) -> Self {
        self.missing = Arc::new(missing);
        self
    }

    /// Adds each node's parents, as the project has them now.
    #[must_use]
    pub fn with_parents(mut self, parents: BTreeMap<String, Vec<String>>) -> Self {
        self.parents = Arc::new(parents);
        self
    }

    /// Says which engine invocation wrote the project's artifacts (for dbt, the
    /// manifest's): only that run's explanations are confirmed by them (#323 review).
    #[must_use]
    pub fn with_artifacts_from(mut self, invocation: Option<String>) -> Self {
        self.invocation = invocation;
        self
    }

    /// Names the state database in retry commands, when it isn't the default.
    #[must_use]
    pub fn with_retry_state_db(mut self, state_db: Option<String>) -> Self {
        self.state_db = state_db;
        self
    }

    /// Explains each failed node and failed test of `run`. `before` and `after` are the
    /// states around it, `earlier` the runs before it (newest first), and `last`
    /// whether it is the last run, the one a retry retries.
    pub(crate) fn explain(
        &self,
        run: &RunSummary,
        (before, after): (Option<&StateSnapshot>, Option<&StateSnapshot>),
        earlier: &[RunSummary],
        last: bool,
    ) -> Explained {
        let failed = failed_nodes(run);
        let checks = failed_checks(run);
        if failed.is_empty() && checks.is_empty() {
            return Explained::default();
        }
        let parents = |id: &str| self.parents.get(id).cloned().unwrap_or_default();
        let plan = plan_from_states(run, before, after, &parents);
        let info = self.catalogue.catalogue();
        let project_is_run =
            self.invocation.is_some() && self.invocation.as_deref() == run.run_id.as_deref();
        let mut explained = Explained::default();
        for node in failed {
            let Some(summary) = run.get(node) else {
                continue;
            };
            let classification = match &summary.stats.error {
                Some(error) => self.catalogue.classify(error),
                None => Classification::NotRecognised {
                    category: ods_core::failure::ErrorCategory::Unknown,
                },
            };
            let mut facts = FailureFacts::new(node, &classification, &info, FailureStage::Run);
            facts.error = summary.stats.error.as_ref();
            facts.stats = Some(&summary.stats);
            facts.run = Some(run);
            facts.plan = Some(&plan);
            facts.before = before;
            facts.history = earlier;
            facts.index = self.index.as_deref();
            facts.missing_columns = self.missing.get(node).map_or(&[], Vec::as_slice);
            facts.retry = last.then(|| ods_state::Retry::new(self.state_db.as_deref()));
            facts.project_is_run = project_is_run;
            explained
                .nodes
                .insert(node.to_owned(), explain_failure(&facts));
        }
        for check in checks {
            let classification = match &check.error {
                Some(error) => self.catalogue.classify(error),
                None => Classification::NotRecognised {
                    category: ods_core::failure::ErrorCategory::Unknown,
                },
            };
            let mut facts =
                FailureFacts::new(&check.check, &classification, &info, FailureStage::Run);
            facts.check = Some(check);
            facts.error = check.error.as_ref();
            facts.run = Some(run);
            facts.plan = Some(&plan);
            facts.before = before;
            facts.history = earlier;
            facts.index = self.index.as_deref();
            facts.state_db = self.state_db.as_deref();
            facts.project_is_run = project_is_run;
            let explanation = explain_failure(&facts);
            if check.covers.is_empty() {
                explained.unattached.push(explanation);
                continue;
            }
            for node in &check.covers {
                explained
                    .tests
                    .entry(node.clone())
                    .or_default()
                    .push(explanation.clone());
            }
        }
        explained
    }
}
