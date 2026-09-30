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

use ods_core::FreshnessPolicy;
use ods_core::failure::{
    EngineMessage, ErrorExplanation, EvidenceData, EvidenceItem, EvidenceSource,
    ExplanationBuilder, Location, MissingColumn, PatternRef, Suggestion, Symptom, Text, is_code,
};
use ods_core::state::{
    ExecutionPlan, NodeState, PlanAction, PlanEntry, Reason, ReasonCode, StateSnapshot, Timestamp,
};
use ods_sdk::contracts::error_catalogue::{
    CatalogueInfo, Classification, IndexedNode, NameAt, ProjectIndex,
};
use ods_sdk::contracts::run_events::{ErrorSummary, NodeRunStats, NodeRunStatus, RunSummary};

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

/// Explains a failed node (see the module docs).
pub fn explain_failure(facts: &FailureFacts<'_>) -> ErrorExplanation {
    let mut builder = ExplanationBuilder::new(facts.node, facts.classification.category());
    if let Classification::Recognised(found) = facts.classification {
        builder = builder.recognised(
            PatternRef::new(
                facts.catalogue.name.clone(),
                facts.catalogue.version.clone(),
                found.id.clone(),
            ),
            found.symptom,
        );
        builder = match found.symptom {
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
            Symptom::MissingRef => builder
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
    } else {
        builder = builder.detail(Text::new().plain(
            "ODS doesn't recognise this error, so it won't guess the cause. Here is what it does know.",
        ));
    }
    builder = context(facts, builder);
    builder = where_it_is(facts, builder);
    builder = next_steps(facts, builder);
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
    let mut frontier = vec![facts.node];
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
        assert!(similar("zzzzzz", &names).is_empty());
    }
}
