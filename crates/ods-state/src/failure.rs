//! Explains a failed node from a provider's classification and ODS's own evidence
//! (#323, ADR-0025).
//!
//! [`explain_failure`] is pure and synchronous. The host gathers the facts
//! ([`FailureFacts`]): the node's redacted error and stats from the run's journal, the
//! provider's [classification](Classification), the plan, the last committed state,
//! earlier runs, the project index, and column lineage. Nothing here reads an
//! engine's text: the classification did that, from the redacted summary (AGENTS.md
//! rules 1 and 9). Explanations are computed when shown, never stored, so older runs
//! get better explanations as the catalogue grows.
//!
//! Evidence **confirms** a recognised pattern only when it independently shows the
//! same thing, about one candidate, from the code the run ran: column lineage for a
//! missing column (one column, from an upstream built from the code lineage read), the
//! project for an undefined macro (one undefined call), the state for a parent that was
//! never built. Everything else (what changed, how long it ran, earlier runs, several
//! candidates, or the project as it is now for an older run) is context. An
//! unrecognised error gets context only, and its category's neutral headline (rule 3).
//!
//! A failed **check** (e.g. a data test, #323) is explained too, with
//! [`FailureFacts::check`]: which test, on which column of which node, and how many rows
//! don't pass. It found failing rows when the catalogue recognises its message as a
//! [failed test](Symptom::TestFailed), or, with no message ODS recognises, when the
//! engine reported a count of failing rows; that count is what confirms it. A check
//! that errored (its message is another recognised error, e.g. a compile error in the
//! test) is explained like a node's error, never as rows that failed. Checks that only
//! warned aren't explained: they didn't fail anything. A check is named by what it
//! tests, never by its id, which an engine may build from the test's arguments (dbt
//! names `accepted_values` tests after the values they accept): its explanation's node
//! is the check's [handle](ods_core::failure::check_handle). The steps it offers are
//! neutral (test the node again), plus what the provider's pattern offers for running
//! it again (e.g. dbt's `--store-failures`, [`Rerun`]).
//!
//! A reference to a missing node gets **did-you-mean** (#323): the project's nodes that
//! others refer to by name ([`IndexedNode::referable`]) within two edits of the name
//! the reference used (the pattern's subject), at most three, closest first. It is a
//! suggestion from the names alone, never evidence that the name is a typo, and the
//! name the reference used is never shown: only the project's own names are.
//!
//! The host may also run health checks (`ods doctor`'s local ones, #181) and pass what
//! they found as neutral [`DoctorFinding`]s: each becomes evidence from
//! [`EvidenceSource::Doctor`], which confirms only when the host says the finding shows
//! the very symptom the pattern recognised.

use ods_core::FreshnessPolicy;
use ods_core::diagnostic::CheckStatus as HealthStatus;
use ods_core::failure::{
    EngineMessage, ErrorCategory, ErrorExplanation, EvidenceData, EvidenceItem, EvidenceSource,
    ExplanationBuilder, FailedCheck, Location, MissingColumn, PatternRef, Suggestion, Symptom,
    Text, check_handle, is_code,
};
use ods_core::state::{
    ExecutionPlan, NodeState, PlanAction, PlanEntry, Reason, ReasonCode, StateSnapshot, Timestamp,
};
use ods_sdk::contracts::error_catalogue::{
    CatalogueInfo, CheckTarget, Classification, IndexedNode, NameAt, ProjectIndex, Rerun,
};
use ods_sdk::contracts::run_events::{
    CheckStatus, CheckSummary, ErrorSummary, NodeRunStats, NodeRunStatus, RunSummary,
};

/// How many earlier runs of a node its history looks at.
pub const HISTORY_RUNS: usize = 5;

/// How to retry what failed in the last run: `ods state retry --failed`, with the
/// state database when the run didn't use the default one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Retry<'a> {
    /// The state database, when not the default.
    pub state_db: Option<&'a str>,
}

impl<'a> Retry<'a> {
    /// Retrying with the state database at `state_db`, or the default one.
    pub fn new(state_db: Option<&'a str>) -> Self {
        Self { state_db }
    }

    /// Testing `node` again (`ods state test --select <node>`), with the engine's
    /// `passthrough` argument after `--` when the provider offered one ([`Rerun`]).
    fn test_again(self, text: Text, node: &str, passthrough: Option<&str>) -> Suggestion {
        let s = Suggestion::new(text);
        match (self.state_db, passthrough) {
            (Some(db), Some(arg)) => s.with_command(
                "ods state test --select {} --state-db {} -- {}",
                &[node, db, arg],
            ),
            (Some(db), None) => {
                s.with_command("ods state test --select {} --state-db {}", &[node, db])
            }
            (None, Some(arg)) => s.with_command("ods state test --select {} -- {}", &[node, arg]),
            (None, None) => s.with_command("ods state test --select {}", &[node]),
        }
    }

    /// The suggestion's command.
    fn suggest(self, text: &str) -> Suggestion {
        let s = Suggestion::new(Text::new().plain(text));
        match self.state_db {
            Some(db) => s.with_command("ods state retry --failed --state-db {}", &[db]),
            None => s.with_command("ods state retry --failed", &[]),
        }
    }
}

/// When the node failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureStage {
    /// While the project was prepared, before anything ran.
    Prepare,
    /// During the run.
    Run,
}

/// What one health check (e.g. `ods doctor`'s `config.resolution`) found, as neutral
/// data for an explanation (#323, #181). The host runs only local checks with no side
/// effects and no connection, right after the failure, and words what they found
/// itself; a check's output never holds a secret (ADR-0023), and the text keeps only
/// identifier-shaped names in code spans ([`Text`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DoctorFinding {
    /// The check's stable id, e.g. `config.resolution`.
    pub check: String,
    /// What it concluded.
    pub status: HealthStatus,
    /// What it found, for people.
    pub text: Text,
    /// The symptom this finding shows on its own, when the host knows it does (e.g. a
    /// profiles directory with no profiles file shows that no profile can be found).
    /// It confirms an explanation only when the catalogue recognised that symptom.
    pub shows: Option<Symptom>,
}

impl DoctorFinding {
    /// Check `check` concluded `status`, and found what `text` says.
    pub fn new(check: impl Into<String>, status: HealthStatus, text: Text) -> Self {
        Self {
            check: check.into(),
            status,
            text,
            shows: None,
        }
    }

    /// Says the finding shows `symptom` on its own.
    #[must_use]
    pub fn showing(mut self, symptom: Symptom) -> Self {
        self.shows = Some(symptom);
        self
    }
}

/// Everything ODS knows about a failed node. Only [`FailureFacts::new`]'s arguments
/// are needed; each other fact adds evidence when present.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FailureFacts<'a> {
    /// The failed node's id.
    pub node: &'a str,
    /// What the provider's catalogue made of its error.
    pub classification: &'a Classification,
    /// The catalogue.
    pub catalogue: &'a CatalogueInfo,
    /// When it failed.
    pub stage: FailureStage,
    /// Its (redacted) error.
    pub error: Option<&'a ErrorSummary>,
    /// Its stats in this run.
    pub stats: Option<&'a NodeRunStats>,
    /// The run, for what it blocked.
    pub run: Option<&'a RunSummary>,
    /// The plan the run built from.
    pub plan: Option<&'a ExecutionPlan>,
    /// The state committed before the run: each node's last successful build.
    pub before: Option<&'a StateSnapshot>,
    /// Earlier runs, newest first.
    pub history: &'a [RunSummary],
    /// The project as the provider describes it.
    pub index: Option<&'a ProjectIndex>,
    /// Columns the node reads that its upstreams don't produce, from column lineage.
    pub missing_columns: &'a [MissingColumn],
    /// How to retry what failed, when the host can retry this run.
    pub retry: Option<Retry<'a>>,
    /// Whether the project index and column lineage describe the code this run ran
    /// (right after the run, or nothing ran since). When not, they describe the project
    /// as it is now: their evidence is context, never confirmation.
    pub project_is_run: bool,
    /// The check, when it is a check that failed (e.g. a data test): then
    /// [`node`](Self::node) is the check's id (#323).
    pub check: Option<&'a CheckSummary>,
    /// The state database a suggested command names, when it isn't the default.
    pub state_db: Option<&'a str>,
    /// What health checks found about the environment, run for this failure right
    /// after it (#181): evidence from [`EvidenceSource::Doctor`].
    pub doctor: &'a [DoctorFinding],
}

impl<'a> FailureFacts<'a> {
    /// Facts about `node`, classified by `catalogue`, failed at `stage`.
    pub fn new(
        node: &'a str,
        classification: &'a Classification,
        catalogue: &'a CatalogueInfo,
        stage: FailureStage,
    ) -> Self {
        Self {
            node,
            classification,
            catalogue,
            stage,
            error: None,
            stats: None,
            run: None,
            plan: None,
            before: None,
            history: &[],
            index: None,
            missing_columns: &[],
            retry: None,
            project_is_run: true,
            check: None,
            state_db: None,
            doctor: &[],
        }
    }

    fn entry(&self, node: &str) -> Option<&'a PlanEntry> {
        self.plan?.entries.iter().find(|e| e.node == node)
    }

    fn indexed(&self, node: &str) -> Option<&'a IndexedNode> {
        self.index?.nodes.get(node)
    }

    fn last_good(&self, node: &str) -> Option<&'a NodeState> {
        self.before?.nodes.get(node)
    }

    /// A node's name for people: the project's, the plan's, or the id's last part.
    fn name(&self, node: &str) -> String {
        self.indexed(node)
            .map(|n| n.name.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| self.entry(node).map(|e| e.name.clone()))
            .unwrap_or_else(|| node.rsplit('.').next().unwrap_or(node).to_owned())
    }

    fn status_in_run(&self, node: &str) -> Option<NodeRunStatus> {
        self.run?.get(node).map(|n| n.stats.status)
    }
}

/// The failed nodes of `run` to explain, in run order: each node that failed, except
/// one with no error of its own whose failed checks failed it (as in a test run, where
/// nothing is built): its checks are explained instead ([`failed_checks`]).
pub fn failed_nodes(run: &RunSummary) -> Vec<&str> {
    run.nodes
        .iter()
        .filter(|n| n.stats.status == NodeRunStatus::Error)
        .filter(|n| {
            n.stats.error.is_some()
                || !run
                    .checks
                    .iter()
                    .any(|c| c.status == CheckStatus::Failed && c.covers.contains(&n.node))
        })
        .map(|n| n.node.as_str())
        .collect()
}

/// The checks of `run` that failed, in the order they finished. A check that only
/// warned didn't fail anything, so it isn't listed.
pub fn failed_checks(run: &RunSummary) -> Vec<&CheckSummary> {
    run.checks
        .iter()
        .filter(|c| c.status == CheckStatus::Failed)
        .collect()
}

/// Explains a failed node, or a failed check (see the module docs).
pub fn explain_failure(facts: &FailureFacts<'_>) -> ErrorExplanation {
    if let Some(check) = facts.check
        && found_failing_rows(facts, check)
    {
        return explain_failing_rows(facts, check);
    }
    let category = match (facts.check, facts.error, facts.classification) {
        // A check that failed without a word: a test failure, how is unknown.
        (Some(_), None, c) if !matches!(c, Classification::Recognised(_)) => {
            ErrorCategory::TestFailure
        }
        _ => facts.classification.category(),
    };
    let mut builder = ExplanationBuilder::new(shown_id(facts), category);
    if let Some(check) = facts.check {
        builder = builder.check(failed_check(facts, check));
        // Another recognised error than failing rows: the check errored.
        if matches!(facts.classification, Classification::Recognised(_)) {
            builder = builder.detail(
                test_phrase(facts, check, true)
                    .plain(" couldn't run, so it says nothing about the data yet."),
            );
        }
    }
    if let Classification::Recognised(found) = facts.classification {
        builder = builder.recognised(
            PatternRef::new(
                facts.catalogue.name.clone(),
                facts.catalogue.version.clone(),
                found.id.clone(),
            ),
            found.symptom,
        );
        builder =
            match found.symptom {
                Symptom::MissingColumn => missing_column(facts, builder),
                Symptom::UnknownMacro => unknown_macro(facts, builder),
                Symptom::MissingRelation => missing_relation(facts, builder),
                Symptom::PythonException => {
                    let python = facts
                        .indexed(facts.node)
                        .and_then(|n| n.language.as_deref())
                        == Some("python");
                    match (&found.subject, python) {
                        (Some(exception), true) => builder.headline(
                            Text::new()
                                .plain("The Python model raised ")
                                .code(exception),
                        ),
                        (Some(exception), false) => builder.headline(
                            Text::new()
                                .plain("It raised the Python exception ")
                                .code(exception),
                        ),
                        (None, true) => builder,
                        (None, false) => {
                            builder.headline(Text::new().plain("It raised a Python exception"))
                        }
                    }
                }
                Symptom::MissingRef => similar_ref(facts, found.subject.as_deref(), builder)
                    .suggest(Suggestion::new(Text::new().plain(
                        "Check the names the model refers to against the project's nodes.",
                    ))),
                _ => builder,
            };
        // A macro with a close name comes first: without evidence that packages are
        // missing, a typo is the likelier cause.
        if found.symptom == Symptom::UnknownMacro {
            builder = similar_macro(facts, builder);
        }
        for suggestion in &found.suggestions {
            builder = builder.suggest(suggestion.clone());
        }
        builder = symptom_steps(found.symptom, builder);
        builder = doctor_evidence(facts, Some(found.symptom), builder);
    } else {
        builder = builder.detail(Text::new().plain(
            "ODS doesn't recognise this error, so it won't guess the cause. Here is what it does know.",
        ));
        builder = doctor_evidence(facts, None, builder);
    }
    builder = match facts.check {
        Some(check) => check_context(facts, check, builder),
        None => context(facts, builder),
    };
    builder = where_it_is(facts, builder);
    builder = match facts.check {
        Some(check) => check_steps(facts, check, builder),
        None => next_steps(facts, builder),
    };
    builder = impact(facts, builder);
    if let Some(error) = facts.error {
        builder = builder.engine_message(EngineMessage::new(
            &facts.catalogue.engine,
            error.kind(),
            error.message(),
            error.details_at(),
        ));
    }
    builder.build()
}

/// The failed node's id, or a failed check's [handle](check_handle): a check's id may
/// hold its arguments.
fn shown_id(facts: &FailureFacts<'_>) -> String {
    match facts.check {
        Some(_) => check_handle(facts.node),
        None => facts.node.to_owned(),
    }
}

/// ODS's own pattern for a check whose engine reported failing rows without a message
/// the catalogue recognises: the count is the engine's structured report, not its text.
const FAILING_ROWS_PATTERN: (&str, &str, &str) = ("ods", "1", "check-failing-rows");

/// The rows the engine said a check found that don't pass; zero says nothing.
fn failing_rows(check: &CheckSummary) -> Option<u64> {
    check.failures.filter(|rows| *rows > 0)
}

/// Whether the check ran and found rows that don't pass: its message is a recognised
/// failed test, or, without a message ODS recognises, the engine counted failing rows.
fn found_failing_rows(facts: &FailureFacts<'_>, check: &CheckSummary) -> bool {
    match facts.classification {
        Classification::Recognised(found) => found.symptom == Symptom::TestFailed,
        _ => failing_rows(check).is_some(),
    }
}

/// What the project says the check tests, when it knows.
fn check_target<'a>(facts: &FailureFacts<'a>) -> Option<&'a CheckTarget> {
    facts.indexed(facts.node)?.check.as_ref()
}

/// The one node the check is about: the one the project attaches it to, or the only
/// one it covers.
fn tested_node<'a>(facts: &FailureFacts<'a>, check: &'a CheckSummary) -> Option<&'a str> {
    check_target(facts)
        .and_then(|t| t.node.as_deref())
        .or(match check.covers.as_slice() {
            [one] => Some(one.as_str()),
            _ => None,
        })
}

/// The check, for machines: what it covers, tests and on which column.
fn failed_check(facts: &FailureFacts<'_>, check: &CheckSummary) -> FailedCheck {
    let target = check_target(facts);
    FailedCheck::new(check.covers.clone()).testing(
        target.and_then(|t| t.test.as_deref()),
        target.and_then(|t| t.column.as_deref()),
    )
}

/// The check, for people: "the `not_null` test on `customer_id` of `orders`", "the
/// test `only_placed_orders` on `orders`", or "a test on `orders`". A test's own name
/// is used only for one the provider says is [singular](CheckTarget::singular) (named
/// by its file): a generic test's name may hold its arguments.
fn test_phrase(facts: &FailureFacts<'_>, check: &CheckSummary, start: bool) -> Text {
    let the = if start { "The " } else { "the " };
    let target = check_target(facts);
    let node = tested_node(facts, check).map(|n| facts.name(n));
    let a_test = if start { "A test" } else { "a test" };
    let mut text = Text::new();
    match target {
        Some(CheckTarget {
            test: Some(kind), ..
        }) => {
            text = text.plain(the).code(kind).plain(" test");
        }
        Some(CheckTarget { singular: true, .. }) => {
            let name = facts.indexed(facts.node).map(|n| n.name.as_str());
            text = match name.filter(|n| !n.is_empty()) {
                Some(name) => text.plain(the).plain("test ").code(name),
                None => text.plain(a_test),
            };
        }
        _ => text = text.plain(a_test),
    }
    let column = target.and_then(|t| t.column.as_deref());
    match (column, node) {
        (Some(column), Some(node)) => text.plain(" on ").code(column).plain(" of ").code(&node),
        (None, Some(node)) => text.plain(" on ").code(&node),
        (Some(column), None) => text.plain(" on ").code(column),
        (None, None) => text,
    }
}

/// A check that ran and found rows that don't pass (see the module docs).
fn explain_failing_rows(facts: &FailureFacts<'_>, check: &CheckSummary) -> ErrorExplanation {
    let pattern = if let Classification::Recognised(found) = facts.classification {
        PatternRef::new(
            facts.catalogue.name.clone(),
            facts.catalogue.version.clone(),
            found.id.clone(),
        )
    } else {
        let (catalogue, version, id) = FAILING_ROWS_PATTERN;
        PatternRef::new(catalogue, version, id)
    };
    let rows = failing_rows(check);
    let engine = &facts.catalogue.engine;
    let headline = test_phrase(facts, check, true).plain(&match rows {
        Some(1) => " failed: 1 row doesn't pass".to_owned(),
        Some(n) => format!(" failed: {n} rows don't pass"),
        None => format!(" failed; {engine} didn't say how many rows don't pass"),
    });
    let mut builder = ExplanationBuilder::new(shown_id(facts), ErrorCategory::TestFailure)
        .check(failed_check(facts, check))
        .recognised(pattern, Symptom::TestFailed)
        .headline(headline);
    if let Classification::Recognised(found) = facts.classification {
        for suggestion in &found.suggestions {
            builder = builder.suggest(suggestion.clone());
        }
    }
    if let Some(rows) = rows {
        builder = builder.evidence(
            EvidenceItem::confirming(
                EvidenceSource::RunStats,
                Text::new().plain(&format!(
                    "{engine} reported {rows} {} that {} the test.",
                    if rows == 1 { "row" } else { "rows" },
                    if rows == 1 { "fails" } else { "fail" }
                )),
            )
            .with_data(EvidenceData::FailingRows { rows }),
        );
    }
    builder = check_context(facts, check, builder);
    builder = where_it_is(facts, builder);
    builder = check_steps(facts, check, builder);
    builder = impact(facts, builder);
    if let Some(error) = facts.error {
        builder = builder.engine_message(EngineMessage::new(
            engine,
            error.kind(),
            error.message(),
            error.details_at(),
        ));
    }
    builder.build()
}

/// What ODS knows about a failed check: what it tests, whether the node it tests
/// changed in this run, and how it did before.
fn check_context(
    facts: &FailureFacts<'_>,
    check: &CheckSummary,
    mut builder: ExplanationBuilder,
) -> ExplanationBuilder {
    if let Some(target) = check_target(facts) {
        let node = tested_node(facts, check);
        let mut text = Text::new().plain(if facts.project_is_run {
            "The project declares it as "
        } else {
            "As the project is now, it is "
        });
        text = match (&target.test, target.singular) {
            (Some(kind), _) => text.plain("a ").code(kind).plain(" test"),
            (None, true) => text.plain("a test of its own (a singular test)"),
            (None, false) => text.plain("a test"),
        };
        if let Some(column) = &target.column {
            text = text.plain(" of column ").code(column);
        }
        if let Some(node) = node {
            text = text.plain(" on ").code(&facts.name(node));
        }
        builder = builder.evidence(
            EvidenceItem::context(EvidenceSource::Project, text.plain(".")).with_data(
                EvidenceData::TestTarget {
                    test: target.test.clone(),
                    column: target.column.clone(),
                    node: node.map(str::to_owned),
                },
            ),
        );
    }
    if let Some(node) = tested_node(facts, check) {
        let name = facts.name(node);
        let text = match (changed_in_run(facts, node), facts.status_in_run(node)) {
            (Some(components), _) if !components.is_empty() => Some(
                Text::new()
                    .code(&name)
                    .plain(&format!(
                        " changed in this run ({}), so the test checked its new build.",
                        components.join(", ")
                    )),
            ),
            (Some(_), _) => Some(
                Text::new()
                    .code(&name)
                    .plain(" changed in this run, so the test checked its new build."),
            ),
            (None, Some(NodeRunStatus::Success)) => facts
                .entry(node)
                .filter(|e| e.before.is_some() && e.before == e.after)
                .map(|_| {
                    Text::new()
                        .code(&name)
                        .plain(" was rebuilt in this run, but its code didn't change since its last successful build.")
                }),
            // Not built in this run: what ODS recorded, not which build the test read.
            (None, None) => facts.last_good(node).map(|last| {
                Text::new()
                    .plain("The last build ODS recorded for ")
                    .code(&name)
                    .plain(" is from run ")
                    .code(short_run(&last.run_id))
                    .plain(".")
            }),
            _ => None,
        };
        if let Some(text) = text {
            builder = builder.evidence(EvidenceItem::context(EvidenceSource::Fingerprint, text));
        }
    }
    check_history(facts, builder)
}

/// How the check did in earlier runs: the same check failing again is a pattern.
fn check_history(facts: &FailureFacts<'_>, builder: ExplanationBuilder) -> ExplanationBuilder {
    let earlier: Vec<CheckStatus> = facts
        .history
        .iter()
        .filter_map(|run| run.check(facts.node).map(|c| c.status))
        .filter(|s| {
            matches!(
                s,
                CheckStatus::Passed | CheckStatus::Failed | CheckStatus::Warned
            )
        })
        .take(HISTORY_RUNS)
        .collect();
    let n = earlier.len();
    if n == 0 {
        return builder;
    }
    let last = if n == 1 {
        "its last run".to_owned()
    } else {
        format!("its last {n} runs")
    };
    let failed = earlier
        .iter()
        .filter(|s| **s == CheckStatus::Failed)
        .count();
    let text = if failed > 0 {
        format!("This test failed in {failed} of {last} too.")
    } else if earlier.iter().all(|s| *s == CheckStatus::Passed) {
        format!("This test passed in {last}.")
    } else {
        return builder;
    };
    builder.evidence(EvidenceItem::context(
        EvidenceSource::RunHistory,
        Text::new().plain(&text),
    ))
}

/// What to try for a failed check: look at the rows that fail it, when the provider's
/// pattern offers a way ([`Rerun`]), then test the node again; for one that errored or
/// isn't recognised, read the engine's message first.
fn check_steps(
    facts: &FailureFacts<'_>,
    check: &CheckSummary,
    mut builder: ExplanationBuilder,
) -> ExplanationBuilder {
    let rows = found_failing_rows(facts, check);
    if !builder.is_recognised() {
        builder = builder.suggest(Suggestion::new(Text::new().plain(&format!(
            "Read {}'s message below and the full log.",
            facts.catalogue.engine
        ))));
    }
    let state_db = facts.state_db.or(facts.retry.and_then(|r| r.state_db));
    let commands = Retry::new(state_db);
    let Some(node) = tested_node(facts, check) else {
        return builder;
    };
    let name = facts.name(node);
    let rerun: Option<&Rerun> = match facts.classification {
        Classification::Recognised(found) if rows => found.rerun.as_ref(),
        _ => None,
    };
    if let Some(rerun) = rerun {
        builder = builder.suggest(commands.test_again(
            rerun.text.clone(),
            &name,
            Some(&rerun.passthrough),
        ));
    }
    builder.suggest(
        commands.test_again(
            Text::new()
                .plain(if rows {
                    "Fix the data, the model or the test, then test "
                } else {
                    "Once it is fixed, test "
                })
                .code(&name)
                .plain(" again:"),
            &name,
            None,
        ),
    )
}

/// Did-you-mean: the candidates closest to `name`, at most two edits away (or equal
/// but for case), best first.
fn similar<'c>(name: &str, candidates: impl IntoIterator<Item = &'c String>) -> Vec<&'c str> {
    let lower = name.to_lowercase();
    let mut close: Vec<(usize, &str)> = candidates
        .into_iter()
        .filter(|c| c.as_str() != name)
        .filter_map(|c| {
            let d = strsim::levenshtein(&lower, &c.to_lowercase());
            (d <= 2).then_some((d, c.as_str()))
        })
        .collect();
    close.sort_unstable();
    close.into_iter().map(|(_, c)| c).take(3).collect()
}

/// Whether an upstream's output, as column lineage read it, is what the run read: it
/// was rebuilt in this run, or its committed build is the code there is now.
fn upstream_is_current(facts: &FailureFacts<'_>, upstream: &str) -> bool {
    facts.status_in_run(upstream) == Some(NodeRunStatus::Success)
        || matches!(
            (facts.last_good(upstream), facts.entry(upstream)),
            (Some(last), Some(entry)) if entry.after.as_deref() == Some(last.fingerprint.digest.as_str())
        )
}

/// "Column lineage: " or, for an older run, "Column lineage, as the project is now: ".
fn lineage_label(facts: &FailureFacts<'_>) -> Text {
    Text::new().plain(if facts.project_is_run {
        "Column lineage: "
    } else {
        "Column lineage, as the project is now: "
    })
}

fn missing_column(facts: &FailureFacts<'_>, builder: ExplanationBuilder) -> ExplanationBuilder {
    let candidates = facts.missing_columns;
    let data = EvidenceData::MissingColumns {
        columns: candidates.to_vec(),
    };
    let confirmed = match candidates {
        [one] => facts.project_is_run && upstream_is_current(facts, &one.upstream),
        _ => false,
    };
    if !confirmed {
        return missing_columns_as_context(facts, builder, data);
    }
    let missing = &candidates[0];
    let node = facts.name(facts.node);
    let upstream = facts.name(&missing.upstream);
    let had_it = facts.last_good(facts.node).is_some();
    let mut builder = builder
        .headline(Text::new().plain(if had_it {
            "A column this model reads no longer exists upstream"
        } else {
            "A column this model reads doesn't exist upstream"
        }))
        .detail(
            Text::new()
                .code(&node)
                .plain(" reads ")
                .code(&missing.column)
                .plain(" from ")
                .code(&upstream)
                .plain(if had_it {
                    ", but that model no longer produces it."
                } else {
                    ", but that model doesn't produce it."
                }),
        );
    let lineage = match missing.renamed_to.as_slice() {
        [] => lineage_label(facts)
            .code(&upstream)
            .plain(" doesn't output ")
            .code(&missing.column)
            .plain("."),
        renamed => {
            let mut text = lineage_label(facts).code(&upstream).plain(" now outputs ");
            for (i, to) in renamed.iter().enumerate() {
                if i > 0 {
                    text = text.plain(", ");
                }
                text = text.code(to);
            }
            text.plain(", not ")
                .code(&missing.column)
                .plain(", from the same input column.")
        }
    };
    builder = builder
        .evidence(EvidenceItem::confirming(EvidenceSource::ColumnLineage, lineage).with_data(data));
    let changed = changed_in_run(facts, &missing.upstream);
    if let Some(components) = &changed {
        let mut text = Text::new().code(&upstream).plain(" changed in this run");
        if !components.is_empty() {
            text = text.plain(&format!(" ({})", components.join(", ")));
        }
        if facts.status_in_run(&missing.upstream) == Some(NodeRunStatus::Success) {
            text = text.plain(" and was rebuilt first");
        }
        builder = builder.evidence(EvidenceItem::context(
            EvidenceSource::Fingerprint,
            text.plain("."),
        ));
    }
    if let Some(last) = facts.last_good(facts.node) {
        let mut text = Text::new()
            .code(&node)
            .plain(" last built fine in run ")
            .code(short_run(&last.run_id));
        if changed.is_some() {
            text = text.plain(", before that change");
        }
        builder = builder.evidence(EvidenceItem::context(
            EvidenceSource::RunHistory,
            text.plain("."),
        ));
    }
    missing_column_steps(missing, &node, &upstream, builder)
}

/// Missing columns that can't confirm the pattern (several, a stale upstream, or the
/// project as it is now): listed as context, each with how to see what else reads it.
fn missing_columns_as_context(
    facts: &FailureFacts<'_>,
    builder: ExplanationBuilder,
    data: EvidenceData,
) -> ExplanationBuilder {
    let candidates = facts.missing_columns;
    if candidates.is_empty() {
        return builder;
    }
    let mut text = lineage_label(facts);
    for (i, m) in candidates.iter().enumerate() {
        if i > 0 {
            text = text.plain("; ");
        }
        text = text
            .code(&facts.name(&m.upstream))
            .plain(" doesn't output ")
            .code(&m.column);
    }
    let mut builder = builder.evidence(
        EvidenceItem::context(EvidenceSource::ColumnLineage, text.plain(".")).with_data(data),
    );
    for m in candidates.iter().take(3) {
        builder = builder.suggest(impact_step(m, &facts.name(&m.upstream)));
    }
    builder
}

/// Seeing what else reads a missing column: `ods lineage impact`, by the upstream's
/// unique id, which it always resolves.
fn impact_step(missing: &MissingColumn, upstream: &str) -> Suggestion {
    Suggestion::new(
        Text::new()
            .plain("See what else reads ")
            .code(&format!("{upstream}.{}", missing.column))
            .plain(":"),
    )
    .with_command(
        "ods lineage impact --column {}.{}=removed",
        &[&missing.upstream, &missing.column],
    )
}

/// What to try for a missing column: use its new name, or restore it; and see what
/// else reads it.
fn missing_column_steps(
    missing: &MissingColumn,
    node: &str,
    upstream: &str,
    builder: ExplanationBuilder,
) -> ExplanationBuilder {
    let fix = match missing.renamed_to.as_slice() {
        [to] => Text::new()
            .plain("Use ")
            .code(to)
            .plain(" in ")
            .code(node)
            .plain(", or restore ")
            .code(&missing.column)
            .plain(" in ")
            .code(upstream)
            .plain("."),
        _ => Text::new()
            .plain("Restore ")
            .code(&missing.column)
            .plain(" in ")
            .code(upstream)
            .plain(", or stop reading it in ")
            .code(node)
            .plain("."),
    };
    builder
        .suggest(Suggestion::new(fix))
        .suggest(impact_step(missing, upstream))
}

/// The components of `node`'s code that changed, when the plan built it for a change
/// of its own code.
fn changed_in_run(facts: &FailureFacts<'_>, node: &str) -> Option<Vec<String>> {
    let entry = facts.entry(node)?;
    (entry.action == PlanAction::Build
        && entry
            .reasons
            .iter()
            .any(|r| r.code == ReasonCode::CodeChanged))
    .then(|| entry.changed_components.clone())
}

fn short_run(run_id: &str) -> &str {
    run_id.get(..8).unwrap_or(run_id)
}

/// The one undefined macro call ODS can name: exactly one, in the code the run ran.
fn undefined_call<'a>(facts: &FailureFacts<'a>) -> Option<&'a NameAt> {
    match facts.indexed(facts.node)?.undefined_calls.as_slice() {
        [one] if facts.project_is_run => Some(one),
        _ => None,
    }
}

fn unknown_macro(facts: &FailureFacts<'_>, builder: ExplanationBuilder) -> ExplanationBuilder {
    let mut builder = builder.detail(Text::new().plain(
        "The model calls a macro that isn't defined in this project or its installed packages, so it stopped before running any SQL. This model changed nothing in the warehouse.",
    ));
    let calls = facts
        .indexed(facts.node)
        .map_or(&[][..], |n| n.undefined_calls.as_slice());
    let data = EvidenceData::UndefinedMacros {
        names: calls.iter().map(|c| c.name.clone()).collect(),
    };
    if let Some(call) = undefined_call(facts) {
        return builder
            .headline(
                Text::new()
                    .plain("The macro ")
                    .code(&call.name)
                    .plain(" isn't defined"),
            )
            .evidence(
                EvidenceItem::confirming(
                    EvidenceSource::Project,
                    Text::new()
                        .code(&call.name)
                        .plain(" isn't among the project's macros."),
                )
                .with_data(data),
            );
    }
    if calls.is_empty() {
        return builder;
    }
    // Several candidates, or the project as it is now: which one failed isn't known.
    let mut text = Text::new().plain(if facts.project_is_run {
        "The model calls names the project doesn't define as macros: "
    } else {
        "As the project is now, the model calls names it doesn't define as macros: "
    });
    for (i, c) in calls.iter().enumerate() {
        if i > 0 {
            text = text.plain(", ");
        }
        text = text.code(&c.name);
    }
    builder = builder
        .evidence(EvidenceItem::context(EvidenceSource::Project, text.plain(".")).with_data(data));
    builder
}

/// Did-you-mean for an undefined macro.
fn similar_macro(facts: &FailureFacts<'_>, mut builder: ExplanationBuilder) -> ExplanationBuilder {
    let Some(call) = undefined_call(facts) else {
        return builder;
    };
    if let Some(index) = facts.index {
        let close = similar(&call.name, &index.macros);
        if let Some((first, rest)) = close.split_first() {
            let mut text = Text::new()
                .plain("Check the name: a macro called ")
                .code(first);
            for other in rest {
                text = text.plain(" or ").code(other);
            }
            builder = builder.suggest(Suggestion::new(text.plain(" exists.")));
        }
    }
    builder
}

/// Did-you-mean for a reference to a missing node: the project's referable nodes with
/// a name close to the one the reference used (`missing`, the pattern's subject). The
/// missing name itself is never shown, only the project's names.
fn similar_ref(
    facts: &FailureFacts<'_>,
    missing: Option<&str>,
    builder: ExplanationBuilder,
) -> ExplanationBuilder {
    let (Some(missing), Some(index)) = (missing, facts.index) else {
        return builder;
    };
    // Names repeat across packages: each once.
    let names: std::collections::BTreeSet<&String> = index
        .nodes
        .values()
        .filter(|n| n.referable && is_code(&n.name))
        .map(|n| &n.name)
        .collect();
    let close = similar(missing, names);
    let Some((first, rest)) = close.split_first() else {
        return builder;
    };
    let mut text = Text::new().plain("Did you mean ").code(first);
    for (i, other) in rest.iter().enumerate() {
        text = text
            .plain(if i + 1 == rest.len() { " or " } else { ", " })
            .code(other);
    }
    text = text.plain(if rest.is_empty() {
        "? The project has a node with a name close to the missing one; that is a guess from the names, not evidence of a typo."
    } else {
        "? The project has nodes with names close to the missing one; that is a guess from the names, not evidence of a typo."
    });
    builder.suggest(Suggestion::new(text))
}

/// What health checks found, as evidence: a finding confirms only when it shows the
/// symptom the pattern recognised.
fn doctor_evidence(
    facts: &FailureFacts<'_>,
    recognised: Option<Symptom>,
    mut builder: ExplanationBuilder,
) -> ExplanationBuilder {
    for finding in facts.doctor {
        let text = Text::new()
            .plain("Check ")
            .code(&finding.check)
            .plain(&format!(" ({}): ", finding.status.name()))
            .append(&finding.text);
        let confirms = recognised.is_some() && finding.shows == recognised;
        let item = if confirms {
            EvidenceItem::confirming(EvidenceSource::Doctor, text)
        } else {
            EvidenceItem::context(EvidenceSource::Doctor, text)
        };
        builder = builder.evidence(item.with_data(EvidenceData::DoctorCheck {
            check: if is_code(&finding.check) {
                finding.check.clone()
            } else {
                "[name hidden]".to_owned()
            },
            status: finding.status,
        }));
    }
    builder
}

fn missing_relation(facts: &FailureFacts<'_>, builder: ExplanationBuilder) -> ExplanationBuilder {
    let Some(entry) = facts.entry(facts.node) else {
        return builder;
    };
    let mut builder = builder;
    for parent in &entry.depends_on {
        let unbuilt = facts.last_good(parent).is_none()
            && facts.entry(parent).is_some()
            && matches!(
                facts.status_in_run(parent),
                Some(NodeRunStatus::Error | NodeRunStatus::Skipped)
            );
        if unbuilt {
            builder = builder.evidence(EvidenceItem::confirming(
                EvidenceSource::RunHistory,
                Text::new()
                    .plain("Upstream ")
                    .code(&facts.name(parent))
                    .plain(" has never been built, and didn't build in this run."),
            ));
        }
    }
    builder
}

/// Steps every error with this symptom gets, whatever the engine.
fn symptom_steps(symptom: Symptom, builder: ExplanationBuilder) -> ExplanationBuilder {
    match symptom {
        Symptom::ProfileNotFound | Symptom::CredentialsMissing | Symptom::WarehouseUnavailable => {
            builder.suggest(
                Suggestion::new(Text::new().plain(
                    "Check the profile, target and credentials ODS and the engine use:",
                ))
                .with_command("ods doctor", &[]),
            )
        }
        Symptom::PermissionDenied => builder.suggest(Suggestion::new(Text::new().plain(
            "Check the grants of the role or user the profile connects as.",
        ))),
        Symptom::TypeMismatch => builder.suggest(Suggestion::new(Text::new().plain(
            "Check the casts and comparisons near the reported line: a value doesn't have the type the query expects there.",
        ))),
        Symptom::ConstraintViolation => builder.suggest(Suggestion::new(Text::new().plain(
            "Find the rows that break the constraint (duplicate keys, or nulls) in the model's inputs.",
        ))),
        Symptom::DependentObjects => builder.suggest(Suggestion::new(Text::new().plain(
            "Another object (e.g. a table with a foreign key to it) depends on this relation, so it can't be replaced: rebuild or drop that object first.",
        ))),
        Symptom::MissingRelation => builder.suggest(Suggestion::new(Text::new().plain(
            "Check the relation's name, and that it was built in this target.",
        ))),
        Symptom::PythonException => builder.suggest(Suggestion::new(Text::new().plain(
            "Read the traceback in the full log: it shows where in the model's code the exception was raised.",
        ))),
        _ => builder,
    }
}

/// What ODS knows whatever the error: how and when it failed, what changed, and how it
/// went before.
fn context(facts: &FailureFacts<'_>, mut builder: ExplanationBuilder) -> ExplanationBuilder {
    let node = facts.node;
    // How long it ran, and in which phase.
    let phase = match (facts.stage, facts.stats) {
        (FailureStage::Prepare, _) => Some("while the project was compiled, before anything ran"),
        (FailureStage::Run, Some(s)) if s.execute_ms.is_some() => {
            Some("during execution (compile succeeded)")
        }
        (FailureStage::Run, Some(s)) if s.compile_ms.is_some() => Some("while compiling"),
        _ => None,
    };
    let took = facts.stats.and_then(NodeRunStats::took_ms);
    let stats_line = match (took, phase) {
        (Some(ms), Some(phase)) => Some(format!("It failed after {}, {phase}.", duration(ms))),
        (Some(ms), None) => Some(format!("It failed after {}.", duration(ms))),
        (None, Some(phase)) => Some(format!("It failed {phase}.")),
        (None, None) => None,
    };
    if let Some(line) = stats_line {
        builder = builder.evidence(EvidenceItem::context(
            EvidenceSource::RunStats,
            Text::new().plain(&line),
        ));
    }
    // Its own code: changed, or not, since its last good build.
    if let (Some(entry), Some(last)) = (facts.entry(node), facts.last_good(node)) {
        let text = match changed_in_run(facts, node) {
            Some(components) if !components.is_empty() => Text::new()
                .plain(&format!(
                    "Its code changed since its last successful build ({}), in run ",
                    components.join(", ")
                ))
                .code(short_run(&last.run_id))
                .plain("."),
            Some(_) => Text::new()
                .plain("Its code changed since its last successful build, in run ")
                .code(short_run(&last.run_id))
                .plain("."),
            None if entry.before.is_some() && entry.before == entry.after => Text::new()
                .plain("Its code didn't change since its last successful build, in run ")
                .code(short_run(&last.run_id))
                .plain("."),
            None => Text::new(),
        };
        if !text.is_empty() {
            builder = builder.evidence(EvidenceItem::context(EvidenceSource::Fingerprint, text));
        }
    }
    builder = source_versions(facts, builder);
    history(facts, builder)
}

/// Upstream data that is new since the node's last good build, with its versions.
fn source_versions(
    facts: &FailureFacts<'_>,
    mut builder: ExplanationBuilder,
) -> ExplanationBuilder {
    let (Some(entry), Some(last)) = (facts.entry(facts.node), facts.last_good(facts.node)) else {
        return builder;
    };
    for evidence in entry
        .evidence
        .iter()
        .filter(|e| e.kind == "source_data_version")
    {
        let before = last.inputs.get(&evidence.subject).and_then(Option::as_ref);
        if let (Some(before), Some(now)) = (before, &evidence.value)
            && before.value != *now
        {
            let text = Text::new()
                .plain("Upstream ")
                .code(&facts.name(&evidence.subject))
                .plain(" has new data since then");
            // Versions are shown only when they are plain (e.g. a table version); a
            // timestamp or anything else stays out.
            let text = if is_code(&before.value) && is_code(now) {
                text.plain(" (version ")
                    .code(&before.value)
                    .plain(" → ")
                    .code(now)
                    .plain(").")
            } else {
                text.plain(".")
            };
            builder = builder.evidence(EvidenceItem::context(EvidenceSource::SourceVersions, text));
        }
    }
    builder
}

/// How the node did in its earlier runs.
fn history(facts: &FailureFacts<'_>, builder: ExplanationBuilder) -> ExplanationBuilder {
    let earlier: Vec<(Option<&str>, &NodeRunStats)> = facts
        .history
        .iter()
        .filter_map(|run| {
            run.get(facts.node)
                .map(|n| (run.run_id.as_deref(), &n.stats))
        })
        .filter(|(_, s)| s.status.is_finished())
        .take(HISTORY_RUNS)
        .collect();
    if earlier.is_empty() {
        return builder;
    }
    let same = |s: &NodeRunStats| {
        s.status == NodeRunStatus::Error
            && match (&s.error, facts.error) {
                (Some(a), Some(b)) => a.kind() == b.kind() && a.message() == b.message(),
                _ => false,
            }
    };
    let n = earlier.len();
    let last = if n == 1 {
        "its last run".to_owned()
    } else {
        format!("its last {n} runs")
    };
    let repeated = earlier.iter().filter(|(_, s)| same(s)).count();
    let text = if repeated > 0 {
        format!("It failed the same way in {repeated} of {last}.")
    } else if earlier
        .iter()
        .all(|(_, s)| s.status == NodeRunStatus::Success)
    {
        // One run that built it is its last good build, said already.
        let said = n == 1
            && facts
                .last_good(facts.node)
                .is_some_and(|l| earlier[0].0 == Some(l.run_id.as_str()));
        if said {
            return builder;
        }
        format!("It built fine in {last}.")
    } else {
        return builder;
    };
    builder.evidence(EvidenceItem::context(
        EvidenceSource::RunHistory,
        Text::new().plain(&text),
    ))
}

fn where_it_is(facts: &FailureFacts<'_>, builder: ExplanationBuilder) -> ExplanationBuilder {
    let indexed = facts.indexed(facts.node);
    // A source line only where ODS knows it: the call of an undefined macro.
    let line = match facts.classification {
        Classification::Recognised(m) if m.symptom == Symptom::UnknownMacro => {
            undefined_call(facts).and_then(|c| c.line)
        }
        _ => None,
    };
    let location = indexed
        .and_then(|n| n.file.as_deref())
        .map_or_else(Location::default, Location::in_file)
        .at_line(line)
        .compiled(
            indexed.and_then(|n| n.compiled_file.as_deref()),
            facts.error.and_then(ErrorSummary::line),
        );
    builder.location(location)
}

fn next_steps(facts: &FailureFacts<'_>, mut builder: ExplanationBuilder) -> ExplanationBuilder {
    let recognised = builder.is_recognised();
    if !recognised {
        builder = builder.suggest(Suggestion::new(Text::new().plain(&format!(
            "Read {}'s message below and the full log.",
            facts.catalogue.engine
        ))));
    }
    let transient = matches!(
        facts.classification,
        Classification::Recognised(m) if matches!(m.symptom, Symptom::QueryTimeout | Symptom::LockConflict | Symptom::WarehouseUnavailable)
    );
    match facts.retry {
        Some(retry) if !recognised || transient => builder
            .suggest(retry.suggest("If it may be transient (timeouts, locks), retry what failed:")),
        Some(retry) => builder.suggest(retry.suggest("Then retry what failed:")),
        None if facts.stage == FailureStage::Prepare => builder.suggest(Suggestion::new(
            Text::new().plain("Then run the same command again: nothing was built or recorded."),
        )),
        None => builder,
    }
}

fn impact(facts: &FailureFacts<'_>, builder: ExplanationBuilder) -> ExplanationBuilder {
    let Some(run) = facts.run else {
        return builder;
    };
    // What the engine said it blocked; and a skipped node it said nothing about, when
    // the plan puts it downstream of this one (an engine skips what reads a failure).
    let downstream = downstream_in_plan(facts);
    let blocked: Vec<String> = run
        .nodes
        .iter()
        .filter(|n| {
            n.stats.blocked_by.iter().any(|b| b == facts.node)
                || (n.stats.blocked_by.is_empty()
                    && n.stats.status == NodeRunStatus::Skipped
                    && downstream.contains(n.node.as_str()))
        })
        .map(|n| n.node.clone())
        .collect();
    let kept = blocked
        .iter()
        .filter(|n| facts.last_good(n).is_some())
        .cloned()
        .collect();
    builder.impact(blocked, kept)
}

/// A plan rebuilt from state, for a run whose plan wasn't kept (e.g. a past run shown
/// from its journal): every node the run finished, with its `parents` (as the project
/// has them now), built for a change of its own code when the state the run committed
/// has another fingerprint than the state before it, with the components that differ.
/// Nothing else is claimed: no reasons beyond what the two states show.
pub fn plan_from_states(
    run: &RunSummary,
    before: Option<&StateSnapshot>,
    after: Option<&StateSnapshot>,
    parents: &dyn Fn(&str) -> Vec<String>,
) -> ExecutionPlan {
    let entries = run
        .nodes
        .iter()
        .map(|n| {
            let was = before.and_then(|s| s.nodes.get(&n.node));
            let now = after
                .and_then(|s| s.nodes.get(&n.node))
                .filter(|state| run.run_id.as_deref() == Some(state.run_id.as_str()));
            let name = n.node.rsplit('.').next().unwrap_or(&n.node).to_owned();
            let mut entry = PlanEntry::new(
                n.node.clone(),
                name,
                "node",
                PlanAction::Build,
                Vec::new(),
                FreshnessPolicy::conservative(),
                0,
            );
            entry.depends_on = parents(&n.node);
            if let (Some(was), Some(now)) = (was, now)
                && was.fingerprint.digest != now.fingerprint.digest
            {
                let (a, b) = (&was.fingerprint.components, &now.fingerprint.components);
                let mut changed: Vec<String> = b
                    .iter()
                    .filter(|(k, v)| a.get(*k) != Some(*v))
                    .map(|(k, _)| k.clone())
                    .chain(a.keys().filter(|k| !b.contains_key(*k)).cloned())
                    .collect();
                changed.sort();
                entry.reasons = vec![Reason::new(ReasonCode::CodeChanged, "code changed")];
                entry.changed_components = changed;
                entry.before = Some(was.fingerprint.digest.clone());
                entry.after = Some(now.fingerprint.digest.clone());
            }
            entry
        })
        .collect();
    ExecutionPlan::new(None, Timestamp::from_unix(0), entries)
}

/// Every node the plan puts downstream of the failed node.
fn downstream_in_plan<'a>(facts: &FailureFacts<'a>) -> std::collections::BTreeSet<&'a str> {
    let mut found = std::collections::BTreeSet::new();
    let Some(plan) = facts.plan else {
        return found;
    };
    // A check blocks what is downstream of the nodes it checks.
    let mut frontier = match facts.check {
        Some(check) => check.covers.iter().map(String::as_str).collect(),
        None => vec![facts.node],
    };
    while let Some(node) = frontier.pop() {
        for entry in &plan.entries {
            if entry.depends_on.iter().any(|d| d == node) && found.insert(entry.node.as_str()) {
                frontier.push(entry.node.as_str());
            }
        }
    }
    found
}

/// A duration, for people: `850ms`, `4.2s`, `2m 05s`.
fn duration(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{ms}ms"),
        1_000..60_000 => {
            let tenths = (ms + 50) / 100;
            format!("{}.{}s", tenths / 10, tenths % 10)
        }
        _ => {
            let seconds = (ms + 500) / 1_000;
            format!("{}m {:02}s", seconds / 60, seconds % 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ods_core::FreshnessPolicy;
    use ods_core::failure::{Confidence, ErrorCategory};
    use ods_core::state::{
        DataVersion, Evidence, Exactness, Fingerprint, Reason, Timestamp, TimestampMs,
    };
    use ods_provider_fake::FakeErrorCatalogue;
    use ods_sdk::contracts::error_catalogue::ErrorCatalogue;
    use ods_sdk::contracts::executor::ExecutionMode;
    use ods_sdk::contracts::run_events::{RunEvent, RunEventKind, RunOutcome};

    use super::*;
    use ods_sdk::contracts::error_catalogue::{CheckTarget, PatternMatch};

    const CUSTOMERS: &str = "model.shop.customers";
    const STG: &str = "model.shop.stg_customers";
    const SEGMENTS: &str = "model.shop.customer_segments";
    const SENTINEL: &str = "sk_live_SENTINEL_42";

    fn catalogue() -> FakeErrorCatalogue {
        FakeErrorCatalogue::new()
    }

    fn summary(message: &str) -> ErrorSummary {
        ErrorSummary::from_message(message)
            .unwrap()
            .with_line(Some(25))
    }

    fn failed(error: &ErrorSummary) -> NodeRunStats {
        NodeRunStats::new(NodeRunStatus::Error)
            .with_durations(Some(134_000), Some(20), Some(133_980))
            .with_error(Some(error.clone()))
    }

    fn run(nodes: &[(&str, NodeRunStats)]) -> RunSummary {
        let at = TimestampMs::from_unix_millis(1_000);
        let mut events = vec![RunEvent::new(
            "run-2",
            None,
            at,
            RunEventKind::RunStarted {
                nodes: nodes.iter().map(|(n, _)| (*n).to_owned()).collect(),
                mode: ExecutionMode::Build,
                live: true,
            },
        )];
        for (node, stats) in nodes {
            events.push(RunEvent::new(
                "run-2",
                None,
                at,
                RunEventKind::NodeFinished {
                    node: (*node).to_owned(),
                    stats: stats.clone(),
                },
            ));
        }
        events.push(RunEvent::new(
            "run-2",
            None,
            at,
            RunEventKind::RunFinished {
                outcome: RunOutcome::Failed,
            },
        ));
        RunSummary::from_events(&events)
    }

    fn state(run_id: &str, inputs: BTreeMap<String, Option<DataVersion>>) -> NodeState {
        NodeState::new(
            Fingerprint::from_content([("sql", "x")]),
            Timestamp::from_unix(0),
            run_id,
            inputs,
        )
    }

    fn before() -> StateSnapshot {
        StateSnapshot::new(
            None,
            Timestamp::from_unix(0),
            "b6802661-aaaa",
            BTreeMap::from([
                (
                    CUSTOMERS.to_owned(),
                    state("b6802661-aaaa", BTreeMap::new()),
                ),
                (STG.to_owned(), state("b6802661-aaaa", BTreeMap::new())),
                (SEGMENTS.to_owned(), state("b6802661-aaaa", BTreeMap::new())),
            ]),
        )
    }

    fn plan() -> ExecutionPlan {
        let mut stg = PlanEntry::new(
            STG,
            "stg_customers",
            "model",
            PlanAction::Build,
            vec![Reason::new(ReasonCode::CodeChanged, "code changed")],
            FreshnessPolicy::conservative(),
            0,
        );
        stg.changed_components = vec!["sql".into()];
        let mut customers = PlanEntry::new(
            CUSTOMERS,
            "customers",
            "model",
            PlanAction::Build,
            vec![Reason::new(ReasonCode::UpstreamCodeChanged, "upstream")],
            FreshnessPolicy::conservative(),
            1,
        );
        customers.depends_on = vec![STG.into()];
        ExecutionPlan::new(None, Timestamp::from_unix(0), vec![stg, customers])
    }

    fn json(e: &ErrorExplanation) -> String {
        serde_json::to_string(e).unwrap()
    }

    #[test]
    fn a_missing_column_with_lineage_is_confirmed_and_names_the_rename() {
        let catalogue = catalogue();
        let error = summary(&format!(
            "Query Error: no such column 'first_name' near '{SENTINEL}'"
        ));
        let classification = catalogue.classify(&error);
        let stats = failed(&error);
        let run = run(&[
            (STG, NodeRunStats::new(NodeRunStatus::Success)),
            (CUSTOMERS, stats.clone()),
            (
                SEGMENTS,
                NodeRunStats::new(NodeRunStatus::Skipped).with_blocked_by(vec![CUSTOMERS.into()]),
            ),
        ]);
        let (plan, before) = (plan(), before());
        let missing = [MissingColumn::new(
            STG,
            "first_name",
            vec!["given_name".into()],
        )];
        let info = catalogue.catalogue();
        let mut facts = FailureFacts::new(CUSTOMERS, &classification, &info, FailureStage::Run);
        facts.error = Some(&error);
        facts.stats = Some(&stats);
        facts.run = Some(&run);
        facts.plan = Some(&plan);
        facts.before = Some(&before);
        facts.missing_columns = &missing;
        facts.retry = Some(Retry::default());
        let e = explain_failure(&facts);
        assert_eq!(e.confidence(), Confidence::KnownPatternWithEvidence);
        assert_eq!(e.chip(), "database error · missing column");
        assert_eq!(
            e.headline().as_str(),
            "A column this model reads no longer exists upstream"
        );
        let why: Vec<&str> = e.evidence().iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            why,
            vec![
                "Column lineage: `stg_customers` now outputs `given_name`, not `first_name`, from the same input column.",
                "`stg_customers` changed in this run (sql) and was rebuilt first.",
                "`customers` last built fine in run `b6802661`, before that change.",
                "It failed after 2m 14s, during execution (compile succeeded).",
            ]
        );
        let commands: Vec<&str> = e
            .suggestions()
            .iter()
            .flat_map(|s| s.commands.iter().map(String::as_str))
            .collect();
        assert_eq!(
            commands,
            vec![
                "ods lineage impact --column model.shop.stg_customers.first_name=removed",
                "ods state retry --failed"
            ]
        );
        assert_eq!(
            e.suggestions()[0].text.as_str(),
            "Use `given_name` in `customers`, or restore `first_name` in `stg_customers`."
        );
        let impact = e.impact().unwrap();
        assert_eq!(impact.blocked, vec![SEGMENTS]);
        assert_eq!(impact.kept, vec![SEGMENTS]);
        let location = e.location().unwrap();
        assert_eq!((location.line, location.reported_line), (None, Some(25)));
        assert!(!json(&e).contains(SENTINEL), "{}", json(&e));
        assert!(!json(&e).contains("first_name'"));
    }

    #[test]
    fn a_missing_column_without_lineage_is_a_known_pattern_only() {
        let catalogue = catalogue();
        let error = summary("Query Error: no such column here");
        let classification = catalogue.classify(&error);
        let info = catalogue.catalogue();
        let mut facts = FailureFacts::new(CUSTOMERS, &classification, &info, FailureStage::Run);
        facts.error = Some(&error);
        let e = explain_failure(&facts);
        assert_eq!(e.confidence(), Confidence::KnownPattern);
        assert_eq!(
            e.headline().as_str(),
            "A column this model reads doesn't exist"
        );
        assert!(e.evidence().iter().all(|i| !i.confirms));
        let message = e.engine_message().unwrap();
        assert_eq!(message.engine, "the fake engine");
        assert_eq!(message.kind.as_deref(), Some("Query Error"));
    }

    #[test]
    fn an_unknown_macro_is_confirmed_by_the_manifest_with_a_did_you_mean() {
        let catalogue = catalogue();
        let error = summary("no such macro [value removed]");
        let classification = catalogue.classify(&error);
        let info = catalogue.catalogue();
        let index = ProjectIndex::new(["cents_to_dollars", "dollars_to_cents", "other"]).with_node(
            "model.shop.stg_payments",
            IndexedNode::new("stg_payments")
                .in_file(Some("models/staging/stg_payments.sql"), None)
                .calling_undefined(vec![NameAt::new("cent_to_dollars", Some(5))]),
        );
        let mut facts = FailureFacts::new(
            "model.shop.stg_payments",
            &classification,
            &info,
            FailureStage::Prepare,
        );
        facts.error = Some(&error);
        facts.index = Some(&index);
        let e = explain_failure(&facts);
        assert_eq!(e.confidence(), Confidence::KnownPatternWithEvidence);
        assert_eq!(e.category(), ErrorCategory::Compilation);
        assert_eq!(
            e.headline().as_str(),
            "The macro `cent_to_dollars` isn't defined"
        );
        assert_eq!(
            e.evidence()[0].text.as_str(),
            "`cent_to_dollars` isn't among the project's macros."
        );
        let texts: Vec<&str> = e.suggestions().iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Check the name: a macro called `cents_to_dollars` exists.",
                "Install the fake packages.",
                "Then run the same command again: nothing was built or recorded.",
            ]
        );
        let location = e.location().unwrap();
        assert_eq!(
            (location.file.as_deref(), location.line),
            (Some("models/staging/stg_payments.sql"), Some(5))
        );
        assert!(
            e.evidence().iter().any(|i| i.text.as_str()
                == "It failed while the project was compiled, before anything ran.")
        );
    }

    #[test]
    fn a_ref_to_a_missing_node_is_a_known_pattern() {
        let catalogue = catalogue();
        let error = summary("no such node called [value removed]");
        let classification = catalogue.classify(&error);
        let info = catalogue.catalogue();
        let facts = FailureFacts::new(CUSTOMERS, &classification, &info, FailureStage::Prepare);
        let e = explain_failure(&facts);
        assert_eq!(e.confidence(), Confidence::KnownPattern);
        assert_eq!(e.chip(), "dependency or ref · missing ref");
    }

    #[test]
    fn an_unrecognised_error_gets_facts_but_never_a_cause() {
        let catalogue = catalogue();
        let error = summary(&format!("Query Error: operation failed: '{SENTINEL}'"));
        let classification = catalogue.classify(&error);
        let info = catalogue.catalogue();
        let stats = failed(&error);
        let other = summary("Query Error: something else");
        let history = vec![
            run(&[(CUSTOMERS, failed(&error))]),
            run(&[(CUSTOMERS, NodeRunStats::new(NodeRunStatus::Success))]),
            run(&[(CUSTOMERS, failed(&other))]),
            run(&[(CUSTOMERS, failed(&error))]),
            run(&[(STG, NodeRunStats::new(NodeRunStatus::Success))]),
            run(&[(CUSTOMERS, NodeRunStatus::Success).into_stats()]),
            run(&[(CUSTOMERS, NodeRunStats::new(NodeRunStatus::Success))]),
        ];
        // Unchanged code, and new data in its source since its last good build.
        let mut entry = PlanEntry::new(
            CUSTOMERS,
            "customers",
            "model",
            PlanAction::Build,
            vec![Reason::new(
                ReasonCode::NewUpstreamData,
                "new upstream data",
            )],
            FreshnessPolicy::conservative(),
            1,
        );
        entry.before = Some("d1".into());
        entry.after = Some("d1".into());
        entry.evidence.push(Evidence::new(
            "source_data_version",
            "source.shop.raw.payments",
            Some("830".into()),
            Exactness::Exact,
        ));
        let plan = ExecutionPlan::new(None, Timestamp::from_unix(0), vec![entry]);
        let before = StateSnapshot::new(
            None,
            Timestamp::from_unix(0),
            "51aa02e7-bbbb",
            BTreeMap::from([(
                CUSTOMERS.to_owned(),
                state(
                    "51aa02e7-bbbb",
                    BTreeMap::from([(
                        "source.shop.raw.payments".to_owned(),
                        Some(DataVersion::new("812", Exactness::Exact, "table version")),
                    )]),
                ),
            )]),
        );
        let mut facts = FailureFacts::new(CUSTOMERS, &classification, &info, FailureStage::Run);
        facts.error = Some(&error);
        facts.stats = Some(&stats);
        facts.history = &history;
        facts.plan = Some(&plan);
        facts.before = Some(&before);
        facts.retry = Some(Retry::default());
        let index = ProjectIndex::new(Vec::<String>::new())
            .with_node("source.shop.raw.payments", IndexedNode::new("raw.payments"));
        facts.index = Some(&index);
        // Lineage evidence can't turn an unrecognised error into a cause.
        let missing = [MissingColumn::new(STG, "first_name", Vec::new())];
        facts.missing_columns = &missing;
        let e = explain_failure(&facts);
        assert_eq!(e.confidence(), Confidence::NotRecognised);
        assert_eq!(e.symptom(), None);
        assert_eq!(e.pattern(), None);
        assert_eq!(e.chip(), "database error");
        assert_eq!(e.headline().as_str(), "The warehouse rejected the query");
        let why: Vec<&str> = e.evidence().iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            why,
            vec![
                "It failed after 2m 14s, during execution (compile succeeded).",
                "Its code didn't change since its last successful build, in run `51aa02e7`.",
                "Upstream `raw.payments` has new data since then (version `812` → `830`).",
                "It failed the same way in 2 of its last 5 runs.",
            ]
        );
        assert!(e.evidence().iter().all(|i| !i.confirms));
        let texts: Vec<&str> = e.suggestions().iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Read the fake engine's message below and the full log.",
                "If it may be transient (timeouts, locks), retry what failed:",
            ]
        );
        assert!(!json(&e).contains(SENTINEL));
        assert!(!format!("{e:?}").contains(SENTINEL));
    }

    trait IntoStats {
        fn into_stats(self) -> (&'static str, NodeRunStats);
    }

    impl IntoStats for (&'static str, NodeRunStatus) {
        fn into_stats(self) -> (&'static str, NodeRunStats) {
            (self.0, NodeRunStats::new(self.1))
        }
    }

    #[test]
    fn a_node_that_always_built_says_so() {
        let catalogue = catalogue();
        let error = summary("Query Error: took too long");
        let classification = catalogue.classify(&error);
        let info = catalogue.catalogue();
        let history = vec![run(&[(
            CUSTOMERS,
            NodeRunStats::new(NodeRunStatus::Success),
        )])];
        let mut facts = FailureFacts::new(CUSTOMERS, &classification, &info, FailureStage::Run);
        facts.error = Some(&error);
        facts.history = &history;
        facts.retry = Some(Retry::default());
        let e = explain_failure(&facts);
        assert_eq!(e.confidence(), Confidence::KnownPattern);
        assert_eq!(
            e.evidence()[0].text.as_str(),
            "It built fine in its last run."
        );
        assert_eq!(
            e.suggestions()[0].text.as_str(),
            "If it may be transient (timeouts, locks), retry what failed:"
        );
    }

    #[test]
    fn a_missing_relation_is_confirmed_by_a_parent_never_built() {
        let catalogue = catalogue();
        let error = summary("no such table here");
        let classification = catalogue.classify(&error);
        let info = catalogue.catalogue();
        let run = run(&[
            (STG, NodeRunStats::new(NodeRunStatus::Error)),
            (CUSTOMERS, NodeRunStats::new(NodeRunStatus::Error)),
        ]);
        let plan = plan();
        let mut facts = FailureFacts::new(CUSTOMERS, &classification, &info, FailureStage::Run);
        facts.run = Some(&run);
        facts.plan = Some(&plan);
        let e = explain_failure(&facts);
        assert_eq!(e.confidence(), Confidence::KnownPatternWithEvidence);
        assert_eq!(
            e.evidence()[0].text.as_str(),
            "Upstream `stg_customers` has never been built, and didn't build in this run."
        );
    }

    /// #323 review (H1): several missing columns, or one whose upstream wasn't built
    /// from the code lineage read, are context, all listed, never a confirmed cause.
    #[test]
    fn missing_columns_confirm_only_one_current_candidate() {
        let catalogue = catalogue();
        let error = summary("Query Error: no such column here");
        let classification = catalogue.classify(&error);
        let info = catalogue.catalogue();
        let two = [
            MissingColumn::new(STG, "discount", Vec::new()),
            MissingColumn::new(STG, "fooo", Vec::new()),
        ];
        let run = run(&[(STG, NodeRunStats::new(NodeRunStatus::Success))]);
        let mut facts = FailureFacts::new(CUSTOMERS, &classification, &info, FailureStage::Run);
        facts.error = Some(&error);
        facts.run = Some(&run);
        facts.missing_columns = &two;
        let e = explain_failure(&facts);
        assert_eq!(e.confidence(), Confidence::KnownPattern);
        assert_eq!(
            e.evidence()[0].text.as_str(),
            "Column lineage: `stg_customers` doesn't output `discount`; `stg_customers` doesn't output `fooo`."
        );
        assert!(matches!(
            &e.evidence()[0].data,
            Some(EvidenceData::MissingColumns { columns }) if columns.len() == 2
        ));
        assert_eq!(
            e.suggestions()
                .iter()
                .filter(|s| !s.commands.is_empty())
                .count(),
            2
        );

        // One candidate, but its upstream neither ran nor matches its committed build.
        let one = [MissingColumn::new(STG, "discount", Vec::new())];
        let nothing_ran = super::tests::run(&[]);
        let mut facts = FailureFacts::new(CUSTOMERS, &classification, &info, FailureStage::Run);
        facts.run = Some(&nothing_ran);
        facts.missing_columns = &one;
        assert_eq!(
            explain_failure(&facts).confidence(),
            Confidence::KnownPattern
        );

        // (H2) An older run, explained with the project as it is now: context only.
        let mut facts = FailureFacts::new(CUSTOMERS, &classification, &info, FailureStage::Run);
        facts.run = Some(&run);
        facts.missing_columns = &one;
        facts.project_is_run = false;
        let e = explain_failure(&facts);
        assert_eq!(e.confidence(), Confidence::KnownPattern);
        assert!(
            e.evidence()[0]
                .text
                .as_str()
                .starts_with("Column lineage, as the project is now: ")
        );
    }

    /// #323 review (B2): with two undefined names, which failed isn't known.
    #[test]
    fn several_undefined_calls_are_listed_not_confirmed() {
        let catalogue = catalogue();
        let error = summary("no such macro [value removed]");
        let classification = catalogue.classify(&error);
        let info = catalogue.catalogue();
        let index = ProjectIndex::new(["cents_to_dollars"]).with_node(
            CUSTOMERS,
            IndexedNode::new("customers").calling_undefined(vec![
                NameAt::new("cent_to_dollars", Some(5)),
                NameAt::new("other_one", Some(7)),
            ]),
        );
        let mut facts = FailureFacts::new(CUSTOMERS, &classification, &info, FailureStage::Run);
        facts.index = Some(&index);
        let e = explain_failure(&facts);
        assert_eq!(e.confidence(), Confidence::KnownPattern);
        assert_eq!(
            e.evidence()[0].text.as_str(),
            "The model calls names the project doesn't define as macros: `cent_to_dollars`, `other_one`."
        );
        assert_eq!(
            e.location().and_then(|l| l.line),
            None,
            "no line for a guess"
        );
        assert!(
            e.suggestions()
                .iter()
                .all(|s| !s.text.as_str().contains("Check the name"))
        );
    }

    /// #323 review (M2, M1): "the Python model" only for a Python node; token-shaped
    /// values from the engine never reach an explanation's own text.
    #[test]
    fn python_wording_follows_the_index_and_tokens_stay_out() {
        let classification = Classification::Recognised(
            ods_sdk::contracts::error_catalogue::PatternMatch::new("p", Symptom::PythonException)
                .about(Some("KeyError".into())),
        );
        let info = catalogue().catalogue();
        let error = summary("KeyError: ghp_SENTINEL123 db://u:SENTINEL@h/db token=SENTINEL");
        let sql = ProjectIndex::new(Vec::<String>::new()).with_node(
            CUSTOMERS,
            IndexedNode::new("customers").in_language(Some("sql")),
        );
        let mut facts = FailureFacts::new(CUSTOMERS, &classification, &info, FailureStage::Run);
        facts.error = Some(&error);
        facts.index = Some(&sql);
        let e = explain_failure(&facts);
        assert_eq!(
            e.headline().as_str(),
            "It raised the Python exception `KeyError`"
        );
        let mut json = serde_json::to_value(&e).unwrap();
        json.as_object_mut().unwrap().remove("engine_message");
        assert!(!json.to_string().contains("SENTINEL"), "{json}");
        let python = ProjectIndex::new(Vec::<String>::new()).with_node(
            CUSTOMERS,
            IndexedNode::new("customers").in_language(Some("python")),
        );
        facts.index = Some(&python);
        assert_eq!(
            explain_failure(&facts).headline().as_str(),
            "The Python model raised `KeyError`"
        );
    }

    #[test]
    fn a_plan_rebuilt_from_states_says_only_what_they_show() {
        let run = run(&[
            (STG, NodeRunStats::new(NodeRunStatus::Success)),
            (CUSTOMERS, NodeRunStats::new(NodeRunStatus::Error)),
        ]);
        let before = before();
        let mut after = before.clone();
        after.run_id = "run-2".into();
        after.nodes.insert(
            STG.into(),
            NodeState::new(
                Fingerprint::from_content([("sql", "y")]),
                Timestamp::from_unix(1),
                "run-2",
                BTreeMap::new(),
            ),
        );
        let parents = |id: &str| {
            if id == CUSTOMERS {
                vec![STG.to_owned()]
            } else {
                Vec::new()
            }
        };
        let plan = plan_from_states(&run, Some(&before), Some(&after), &parents);
        let stg = plan.entries.iter().find(|e| e.node == STG).unwrap();
        assert_eq!(stg.changed_components, ["sql"]);
        assert_eq!(stg.reasons[0].code, ReasonCode::CodeChanged);
        let customers = plan.entries.iter().find(|e| e.node == CUSTOMERS).unwrap();
        assert!(
            customers.reasons.is_empty(),
            "it built nothing: nothing to say"
        );
        assert_eq!(customers.depends_on, [STG]);
    }

    #[test]
    fn did_you_mean_is_close_and_ordered() {
        let names: Vec<String> = [
            "cents_to_dollars",
            "cents_to_dollar",
            "x",
            "CENT_TO_DOLLARS",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(
            similar("cent_to_dollars", &names),
            vec!["CENT_TO_DOLLARS", "cents_to_dollars", "cents_to_dollar"]
        );
        assert!(
            similar("zzzzzz", &names).is_empty(),
            "{:?}",
            similar("zzzzzz", &names)
        );
    }

    /// #323: a reference to a missing node suggests the project's referable nodes with a
    /// close name, closest first and at most three, as a guess, never as evidence; the
    /// name the reference used is never shown.
    #[test]
    fn a_missing_ref_suggests_close_names_from_the_project() {
        let index = ProjectIndex::new(Vec::<String>::new())
            .with_node("model.shop.orders", IndexedNode::new("orders").referable())
            .with_node("model.shop.order", IndexedNode::new("order").referable())
            .with_node("seed.shop.border", IndexedNode::new("border").referable())
            .with_node(
                "snapshot.shop.ordered",
                IndexedNode::new("ordered").referable(),
            )
            .with_node(
                "model.shop.orders_",
                IndexedNode::new("orders_").referable(),
            )
            // The same name in a package counts once; what can't be referred to never.
            .with_node("model.pkg.orders", IndexedNode::new("orders").referable())
            .with_node("source.shop.raw.orderx", IndexedNode::new("orderx"))
            .with_node(
                "test.shop.orderz_test",
                IndexedNode::new("orderx").checking(CheckTarget::default()),
            )
            .with_node(
                "model.shop.customers",
                IndexedNode::new("customers").referable(),
            );
        let info = CatalogueInfo::new("fake", "1", "the fake engine");
        let error = summary("Compilation Error: depends on a node named [value removed]");
        let explain = |missing: Option<&str>, index: Option<&ProjectIndex>| {
            let classification = Classification::Recognised(
                PatternMatch::new("fake-missing-ref", Symptom::MissingRef)
                    .about(missing.map(str::to_owned)),
            );
            let mut facts =
                FailureFacts::new("project", &classification, &info, FailureStage::Prepare);
            facts.error = Some(&error);
            facts.index = index;
            explain_failure(&facts)
        };
        let texts = |e: &ErrorExplanation| -> Vec<String> {
            e.suggestions()
                .iter()
                .map(|s| s.text.as_str().to_owned())
                .collect()
        };
        const GENERIC: &str = "Check the names the model refers to against the project's nodes.";

        // One close name.
        let e = explain(Some("custmers"), Some(&index));
        assert_eq!(
            texts(&e)[..2],
            [
                "Did you mean `customers`? The project has a node with a name close to the missing one; that is a guess from the names, not evidence of a typo.",
                GENERIC
            ]
        );
        assert_eq!(
            e.confidence(),
            Confidence::KnownPattern,
            "a guess confirms nothing"
        );
        assert!(e.evidence().iter().all(|i| !i.confirms));
        assert!(!json(&e).contains("custmers"), "{}", json(&e));

        // Several: closest first, then by name; at most three; deterministic.
        let several = explain(Some("orderz"), Some(&index));
        assert_eq!(
            texts(&several)[0],
            "Did you mean `order`, `orders` or `border`? The project has nodes with names close to the missing one; that is a guess from the names, not evidence of a typo."
        );
        assert_eq!(json(&several), json(&explain(Some("orderz"), Some(&index))));
        let two = explain(Some("ordrs"), Some(&index));
        assert!(
            texts(&two)[0].starts_with("Did you mean `orders`, `order` or `orders_`? "),
            "{:?}",
            texts(&two)
        );

        // None close, no name, or no project: only the generic step.
        for e in [
            explain(Some("zzzzzzzz"), Some(&index)),
            explain(None, Some(&index)),
            explain(Some("custmers"), None),
        ] {
            assert_eq!(texts(&e)[0], GENERIC);
            assert!(texts(&e).iter().all(|t| !t.contains("Did you mean")));
            assert_eq!(e.confidence(), Confidence::KnownPattern);
        }
    }

    /// #323, #181: health checks are evidence from `ods doctor`; one confirms only when
    /// it shows the symptom the pattern recognised, and nothing secret gets through.
    #[test]
    fn doctor_findings_are_evidence_and_confirm_only_the_symptom_they_show() {
        let info = CatalogueInfo::new("fake", "1", "the fake engine");
        let error = summary(&format!(
            "Runtime Error: Could not find profile named '{SENTINEL}'"
        ));
        let profile = Classification::Recognised(PatternMatch::new(
            "fake-profile-not-found",
            Symptom::ProfileNotFound,
        ));
        let unknown = Classification::NotRecognised {
            category: ErrorCategory::Configuration,
        };
        let resolution = DoctorFinding::new(
            "config.resolution",
            HealthStatus::Ok,
            Text::new()
                .plain("dbt's profile is ")
                .code("analytics")
                // A value that could carry a secret never reads as a name.
                .plain(", password ")
                .code(&format!("password={SENTINEL}"))
                .plain("."),
        );
        let missing = DoctorFinding::new(
            "config.resolution",
            HealthStatus::Warning,
            Text::new()
                .plain("no profiles file in ")
                .code("profiles")
                .plain("."),
        )
        .showing(Symptom::ProfileNotFound);
        let elsewhere = DoctorFinding::new(
            "config.values",
            HealthStatus::Warning,
            Text::new().plain("something else."),
        )
        .showing(Symptom::CredentialsMissing);
        let explain = |classification: &Classification, doctor: &[DoctorFinding]| {
            let mut facts =
                FailureFacts::new("project", classification, &info, FailureStage::Prepare);
            facts.error = Some(&error);
            facts.doctor = doctor;
            explain_failure(&facts)
        };

        // Without doctor findings, nothing changes.
        let plain = explain(&profile, &[]);
        assert_eq!(plain.confidence(), Confidence::KnownPattern);
        assert!(
            plain
                .evidence()
                .iter()
                .all(|i| i.source != EvidenceSource::Doctor)
        );

        // Findings that don't show the symptom are context.
        let context = explain(&profile, &[resolution.clone(), elsewhere.clone()]);
        assert_eq!(context.confidence(), Confidence::KnownPattern);
        let doctor: Vec<&EvidenceItem> = context
            .evidence()
            .iter()
            .filter(|i| i.source == EvidenceSource::Doctor)
            .collect();
        assert_eq!(doctor.len(), 2);
        assert_eq!(
            doctor[0].text.as_str(),
            "Check `config.resolution` (ok): dbt's profile is `analytics`, password [name hidden]."
        );
        assert_eq!(
            doctor[0].data,
            Some(EvidenceData::DoctorCheck {
                check: "config.resolution".into(),
                status: HealthStatus::Ok,
            })
        );
        assert!(doctor.iter().all(|i| !i.confirms));

        // One that shows it confirms.
        let confirmed = explain(&profile, &[resolution.clone(), missing.clone()]);
        assert_eq!(confirmed.confidence(), Confidence::KnownPatternWithEvidence);
        let confirming: Vec<&str> = confirmed
            .evidence()
            .iter()
            .filter(|i| i.confirms)
            .map(|i| i.text.as_str())
            .collect();
        assert_eq!(
            confirming,
            ["Check `config.resolution` (warning): no profiles file in `profiles`."]
        );

        // An error nothing recognised gets it as context only.
        let unrecognised = explain(&unknown, &[missing]);
        assert_eq!(unrecognised.confidence(), Confidence::NotRecognised);
        assert!(
            unrecognised
                .evidence()
                .iter()
                .any(|i| i.source == EvidenceSource::Doctor)
        );
        assert!(unrecognised.evidence().iter().all(|i| !i.confirms));

        for e in [plain, context, confirmed, unrecognised] {
            assert!(!json(&e).contains("SENTINEL"), "{}", json(&e));
        }
    }

    const TEST: &str = "test.shop.not_null_orders_customer_id.ab12";
    const ORDERS: &str = "model.shop.orders";

    /// A run of `orders` and its test, which ended as `status`.
    fn tested(
        status: CheckStatus,
        failures: Option<u64>,
        error: Option<ErrorSummary>,
    ) -> RunSummary {
        tested_as(TEST, status, failures, error)
    }

    /// A run of `orders` and its test `check`, which ended as `status`.
    fn tested_as(
        check: &str,
        status: CheckStatus,
        failures: Option<u64>,
        error: Option<ErrorSummary>,
    ) -> RunSummary {
        let at = TimestampMs::from_unix_millis(1_000);
        let events = [
            RunEvent::new(
                "run-2",
                None,
                at,
                RunEventKind::RunStarted {
                    nodes: vec![ORDERS.to_owned()],
                    mode: ExecutionMode::Build,
                    live: true,
                },
            ),
            RunEvent::new(
                "run-2",
                None,
                at,
                RunEventKind::NodeFinished {
                    node: ORDERS.to_owned(),
                    stats: NodeRunStats::new(NodeRunStatus::Success),
                },
            ),
            RunEvent::new(
                "run-2",
                None,
                at,
                RunEventKind::CheckFinished {
                    check: check.to_owned(),
                    covers: vec![ORDERS.to_owned()],
                    status,
                    failures,
                    error,
                },
            ),
        ];
        RunSummary::from_events(&events)
    }

    fn test_index() -> ProjectIndex {
        ProjectIndex::new(Vec::<String>::new())
            .with_node(ORDERS, IndexedNode::new("orders"))
            .with_node(
                TEST,
                IndexedNode::new("not_null_orders_customer_id")
                    .in_file(Some("models/schema.yml"), None)
                    .checking(CheckTarget::new(
                        Some("not_null"),
                        Some("customer_id"),
                        Some(ORDERS),
                    )),
            )
    }

    /// Explains the run's failed check with the fake catalogue.
    fn explain_check(
        run: &RunSummary,
        index: Option<&ProjectIndex>,
        history: &[RunSummary],
    ) -> ErrorExplanation {
        let catalogue = catalogue();
        let check = failed_checks(run)[0];
        let classification = match &check.error {
            Some(error) => catalogue.classify(error),
            None => Classification::NotRecognised {
                category: ErrorCategory::Unknown,
            },
        };
        let info = catalogue.catalogue();
        let mut facts = FailureFacts::new(&check.check, &classification, &info, FailureStage::Run);
        facts.check = Some(check);
        facts.error = check.error.as_ref();
        facts.run = Some(run);
        facts.index = index;
        facts.history = history;
        explain_failure(&facts)
    }

    fn commands(e: &ErrorExplanation) -> Vec<&str> {
        e.suggestions()
            .iter()
            .flat_map(|s| s.commands.iter().map(String::as_str))
            .collect()
    }

    /// #323: a failed test with a count says which test, on which column of which
    /// model, and how many rows; the count confirms it, and the values in the engine's
    /// message never reach the explanation.
    #[test]
    fn a_failed_test_names_the_test_column_and_rows() {
        let error = summary(&format!("rows failed the test (5 like '{SENTINEL}')"));
        let run = tested(CheckStatus::Failed, Some(5), Some(error));
        let index = test_index();
        let e = explain_check(&run, Some(&index), &[]);
        assert_eq!(e.node(), check_handle(TEST));
        assert_eq!(e.confidence(), Confidence::KnownPatternWithEvidence);
        assert_eq!(
            e.pattern().map(|p| p.id.as_str()),
            Some("rows-failed-the-test")
        );
        assert_eq!(e.chip(), "test failure · test failed");
        assert_eq!(
            e.headline().as_str(),
            "The `not_null` test on `customer_id` of `orders` failed: 5 rows don't pass"
        );
        let check = e.check().unwrap();
        assert_eq!(check.covers, [ORDERS]);
        assert_eq!(
            (check.test.as_deref(), check.column.as_deref()),
            (Some("not_null"), Some("customer_id"))
        );
        let why: Vec<&str> = e.evidence().iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            why,
            [
                "the fake engine reported 5 rows that fail the test.",
                "The project declares it as a `not_null` test of column `customer_id` on `orders`.",
            ]
        );
        assert_eq!(
            e.evidence()[0].data,
            Some(EvidenceData::FailingRows { rows: 5 })
        );
        assert_eq!(
            commands(&e),
            [
                "ods state test --select orders -- --keep-failing-rows",
                "ods state test --select orders"
            ]
        );
        assert_eq!(
            e.location().and_then(|l| l.file.as_deref()),
            Some("models/schema.yml")
        );
        assert!(!json(&e).contains(SENTINEL), "{}", json(&e));
        assert!(e.engine_message().is_some());
    }

    /// #323: without a count the headline says so (never zero), and the catalogue's
    /// pattern alone is a known pattern; without the project, a test isn't named.
    #[test]
    fn a_failed_test_without_a_count_says_it_isnt_known() {
        let run = tested(
            CheckStatus::Failed,
            Some(0),
            Some(summary("rows failed the test")),
        );
        let index = test_index();
        let e = explain_check(&run, Some(&index), &[]);
        assert_eq!(e.confidence(), Confidence::KnownPattern);
        assert_eq!(
            e.headline().as_str(),
            "The `not_null` test on `customer_id` of `orders` failed; the fake engine didn't say how many rows don't pass"
        );
        assert!(!e.headline().as_str().contains(" 0 "));
        let bare = explain_check(&run, None, &[]);
        assert_eq!(
            bare.headline().as_str(),
            "A test on `orders` failed; the fake engine didn't say how many rows don't pass"
        );
        assert!(!bare.headline().as_str().contains("not_null_orders"));

        // A count alone, with no message ODS recognises: ODS's own pattern, confirmed.
        let counted = tested(CheckStatus::Failed, Some(1), None);
        let e = explain_check(&counted, Some(&index), &[]);
        assert_eq!(e.confidence(), Confidence::KnownPatternWithEvidence);
        assert_eq!(
            e.pattern().map(|p| p.id.as_str()),
            Some("check-failing-rows")
        );
        assert!(
            e.headline()
                .as_str()
                .ends_with("failed: 1 row doesn't pass")
        );

        // Nothing at all: a test failure, not recognised, with no guessed cause.
        let silent = tested(CheckStatus::Failed, None, None);
        let e = explain_check(&silent, Some(&index), &[]);
        assert_eq!(e.confidence(), Confidence::NotRecognised);
        assert_eq!(e.category(), ErrorCategory::TestFailure);
        assert_eq!(e.headline().as_str(), "A test failed");
        // Not recognised, so the provider's way to keep the rows isn't offered.
        assert_eq!(commands(&e), ["ods state test --select orders"]);
    }

    /// #323: a test that errored (here, a missing column in the test's query) is
    /// explained as that error, never as rows that failed.
    #[test]
    fn an_errored_test_is_explained_as_its_error() {
        let error = summary(&format!("Query Error: no such column '{SENTINEL}'"));
        let run = tested(CheckStatus::Failed, None, Some(error));
        let index = test_index();
        let e = explain_check(&run, Some(&index), &[]);
        assert_eq!(e.symptom(), Some(Symptom::MissingColumn));
        assert_eq!(e.category(), ErrorCategory::Database);
        assert!(!e.headline().as_str().contains("rows"), "{}", e.headline());
        assert_eq!(
            e.detail().map(Text::as_str),
            Some(
                "The `not_null` test on `customer_id` of `orders` couldn't run, so it says nothing about the data yet."
            )
        );
        assert!(
            e.evidence()
                .iter()
                .all(|i| i.data != Some(EvidenceData::FailingRows { rows: 0 }))
        );
        assert_eq!(commands(&e), ["ods state test --select orders"]);
        assert!(!json(&e).contains(SENTINEL), "{}", json(&e));
    }

    /// #323: a test that failed before says so; one that only warned isn't a failure;
    /// a node failed only by its tests (as in a test run) is explained by them.
    #[test]
    fn recurring_and_warned_tests() {
        let error = summary("rows failed the test");
        let failed = || tested(CheckStatus::Failed, Some(2), Some(error.clone()));
        let history = vec![failed(), tested(CheckStatus::Passed, None, None), failed()];
        let index = test_index();
        let e = explain_check(&failed(), Some(&index), &history);
        assert!(
            e.evidence()
                .iter()
                .any(|i| i.text.as_str() == "This test failed in 2 of its last 3 runs too."),
            "{:?}",
            e.evidence()
        );
        let passed = vec![tested(CheckStatus::Passed, None, None)];
        let e = explain_check(&failed(), Some(&index), &passed);
        assert!(
            e.evidence()
                .iter()
                .any(|i| i.text.as_str() == "This test passed in its last run.")
        );

        let warned = tested(CheckStatus::Warned, Some(2), Some(error.clone()));
        assert_eq!(failed_checks(&warned), Vec::<&CheckSummary>::new());

        // A test run: the node failed because its test did, with no error of its own.
        let at = TimestampMs::from_unix_millis(1_000);
        let events = [
            RunEvent::new(
                "run-3",
                None,
                at,
                RunEventKind::CheckFinished {
                    check: TEST.to_owned(),
                    covers: vec![ORDERS.to_owned()],
                    status: CheckStatus::Failed,
                    failures: Some(2),
                    error: None,
                },
            ),
            RunEvent::new(
                "run-3",
                None,
                at,
                RunEventKind::NodeFinished {
                    node: ORDERS.to_owned(),
                    stats: NodeRunStats::new(NodeRunStatus::Error),
                },
            ),
            RunEvent::new(
                "run-3",
                None,
                at,
                RunEventKind::NodeFinished {
                    node: CUSTOMERS.to_owned(),
                    stats: NodeRunStats::new(NodeRunStatus::Error),
                },
            ),
        ];
        let run = RunSummary::from_events(&events);
        assert_eq!(failed_nodes(&run), [CUSTOMERS]);
        assert_eq!(failed_checks(&run).len(), 1);
    }

    /// #323: a test's own name is used only for a test the provider says is singular
    /// (named by its file), never for a generic one, whose name may hold its arguments;
    /// nor is the check's id, which may too: the explanation carries its handle.
    #[test]
    fn only_a_singular_tests_name_is_shown() {
        let run = tested(CheckStatus::Failed, Some(3), None);
        let singular = ProjectIndex::new(Vec::<String>::new())
            .with_node(ORDERS, IndexedNode::new("orders"))
            .with_node(
                TEST,
                IndexedNode::new("only_placed_orders")
                    .checking(CheckTarget::new(None, None, Some(ORDERS)).singular()),
            );
        let e = explain_check(&run, Some(&singular), &[]);
        assert_eq!(
            e.headline().as_str(),
            "The test `only_placed_orders` on `orders` failed: 3 rows don't pass"
        );
        assert!(e.evidence().iter().any(|i| i.text.as_str()
            == "The project declares it as a test of its own (a singular test) on `orders`."));

        // dbt's id for an `accepted_values` test, as recorded: the values it accepts
        // (one a secret) are in its name and its id.
        let id =
            format!("test.shop.accepted_values_orders_status__completed__{SENTINEL}.efdbb4986a");
        let run = tested_as(&id, CheckStatus::Failed, Some(3), None);
        let named = |target: CheckTarget| {
            ProjectIndex::new(Vec::<String>::new())
                .with_node(ORDERS, IndexedNode::new("orders"))
                .with_node(
                    &id,
                    IndexedNode::new(format!(
                        "accepted_values_orders_status__completed__{SENTINEL}"
                    ))
                    .checking(target),
                )
        };
        let generic = named(CheckTarget::new(
            Some("accepted_values"),
            Some("status"),
            Some(ORDERS),
        ));
        let e = explain_check(&run, Some(&generic), &[]);
        assert!(!json(&e).contains(SENTINEL), "{}", json(&e));
        assert_eq!(e.node(), check_handle(&id));
        assert_eq!(
            e.headline().as_str(),
            "The `accepted_values` test on `status` of `orders` failed: 3 rows don't pass"
        );
        // A generic test whose kind isn't a name ODS shows: still not named by its name.
        let unnamed = named(CheckTarget::new(
            Some("odd kind!"),
            Some("status"),
            Some(ORDERS),
        ));
        let e = explain_check(&run, Some(&unnamed), &[]);
        assert!(!json(&e).contains(SENTINEL), "{}", json(&e));
        assert_eq!(
            e.headline().as_str(),
            "A test on `status` of `orders` failed: 3 rows don't pass"
        );
        // Without the project, too.
        let e = explain_check(&run, None, &[]);
        assert!(!json(&e).contains(SENTINEL), "{}", json(&e));
    }

    /// #323: for a test of a node this run didn't build, ODS says which build it last
    /// recorded, not which one the test read.
    #[test]
    fn a_test_of_a_node_not_built_says_what_ods_recorded() {
        let at = TimestampMs::from_unix_millis(1_000);
        let run = RunSummary::from_events(&[RunEvent::new(
            "run-3",
            None,
            at,
            RunEventKind::CheckFinished {
                check: TEST.to_owned(),
                covers: vec![CUSTOMERS.to_owned()],
                status: CheckStatus::Failed,
                failures: Some(2),
                error: None,
            },
        )]);
        let catalogue = catalogue();
        let classification = Classification::NotRecognised {
            category: ErrorCategory::Unknown,
        };
        let info = catalogue.catalogue();
        let before = before();
        let check = failed_checks(&run)[0];
        let mut facts = FailureFacts::new(TEST, &classification, &info, FailureStage::Run);
        facts.check = Some(check);
        facts.run = Some(&run);
        facts.before = Some(&before);
        let e = explain_failure(&facts);
        assert!(
            e.evidence().iter().any(|i| i.text.as_str()
                == "The last build ODS recorded for `customers` is from run `b6802661`."),
            "{:?}",
            e.evidence()
        );
    }
}
