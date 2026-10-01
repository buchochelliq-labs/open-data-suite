//! The Lineage page (#312): the explorer inside the dashboard shell, with the State
//! overlay, and the overlay's view model ([`LineageOverlay`], `/api/lineage/overlay`).
//!
//! The overlay says, for every node the plan covers, what the next run would do with
//! it: build, reuse, build because it was never built, or build because the evidence
//! to reuse it is missing. It comes from the same plan as `ods state plan`, made again
//! as of each request (lag tolerances expire with time, AGENTS rule 3). The graph's
//! edges are the DAG's: a node reads another. They are never relationships; those
//! belong to the ERD (AGENTS rule 6).

use std::collections::BTreeMap;

use ods_core::state::{ExecutionPlan, PlanAction, PlanEntry, Reason, ReasonCode, Timestamp};
use ods_lineage::{GraphDocument, NodeKind};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;

use crate::home::{Frame, framed};

use crate::dashboard::{
    Dashboard, RunRecord, ShellView, StateInput, StateStatus, reason_label, sentence, short,
};

/// Version of [`LineageOverlay`]. Additive fields don't change it; a removed or
/// retyped field does.
pub const OVERLAY_SCHEMA_VERSION: u32 = 1;

/// The State overlay: the plan's decision for each node of the graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LineageOverlay {
    /// Format version ([`OVERLAY_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Where the state is. Without a store, every node is `never_built`.
    pub state: StateStatus,
    /// The snapshot the plan compares against, if any.
    pub based_on: Option<u64>,
    /// When the plan was made; `None` when there is no plan.
    pub planned_at: Option<Timestamp>,
    /// Why there is no plan, when there should be one. Beyond loopback, only that it
    /// failed: the text goes to the server log.
    pub error: Option<String>,
    /// What qualifies the plan, e.g. missing source freshness.
    pub warnings: Vec<String>,
    /// How many nodes get each decision.
    pub counts: BTreeMap<Decision, usize>,
    /// Each planned node of the graph, by id. Sources aren't built, so they have none.
    pub nodes: BTreeMap<String, NodeOverlay>,
}

/// What the next run does with a node, as the overlay colours it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Decision {
    /// It builds: its code, checks, target or inputs changed.
    Build,
    /// It keeps its last successful build: its code and inputs are unchanged, or its
    /// new upstream data is within its lag tolerance (see the summary). Whether its
    /// relation is still in the warehouse is only known when the plan checked it, which
    /// this page's plan doesn't: see [`NodeOverlay::relation`].
    Reuse,
    /// It builds: no successful build is recorded.
    NeverBuilt,
    /// It builds, because the evidence to reuse it is missing or the plan couldn't be
    /// made. Never shown as reuse (AGENTS rule 3).
    Unknown,
}

/// One node's decision, with its reason chain (AGENTS rule 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct NodeOverlay {
    /// The decision.
    pub decision: Decision,
    /// The main reason in words, e.g. `code changed`.
    pub summary: String,
    /// Why, most important first, as the planner gives it.
    pub reasons: Vec<ReasonView>,
    /// Fingerprint components that differ from the last successful build.
    pub changed_components: Vec<String>,
    /// Every fingerprint component compared, changed or not; empty when there was no
    /// recorded build to compare with. Changed ones first.
    pub components: Vec<ComponentView>,
    /// For a reused node, what is known about its relation (table or view) in the
    /// warehouse: whether the plan checked it, and what it found. `None` otherwise.
    pub relation: Option<String>,
    /// The recent run that last built it, if it is one of them.
    pub last_built: Option<LastBuilt>,
    /// Why its column lineage is unknown, if it is opaque.
    pub opaque: Option<String>,
    /// Its Model page, relative to the dashboard's root.
    pub model_href: String,
    /// Its decision on the State plan page, relative to the dashboard's root; `None`
    /// when the plan has no entry for it (no store, or no plan), so nothing to explain.
    pub why_href: Option<String>,
}

/// One reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ReasonView {
    /// The planner's stable code; `None` for what the dashboard says when there is no
    /// plan entry, e.g. when the plan couldn't be made.
    pub code: Option<ReasonCode>,
    /// The code in words, e.g. `upstream code changed`.
    pub label: String,
    /// The planner's explanation, with run ids shortened.
    pub message: String,
}

/// A fingerprint component, and whether it changed since the last successful build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct ComponentView {
    /// Its name, e.g. `sql` or `config`.
    pub name: String,
    /// Whether it differs.
    pub changed: bool,
}

/// What a reused node's relation check says when the plan didn't check the warehouse,
/// as this page's plan never does. The words of `ods state run`'s notice, for this page.
pub const RELATION_NOT_CHECKED: &str = "not checked: this page doesn't query the warehouse; \
    `ods state build --dry-run` checks";

/// The warning the overlay carries whenever it shows reuse it took on trust.
pub const TRUSTED_REUSE: &str = "reuse assumes each relation built earlier still exists: \
    this page didn't check the warehouse (`ods state build --dry-run` does)";

/// The recorded run a node's last successful build came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub struct LastBuilt {
    /// The snapshot the run committed.
    pub snapshot: u64,
    /// The run's id, first eight characters.
    pub run: String,
    /// The target it built in, if recorded.
    pub target: Option<String>,
}

/// Everything but RFC 3986's unreserved characters is percent-encoded, so an id is one
/// path segment or query value whatever it holds, and a plain one stays readable.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// `model.shop.orders` → `catalog/model.shop.orders`; `a/b` → `catalog/a%2Fb`.
fn model_href(id: &str) -> String {
    format!("catalog/{}", utf8_percent_encode(id, COMPONENT))
}

/// `model.shop.orders` → `state/plan?node=model.shop.orders`.
fn why_href(id: &str) -> String {
    format!("state/plan?node={}", utf8_percent_encode(id, COMPONENT))
}

/// The decision a plan entry shows as. Only the first (most important) reason decides
/// between BUILD, NEVER BUILT and UNKNOWN: a build whose first reason is missing
/// evidence is UNKNOWN even if a later reason is, say, a code change.
fn decision(entry: &PlanEntry) -> Decision {
    match entry.action {
        PlanAction::Reuse => Decision::Reuse,
        PlanAction::Build => match entry.reasons.first().map(|r| r.code) {
            Some(ReasonCode::NeverBuilt) => Decision::NeverBuilt,
            // A build with no reason would be a planner bug: shown as unknown, not hidden.
            None
            | Some(
                ReasonCode::MissingDataEvidence
                | ReasonCode::CodeEvidenceIncomplete
                | ReasonCode::UnknownDependency
                | ReasonCode::RelationUnverified,
            ) => Decision::Unknown,
            Some(_) => Decision::Build,
        },
        // An action this page doesn't know yet: whether it builds isn't known.
        _ => Decision::Unknown,
    }
}

fn reason_view(reason: &Reason, runs: &[RunRecord]) -> ReasonView {
    // Long run ids read better short, as on Home.
    let message = runs.iter().fold(reason.message.clone(), |m, run| {
        m.replace(&run.run_id, &short(&run.run_id))
    });
    ReasonView {
        code: Some(reason.code),
        label: reason_label(reason.code),
        message: sentence(&message),
    }
}

/// What the plan knows of a reused node's relation, from its `relation_exists`
/// evidence: found (and as what), or not checked.
fn relation_fact(entry: &PlanEntry) -> Option<String> {
    if entry.action != PlanAction::Reuse {
        return None;
    }
    let checked = entry
        .evidence
        .iter()
        .find(|e| e.kind == "relation_exists")
        .and_then(|e| e.value.as_deref());
    Some(checked.map_or_else(
        || RELATION_NOT_CHECKED.to_owned(),
        |found| format!("checked: in the warehouse ({found})"),
    ))
}

/// Each component of the recorded fingerprint, changed ones first, then by name.
fn components(entry: &PlanEntry, recorded: Option<&Vec<String>>) -> Vec<ComponentView> {
    let Some(recorded) = recorded else {
        return Vec::new();
    };
    let mut names: Vec<&String> = recorded.iter().chain(&entry.changed_components).collect();
    names.sort();
    names.dedup();
    let mut views: Vec<ComponentView> = names
        .into_iter()
        .map(|name| ComponentView {
            name: name.clone(),
            changed: entry.changed_components.contains(name),
        })
        .collect();
    views.sort_by_key(|c| !c.changed);
    views
}

/// What the whole graph shows when there is no plan: the same decision and reason for
/// every node that would be planned.
struct Blanket {
    decision: Decision,
    reason: ReasonView,
}

impl Blanket {
    /// The plan couldn't be made: every node would build, for a reason not known.
    fn unknown(message: &str) -> Self {
        Self {
            decision: Decision::Unknown,
            reason: ReasonView {
                code: None,
                label: "plan unavailable".to_owned(),
                message: message.to_owned(),
            },
        }
    }
}

/// What the overlay rests on: the plan (or why there is none) and the recent runs.
struct Basis<'a> {
    overlay: LineageOverlay,
    plan: Option<ExecutionPlan>,
    runs: &'a [RunRecord],
    blanket: Option<Blanket>,
}

/// A node's decision, main reason, reasons and changed fingerprint components.
type Decided = (Decision, String, Vec<ReasonView>, Vec<String>);

fn decided(entry: Option<&PlanEntry>, blanket: Option<&Blanket>, runs: &[RunRecord]) -> Decided {
    match (entry, blanket) {
        (Some(entry), _) => {
            let reasons: Vec<ReasonView> =
                entry.reasons.iter().map(|r| reason_view(r, runs)).collect();
            (
                decision(entry),
                reasons
                    .first()
                    .map_or_else(|| "no reason given".to_owned(), |r| r.label.clone()),
                reasons,
                entry.changed_components.clone(),
            )
        }
        (None, Some(blanket)) => (
            blanket.decision,
            blanket.reason.label.clone(),
            vec![blanket.reason.clone()],
            Vec::new(),
        ),
        // The plan covers the graph's buildable nodes; one it left out can't be shown
        // as reused.
        (None, None) => (
            Decision::Unknown,
            "not planned".to_owned(),
            vec![ReasonView {
                code: None,
                label: "not planned".to_owned(),
                message: "The plan doesn't cover this node, so what the next run does with \
                          it isn't known."
                    .to_owned(),
            }],
            Vec::new(),
        ),
    }
}

impl Dashboard {
    /// The State overlay for `document` as of now. `details` shows error text (on
    /// loopback only).
    pub fn lineage_overlay(&self, document: &GraphDocument, details: bool) -> LineageOverlay {
        self.lineage_overlay_at(document, details, Timestamp::now())
    }

    /// The State overlay for `document` as of `now`: with a planner, the plan is made
    /// again for `now`, as `ods state plan` would make it.
    pub fn lineage_overlay_at(
        &self,
        document: &GraphDocument,
        details: bool,
        now: Timestamp,
    ) -> LineageOverlay {
        let Basis {
            mut overlay,
            plan,
            runs,
            blanket,
        } = self.overlay_basis(details, now);
        let opaque: BTreeMap<&str, &str> = self
            .opaque
            .iter()
            .map(|n| (n.id.as_str(), n.why.as_str()))
            .collect();
        // The latest snapshot's fingerprints, to list what was compared (#311's History).
        let recorded_components: BTreeMap<&str, Vec<String>> = self
            .history()
            .and_then(|h| h.snapshots.first())
            .map(|(_, snapshot)| {
                snapshot
                    .nodes
                    .iter()
                    .map(|(id, n)| {
                        (
                            id.as_str(),
                            n.fingerprint.components.keys().cloned().collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let entries: BTreeMap<&str, &PlanEntry> = plan
            .iter()
            .flat_map(|p| p.entries.iter())
            .map(|e| (e.node.as_str(), e))
            .collect();
        for node in &document.nodes {
            // Sources are read, never built: there is nothing to decide.
            if node.kind == NodeKind::Source {
                continue;
            }
            let entry = entries.get(node.id.as_str()).copied();
            let (decision, summary, reasons, changed_components) =
                decided(entry, blanket.as_ref(), runs);
            // The latest run's snapshot holds every node's last good build.
            let recorded = recorded_components.get(node.id.as_str());
            let last_built = runs
                .iter()
                .find(|run| run.built.iter().any(|b| b == &node.id))
                .map(|run| LastBuilt {
                    snapshot: run.snapshot,
                    run: short(&run.run_id),
                    target: run.target.as_ref().map(|t| t.name.clone()),
                });
            let opaque = node.opaque.then(|| {
                opaque.get(node.id.as_str()).map_or_else(
                    || "Its SQL couldn't be analyzed: column lineage unknown".to_owned(),
                    |why| (*why).to_owned(),
                )
            });
            *overlay.counts.entry(decision).or_default() += 1;
            overlay.nodes.insert(
                node.id.clone(),
                NodeOverlay {
                    decision,
                    summary,
                    reasons,
                    changed_components,
                    components: entry.map(|e| components(e, recorded)).unwrap_or_default(),
                    relation: entry.and_then(relation_fact),
                    last_built,
                    opaque,
                    model_href: model_href(&node.id),
                    why_href: entry.map(|_| why_href(&node.id)),
                },
            );
        }
        // Reuse taken on trust is said so, as `ods state run` says it (rules 3 and 4).
        if overlay
            .nodes
            .values()
            .any(|n| n.relation.as_deref() == Some(RELATION_NOT_CHECKED))
        {
            overlay.warnings.push(TRUSTED_REUSE.to_owned());
        }
        overlay
    }

    /// The plan as of `now`, or why there is none, with the overlay's header filled in.
    fn overlay_basis(&self, details: bool, now: Timestamp) -> Basis<'_> {
        let hidden = |error: &str| {
            if details {
                error.to_owned()
            } else {
                "the plan couldn't be made; see the server log".to_owned()
            }
        };
        let mut overlay = LineageOverlay {
            schema_version: OVERLAY_SCHEMA_VERSION,
            state: StateStatus::NoStore,
            based_on: None,
            planned_at: None,
            error: None,
            warnings: Vec::new(),
            counts: BTreeMap::new(),
            nodes: BTreeMap::new(),
        };
        let basis = |overlay, blanket| Basis {
            overlay,
            plan: None,
            runs: &[],
            blanket: Some(blanket),
        };
        match &self.state {
            StateInput::NoStore { .. } => basis(
                overlay,
                Blanket {
                    decision: Decision::NeverBuilt,
                    reason: ReasonView {
                        code: Some(ReasonCode::NeverBuilt),
                        label: reason_label(ReasonCode::NeverBuilt),
                        message: "No state store yet: no build is recorded, so every node \
                                  builds. Record a first run with `ods state build`."
                            .to_owned(),
                    },
                },
            ),
            StateInput::Unreadable { error, .. } => {
                overlay.state = StateStatus::Unreadable;
                overlay.error = Some(hidden(error));
                basis(
                    overlay,
                    Blanket::unknown(
                        "The state store can't be read, so the plan couldn't be made: every \
                         node would build. `ods state doctor` says what is wrong.",
                    ),
                )
            }
            StateInput::ProjectUnreadable { error, .. } => {
                overlay.state = StateStatus::ProjectUnreadable;
                overlay.error = Some(hidden(error));
                basis(
                    overlay,
                    Blanket::unknown(
                        "The project can't be read, so the plan couldn't be made: every node \
                         would build.",
                    ),
                )
            }
            StateInput::Recorded(recorded) => {
                overlay.state = if recorded.runs.is_empty() {
                    StateStatus::NoRuns
                } else {
                    StateStatus::Recorded
                };
                // The plan every page shares: memoised per reload and time bucket (#311).
                let (planned, warnings) = self
                    .plan_at(now)
                    .unwrap_or_else(|| (recorded.plan.clone(), recorded.warnings.clone()));
                match planned {
                    Ok(plan) => {
                        overlay.warnings = warnings;
                        overlay.based_on = plan.based_on.map(|s| s.0);
                        overlay.planned_at = Some(plan.created_at);
                        Basis {
                            overlay,
                            plan: Some(plan),
                            runs: &recorded.runs,
                            blanket: None,
                        }
                    }
                    Err(error) => {
                        overlay.error = Some(hidden(&error));
                        basis(
                            overlay,
                            Blanket::unknown(
                                "The plan couldn't be made, so whether this node would be \
                                 reused isn't known: it would build.",
                            ),
                        )
                    }
                }
            }
        }
    }
}

// --------------------------------------------------------------------------- page

/// The page's own stylesheet: the explorer, in the dashboard's tokens.
pub(crate) const CSS: &str = include_str!("../assets/lineage.css");
/// The explorer's script, shared by the served page and the offline one.
pub(crate) const JS: &str = include_str!("../assets/lineage.js");
/// The live run view (#322): served only, as it streams from the server.
pub(crate) const LIVE_JS: &str = include_str!("../assets/live.js");
/// Its stylesheet.
pub(crate) const LIVE_CSS: &str = include_str!("../assets/live.css");

/// The explorer's markup: toolbar, canvas, legend and side panel. `served` adds the
/// overlay picker and impact, which need the server.
pub(crate) fn explorer_markup(served: bool) -> String {
    let overlay = if served {
        r#"<label class="lin-muted" for="lin-overlay">Overlay</label>
<select id="lin-overlay" title="What colours the nodes">
<option value="state" selected>State decision</option>
<option value="none">None</option>
<option disabled>Resource type (planned)</option>
<option disabled>Freshness evidence (planned)</option>
<option disabled>Lineage confidence (planned)</option>
<option disabled>Latest status (planned)</option>
</select>"#
    } else {
        ""
    };
    let impact = if served {
        r#"<button type="button" id="lin-impact" class="lin-primary" title="What must run if the selected node or column changes">Simulate impact</button>"#
    } else {
        ""
    };
    format!(
        r##"<div class="lin" id="lin">
<a class="lin-skip" href="#lin-panel">Skip to the selected node's details</a>
<section class="lin-main" aria-label="Lineage graph">
<div class="lin-bar">
<div class="lin-find"><label class="lin-sel" for="lin-search"><span class="lin-muted">search</span><input id="lin-search" type="search" autocomplete="off" spellcheck="false" placeholder="model or column" aria-label="Search model and column names" role="combobox" aria-autocomplete="list" aria-expanded="false" aria-controls="lin-results"></label><div id="lin-results" role="listbox" aria-label="Matching models and columns"></div></div>
{overlay}
<label class="lin-check"><input type="checkbox" id="lin-columns">Columns</label>
<label class="lin-check" title="Column view: also show inputs that shape rows (joins, filters, grouping)"><input type="checkbox" id="lin-indirect" checked>Indirect edges</label>
<button type="button" id="lin-fit" class="lin-fit">Fit</button>
{impact}
</div>
<div class="lin-canvas" id="lin-canvas"><svg id="lin-svg" role="group" aria-label="Lineage graph: each node reads the nodes to its left"><g id="lin-viewport"><g id="lin-edges"></g><g id="lin-nodes"></g></g></svg>
<div class="lin-legend" id="lin-legend"></div>
<div class="lin-corner"><span id="lin-stats"></span><label class="lin-check" title="Show only the selection and what it connects to"><input type="checkbox" id="lin-focus">Focus on selection</label></div>
</div>
</section>
<aside class="lin-panel" id="lin-panel" aria-label="Selected node" tabindex="-1"></aside>
</div>"##
    )
}

/// Serializes `value` so it can sit inside a `<script>` element: `<` is escaped, so
/// no value (e.g. a model named `</script>`) can end the element early.
pub(crate) fn embeddable<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    Ok(serde_json::to_string(value)?.replace('<', "\\u003c"))
}

/// The served Lineage page: the shell around the explorer, with the graph and the
/// overlay embedded as its first paint. `node` is a deep link to select.
pub(crate) fn lineage_page(
    shell: &ShellView,
    document: &GraphDocument,
    overlay: &LineageOverlay,
    node: Option<&str>,
    generation: u64,
) -> Result<String, serde_json::Error> {
    // The header says which plan the overlay is, as the design does.
    let status = Some(overlay.based_on.map_or_else(
        || {
            r#"<span class="pill-snap lin-status" title="Without recorded state there is no plan to compare with"><span class="dot none"></span>no recorded state</span>"#
                .to_owned()
        },
        |snapshot| {
            format!(
                r#"<span class="pill-snap lin-status" title="The overlay is the plan against this snapshot"><span class="dot"></span>plan against snapshot {snapshot}</span>"#
            )
        },
    ));
    let graph = embeddable(document)?;
    let overlay = embeddable(overlay)?;
    let selected = embeddable(&node)?;
    let body = format!(
        r#"<meta name="ods-source" content="api">
{markup}
<script>{dagre}</script>
<script type="application/json" id="ods-graph">{graph}</script>
<script type="application/json" id="ods-overlay">{overlay}</script>
<script type="application/json" id="ods-selected">{selected}</script>
<script>{LIVE_JS}</script>
<script>{JS}</script>"#,
        markup = explorer_markup(true),
        dagre = crate::page::DAGRE,
    );
    // The live view's node card is the Run page's (#322), so its styles come too.
    let css = format!("{CSS}{}{LIVE_CSS}", crate::state_pages::CSS);
    let frame = Frame {
        title: "Lineage",
        crumbs: None,
        root: "",
        status,
        sub: None,
        // The toolbar's search is the page's; the header's would search elsewhere.
        search: false,
        css: &css,
        js: "",
    };
    Ok(framed(shell, &frame, &body, generation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ods_core::FreshnessPolicy;

    fn entry(action: PlanAction, codes: &[ReasonCode]) -> PlanEntry {
        PlanEntry::new(
            "model.a",
            "a",
            "model",
            action,
            codes.iter().map(|c| Reason::new(*c, "why")).collect(),
            FreshnessPolicy::conservative(),
            0,
        )
    }

    #[test]
    fn missing_evidence_is_never_shown_as_reuse() {
        use ReasonCode::*;
        assert_eq!(
            decision(&entry(PlanAction::Reuse, &[Unchanged])),
            Decision::Reuse
        );
        assert_eq!(
            decision(&entry(PlanAction::Build, &[CodeChanged])),
            Decision::Build
        );
        assert_eq!(
            decision(&entry(PlanAction::Build, &[TargetChanged])),
            Decision::Build
        );
        assert_eq!(
            decision(&entry(PlanAction::Build, &[NeverBuilt])),
            Decision::NeverBuilt
        );
        for code in [
            MissingDataEvidence,
            CodeEvidenceIncomplete,
            UnknownDependency,
            RelationUnverified,
        ] {
            assert_eq!(
                decision(&entry(PlanAction::Build, &[code])),
                Decision::Unknown
            );
        }
        assert_eq!(decision(&entry(PlanAction::Build, &[])), Decision::Unknown);
    }

    #[test]
    fn links_percent_encode_the_id() {
        assert_eq!(model_href("model.a_b-c~"), "catalog/model.a_b-c~");
        assert_eq!(model_href("model.a b/c?"), "catalog/model.a%20b%2Fc%3F");
        assert_eq!(why_href("model.x&y#z"), "state/plan?node=model.x%26y%23z");
        assert_eq!(why_href("model.é"), "state/plan?node=model.%C3%A9");
    }

    #[test]
    fn the_inlined_parts_cannot_collide_with_a_placeholder_or_end_their_element() {
        let dashboard_css = include_str!("../assets/dashboard.css");
        let offline = explorer_markup(false);
        let served = explorer_markup(true);
        for asset in [
            CSS,
            JS,
            LIVE_JS,
            LIVE_CSS,
            dashboard_css,
            offline.as_str(),
            served.as_str(),
        ] {
            assert!(!asset.contains("__ODS_"));
            assert!(!asset.to_ascii_lowercase().contains("</script"));
            assert!(!asset.to_ascii_lowercase().contains("</style"));
        }
    }
}
