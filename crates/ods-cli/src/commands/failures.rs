//! Failed nodes, explained (#323, ADR-0025): gathers what ODS knows about each failure
//! (the run's journal, earlier runs, the plan, the last committed state, the manifest
//! and column lineage), has the dbt catalogue classify each redacted error, and
//! renders the explanations as view nodes for `ods state run`, `build`, `test` and
//! `history --run`.
//!
//! Explanations are computed when shown, from what is kept anyway; nothing new is
//! stored, so an older run is explained with the current catalogue.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ods_core::failure::{Confidence, ErrorExplanation, Text};
use ods_core::state::{ExecutionPlan, StateSnapshot};
use ods_provider_dbt::error_catalogue::{DbtErrorCatalogue, project_index};
use ods_provider_dbt::events::project_failure;
use ods_sdk::contracts::error_catalogue::{ErrorCatalogue, ProjectIndex};
use ods_sdk::contracts::run_events::{NodeRunStatus, RunSummary};
use ods_sdk::run_journal::Journals;
use ods_state::{FailureFacts, FailureStage, explain_failure};

use super::lineage::{LoadOptions, Loaded, shared_cache};
use super::state_plan::display_name;
use crate::present::{Line, Span, Tone, TreeItem, ViewNode};

/// How many earlier journals are read for a node's history.
const HISTORY_JOURNALS: usize = 20;

/// Where a project's files are, for explanations.
pub(super) struct ProjectFiles<'a> {
    /// The dbt project.
    pub(super) project_dir: &'a Path,
    /// Its target directory, with the manifest dbt wrote last.
    pub(super) target_dir: &'a Path,
    /// The manifest, if one was read.
    pub(super) manifest: Option<&'a ods_provider_dbt::Manifest>,
}

impl ProjectFiles<'_> {
    /// The target directory as the project names it, e.g. `target`.
    fn target_name(&self) -> String {
        let absolute = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_owned());
        let (target, project) = (absolute(self.target_dir), absolute(self.project_dir));
        // One side may reach the project through a symlink the other doesn't (macOS's
        // `/var` is `/private/var`), so compare the resolved paths when the plain ones
        // don't nest.
        let resolved = |p: &Path| std::fs::canonicalize(p).ok();
        let relative = target
            .strip_prefix(&project)
            .map(Path::to_path_buf)
            .ok()
            .or_else(|| {
                let (target, project) = (resolved(&target)?, resolved(&project)?);
                target.strip_prefix(project).map(Path::to_path_buf).ok()
            });
        let text = relative.unwrap_or(target).display().to_string();
        text.strip_prefix("./").unwrap_or(&text).to_owned()
    }

    pub(super) fn index(&self) -> Option<ProjectIndex> {
        let name = self.target_name();
        self.manifest.map(|m| project_index(m, Some(&name)))
    }
}

/// What explains a run's failures, besides the run itself.
pub(super) struct Evidence<'a> {
    pub(super) files: ProjectFiles<'a>,
    /// The plan the run built from, when it is known.
    pub(super) plan: Option<&'a ExecutionPlan>,
    /// The state committed before the run.
    pub(super) before: Option<&'a StateSnapshot>,
    /// The state database, beside which the journals are.
    pub(super) state_db: &'a Path,
    /// The command that retries what failed, when this run can be retried.
    pub(super) retry: Option<ods_state::Retry<'a>>,
    /// Whether the manifest and lineage describe the code the run ran: right after it,
    /// or when the manifest was written by that run (its invocation is the run id).
    pub(super) project_is_run: bool,
}

/// Earlier runs than `run`, from their journals, newest first.
fn earlier_runs(state_db: &Path, run: &RunSummary) -> Vec<RunSummary> {
    let Ok(files) = Journals::beside(state_db).list() else {
        return Vec::new();
    };
    let mut runs: Vec<RunSummary> = files
        .into_iter()
        .filter(|f| Some(&f.run_id) != run.run_id.as_ref())
        .take(HISTORY_JOURNALS)
        .filter_map(|f| super::run_journal::read(&f.path).ok().flatten())
        .map(|j| RunSummary::from_events(&j.events))
        .filter(|r| match (r.started_at, run.started_at) {
            (Some(earlier), Some(this)) => earlier < this,
            _ => false,
        })
        .collect();
    runs.sort_by_key(|r| std::cmp::Reverse(r.started_at));
    runs
}

/// Explains every failed node of `run`.
pub(super) fn explain_run(run: &RunSummary, evidence: &Evidence<'_>) -> Vec<ErrorExplanation> {
    let failed: Vec<&str> = run
        .nodes
        .iter()
        .filter(|n| n.stats.status == NodeRunStatus::Error)
        .map(|n| n.node.as_str())
        .collect();
    if failed.is_empty() {
        return Vec::new();
    }
    let catalogue = DbtErrorCatalogue::new();
    let info = catalogue.catalogue();
    let index = evidence.files.index();
    let history = earlier_runs(evidence.state_db, run);
    // Lineage only if a missing column needs it: it analyzes the whole project.
    let mut lineage: Option<Option<Loaded>> = None;
    let mut explanations = Vec::new();
    for node in failed {
        let Some(summary) = run.get(node) else {
            continue;
        };
        let stats = &summary.stats;
        let classification = match &stats.error {
            Some(error) => catalogue.classify(error),
            None => ods_sdk::contracts::error_catalogue::Classification::NotRecognised {
                category: ods_core::failure::ErrorCategory::Unknown,
            },
        };
        let wants_lineage = matches!(
            &classification,
            ods_sdk::contracts::error_catalogue::Classification::Recognised(m)
                if m.symptom == ods_core::failure::Symptom::MissingColumn
        );
        let missing = if wants_lineage {
            let loaded = lineage.get_or_insert_with(|| {
                let options = LoadOptions {
                    dialect: None,
                    preference: ods_provider_dbt::ArtifactPreference::Auto,
                    observed: None,
                    trust_observed: false,
                };
                Loaded::from_dir(evidence.files.target_dir, &options, shared_cache()).ok()
            });
            loaded
                .as_ref()
                .map(|l| l.graph.missing_columns(node))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut facts = FailureFacts::new(node, &classification, &info, FailureStage::Run);
        facts.error = stats.error.as_ref();
        facts.stats = Some(stats);
        facts.run = Some(run);
        facts.plan = evidence.plan;
        facts.before = evidence.before;
        facts.history = &history;
        facts.index = index.as_ref();
        facts.missing_columns = &missing;
        facts.retry = evidence.retry;
        facts.project_is_run = evidence.project_is_run;
        explanations.push(explain_failure(&facts));
    }
    explanations
}

/// Explains a failure that stopped the whole project before any node ran (e.g. `dbt
/// compile` in the prepare step), from what dbt printed. `None` if dbt's error can't
/// be found in it.
pub(super) fn explain_prepare(output: &str, evidence: &Evidence<'_>) -> Option<ErrorExplanation> {
    let failure = project_failure(output)?;
    let catalogue = DbtErrorCatalogue::new();
    let info = catalogue.catalogue();
    let classification = catalogue.classify(&failure.summary);
    let index = evidence.files.index();
    // The node dbt named, by the file it named: names alone can repeat across packages.
    let node = index
        .as_ref()
        .and_then(|i| {
            i.nodes
                .iter()
                .find(|(_, n)| {
                    failure.file.is_some()
                        && n.file == failure.file
                        && failure.node_name.as_deref() == Some(n.name.as_str())
                })
                .map(|(id, _)| id.clone())
        })
        .or_else(|| failure.file.clone())
        .unwrap_or_else(|| "project".to_owned());
    let mut facts = FailureFacts::new(&node, &classification, &info, FailureStage::Prepare);
    facts.error = Some(&failure.summary);
    facts.index = index.as_ref();
    facts.plan = evidence.plan;
    facts.before = evidence.before;
    Some(explain_failure(&facts))
}

/// What was removed from the engine's message, said the same way everywhere.
pub(super) const REDACTED_NOTE: &str = "Literal values and SQL removed.";

/// Text with code spans, as a line.
fn line(text: &Text) -> Line {
    text.parts()
        .into_iter()
        .map(|(part, code)| {
            if code {
                Span::toned(part, Tone::Code)
            } else {
                Span::plain(part)
            }
        })
        .collect()
}

fn names(nodes: &[String]) -> String {
    nodes
        .iter()
        .map(|n| display_name(n))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where, for people: `models/marts/customers.sql:16 (compiled line 25 · target/…)`.
pub(super) fn where_line(explanation: &ErrorExplanation) -> Option<Line> {
    let location = explanation.location()?;
    let mut spans = Vec::new();
    if let Some(file) = &location.file {
        let at = location
            .line
            .map_or_else(|| file.clone(), |l| format!("{file}:{l}"));
        spans.push(Span::toned(at, Tone::Code));
    }
    let compiled = match (&location.reported_line, &location.compiled_file) {
        (Some(l), Some(f)) => Some(format!(
            "reported at line {l} of the code it ran · compiled: {f}"
        )),
        (Some(l), None) => Some(format!("reported at line {l} of the code it ran")),
        (None, Some(f)) => Some(format!("compiled: {f}")),
        (None, None) => None,
    };
    if let Some(compiled) = compiled {
        if !spans.is_empty() {
            spans.push(Span::plain("  "));
        }
        spans.push(Span::toned(compiled, Tone::Muted));
    }
    (!spans.is_empty()).then_some(spans)
}

/// The impact, for people: `blocks 1 downstream node (customer_segments was skipped).
/// Its last good build is kept.`
pub(super) fn impact_text(explanation: &ErrorExplanation) -> Option<String> {
    let impact = explanation.impact()?;
    let n = impact.blocked.len();
    let mut text = format!(
        "blocks {n} downstream node{} ({} {} skipped).",
        if n == 1 { "" } else { "s" },
        names(&impact.blocked),
        if n == 1 { "was" } else { "were" }
    );
    match impact.kept.len() {
        0 => {}
        k if k == n => text.push_str(if n == 1 {
            " Its last good build is kept."
        } else {
            " Their last good builds are kept."
        }),
        _ => {
            let _ = write!(
                text,
                " Last good builds are kept for {}.",
                names(&impact.kept)
            );
        }
    }
    Some(text)
}

/// Why ODS thinks so (or what it knows), with each item's source.
fn evidence_tree(explanation: &ErrorExplanation, recognised: bool) -> Option<ViewNode> {
    if explanation.evidence().is_empty() {
        return None;
    }
    let label = if recognised {
        "why ODS thinks so"
    } else {
        "what ODS knows"
    };
    Some(ViewNode::Tree(TreeItem {
        label: vec![Span::toned(label, Tone::Emphasis)],
        children: explanation
            .evidence()
            .iter()
            .map(|e| {
                let mut l = line(&e.text);
                l.push(Span::toned(
                    format!("  [{}]", e.source.label()),
                    Tone::Muted,
                ));
                TreeItem::leaf(l)
            })
            .collect(),
    }))
}

/// What to try, numbered, with the commands under each step.
fn suggestions_tree(explanation: &ErrorExplanation) -> Option<ViewNode> {
    if explanation.suggestions().is_empty() {
        return None;
    }
    Some(ViewNode::Tree(TreeItem {
        label: vec![Span::toned("what to try", Tone::Emphasis)],
        children: explanation
            .suggestions()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut label = vec![Span::plain(format!("{}. ", i + 1))];
                label.extend(line(&s.text));
                TreeItem {
                    label,
                    children: s
                        .commands
                        .iter()
                        .map(|c| TreeItem::leaf(vec![Span::toned(format!("$ {c}"), Tone::Code)]))
                        .collect(),
                }
            })
            .collect(),
    }))
}

/// One failed node's explanation, as the terminal shows it.
pub(super) fn view(explanation: &ErrorExplanation) -> ViewNode {
    let recognised = explanation.confidence() != Confidence::NotRecognised;
    let mut blocks = vec![ViewNode::KeyValue(vec![
        (
            "failed".into(),
            vec![Span::toned(display_name(explanation.node()), Tone::Code)],
        ),
        ("what".into(), {
            let mut l = line(explanation.headline());
            for span in &mut l {
                if span.tone.is_none() {
                    span.tone = Some(Tone::Emphasis);
                }
            }
            l
        }),
        ("kind".into(), vec![Span::plain(explanation.chip())]),
        (
            "confidence".into(),
            vec![Span::toned(
                explanation.confidence().label(),
                if recognised {
                    Tone::Emphasis
                } else {
                    Tone::Warning
                },
            )],
        ),
    ])];
    if let Some(detail) = explanation.detail() {
        blocks.push(ViewNode::Paragraph(line(detail)));
    }
    blocks.extend(evidence_tree(explanation, recognised));
    let mut facts = Vec::new();
    if let Some(at) = where_line(explanation) {
        facts.push(("where".to_owned(), at));
    }
    if let Some(impact) = impact_text(explanation) {
        facts.push(("impact".to_owned(), vec![Span::plain(impact)]));
    }
    if !facts.is_empty() {
        blocks.push(ViewNode::KeyValue(facts));
    }
    blocks.extend(suggestions_tree(explanation));
    if let Some(message) = explanation.engine_message() {
        let mut lines = vec![(
            format!("{} said", message.engine),
            vec![
                Span::plain(message.message.clone()),
                Span::toned(format!("  {REDACTED_NOTE}"), Tone::Muted),
            ],
        )];
        if let Some(at) = &message.details_at {
            lines.push((
                "full text".to_owned(),
                vec![Span::toned(at.clone(), Tone::Code)],
            ));
        }
        blocks.push(ViewNode::KeyValue(lines));
    }
    ViewNode::Group(blocks)
}

/// Every explanation, under a heading.
pub(super) fn section(explanations: &[ErrorExplanation]) -> Vec<ViewNode> {
    if explanations.is_empty() {
        return Vec::new();
    }
    let mut blocks = vec![ViewNode::Heading(if explanations.len() == 1 {
        "Why it failed".to_owned()
    } else {
        format!("Why {} nodes failed", explanations.len())
    })];
    blocks.extend(explanations.iter().map(view));
    blocks
}

/// How to retry the last run: with `--state-db` when it came from a flag or the
/// environment, which a retry wouldn't see otherwise.
pub(super) fn retry_state_db(settings: &super::state_settings::StateSettings) -> Option<&str> {
    use super::state_settings::Origin;
    matches!(settings.state_db.origin, Origin::Flag | Origin::Env(_))
        .then_some(settings.state_db.value.as_str())
}

/// The files of a project, from the state settings.
pub(super) fn project_dir(settings: &super::state_settings::StateSettings) -> PathBuf {
    settings
        .project_dir
        .as_ref()
        .map_or_else(|| PathBuf::from("."), |s| PathBuf::from(&s.value))
}

#[cfg(test)]
mod tests {
    use super::ProjectFiles;

    /// The project reached through a symlink and its target directory through the real
    /// path (macOS's `/var` is `/private/var`) still gives the name the project uses.
    #[cfg(unix)]
    #[test]
    fn the_target_is_named_relative_to_the_project_through_a_symlink() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("target")).expect("the target directory");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("a symlink to the project");
        let files = ProjectFiles {
            project_dir: &link,
            target_dir: &real.join("target"),
            manifest: None,
        };
        assert_eq!(files.target_name(), "target");
        let files = ProjectFiles {
            project_dir: &link,
            target_dir: &link.join("target"),
            manifest: None,
        };
        assert_eq!(files.target_name(), "target");
    }
}
