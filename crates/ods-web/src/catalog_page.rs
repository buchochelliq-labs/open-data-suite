//! The Catalog's HTML (#313), rendered on the server from [`CatalogView`] inside the
//! dashboard's shell. It works without script: filters are a form whose values go in
//! the URL query; a little script submits it when a box is ticked, puts the focus
//! back on that box, and binds `/` to the Catalog's own search.

use std::fmt::Write as _;

use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};

use crate::catalog::{
    CatalogQuery, CatalogRow, CatalogView, Decision, DecisionView, LastBuildView,
    LineageConfidence, REUSE_RELATION,
};
use crate::dashboard::{ShellView, StateStatus};
use crate::home::{Frame, framed};
use crate::state_pages::crumbs;

/// The stylesheet of the Catalog and the model pages.
pub(crate) const CSS: &str = include_str!("../assets/catalog.css");

/// The table's columns: sort key (`None` if it doesn't sort), label, what it says.
const COLUMNS: [(Option<&str>, &str, &str); 8] = [
    (Some("name"), "Name", "name"),
    (Some("type"), "Type", "type"),
    (Some("layer"), "Layer*", "layer (inferred)"),
    (Some("materialized"), "Material.", "materialization"),
    (Some("lineage"), "Lineage", "lineage confidence"),
    (Some("decision"), "Next run", "next-run decision"),
    (None, "Health", "Health signals come later (#117)"),
    (Some("last_built"), "Last built", "last successful build"),
];

const SEARCH: &str = r#"<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7"></circle><path d="m21 21-4.3-4.3"></path></svg>"#;

/// `catalog?type=model&sort=layer`: the Catalog with `query`.
fn href(query: &CatalogQuery) -> String {
    let pairs = query.to_pairs();
    if pairs.is_empty() {
        return "catalog".to_owned();
    }
    let encoded = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&pairs)
        .finish();
    format!("catalog?{encoded}")
}

/// The pill for a decision: CSS class, text.
pub(crate) fn pill(decision: Decision) -> (&'static str, &'static str) {
    match decision {
        Decision::Build => ("build", "BUILD"),
        Decision::Reuse => ("reuse", "REUSE"),
        Decision::NeverBuilt => ("never_built", "NEVER BUILT"),
        Decision::Unknown => ("unknown", "UNKNOWN"),
    }
}

/// Why, for a pill's tooltip: a reuse also says it is taken on trust.
pub(crate) fn why(decision: &DecisionView) -> String {
    if decision.decision == Decision::Reuse {
        format!("{}; its relation is {REUSE_RELATION}", decision.summary)
    } else {
        decision.summary.clone()
    }
}

pub(crate) fn decision_pill(decision: &DecisionView) -> String {
    let (class, label) = pill(decision.decision);
    format!(
        r#"<span class="pill {class}" title="{why}">{label}</span>"#,
        why = attr(&why(decision)),
    )
}

pub(crate) fn confidence(lineage: LineageConfidence) -> String {
    format!(
        r#"<span class="conf"><span class="cdot {key}"></span>{label}</span>"#,
        key = attr(&lineage.key().replace('/', "")),
        label = text(lineage.key()),
    )
}

/// `2 · 9ea38bd5`, with when in the tooltip.
pub(crate) fn last_built(build: Option<&LastBuildView>) -> String {
    match build {
        Some(b) => format!(
            r#"<span class="mono" title="snapshot {snap}, run {run}, finished {at}">{snap} · {short}</span>"#,
            snap = b.snapshot.map_or_else(|| "?".to_owned(), |s| s.to_string()),
            run = attr(&b.run_id),
            short = text(&b.short_run_id),
            at = attr(&b.built_at.to_string()),
        ),
        None => r#"<span class="never">never built</span>"#.to_owned(),
    }
}

/// The Catalog, inside the shell.
pub(crate) fn catalog_page(shell: &ShellView, view: &CatalogView, generation: u64) -> String {
    let mut b = String::with_capacity(32 * 1024);
    b.push_str(r#"<div class="catalog">"#);
    filters(&mut b, view);
    b.push_str(r#"<section class="cat-main" aria-label="Nodes">"#);
    toolbar(&mut b, view);
    table(&mut b, view);
    legend(&mut b, view);
    b.push_str("</section></div>");
    b.push_str(SCRIPT);
    let frame = Frame {
        title: "Catalog",
        crumbs: Some(crumbs(
            &[("Catalog", Some("catalog")), ("Models", None)],
            false,
        )),
        root: "",
        status: None,
        sub: Some("models"),
        // The Catalog has its own search; the header's would search elsewhere.
        search: false,
        css: CSS,
        js: "",
    };
    framed(shell, &frame, &b, generation)
}

/// Ticking a box applies it at once, and the focus returns to it after the reload;
/// `/` focuses the Catalog's search. Without script, the Apply button does the same.
const SCRIPT: &str = r#"<script>(function(){var f=document.getElementById("facets");if(!f)return;f.classList.add("live");var k="ods-catalog-focus";try{var id=sessionStorage.getItem(k);sessionStorage.removeItem(k);var el=id&&document.getElementById(id);if(el)el.focus();}catch(_){}f.addEventListener("change",function(e){if(e.target.type!=="checkbox")return;try{sessionStorage.setItem(k,e.target.id);}catch(_){}f.submit();});var q=document.getElementById("catq");window.addEventListener("keydown",function(e){var t=document.activeElement;if(e.key==="/"&&q&&t!==q&&!(t&&(t.tagName==="INPUT"||t.tagName==="TEXTAREA"))){e.preventDefault();q.focus();}});})();</script>"#;

fn filters(b: &mut String, view: &CatalogView) {
    b.push_str(
        r#"<aside class="filters" aria-label="Filters"><form id="facets" method="get" action="catalog"><div class="f-head"><h2>Filters</h2><a href="catalog">Clear all</a></div>"#,
    );
    for facet in &view.facets {
        let _ = write!(
            b,
            r#"<fieldset data-facet="{key}"><legend>{label}</legend>"#,
            key = attr(facet.key),
            label = text(facet.label),
        );
        if facet.values.is_empty() {
            let empty = match facet.key {
                "tag" => "No tags in this project",
                "layer" => "No layers",
                _ => "None",
            };
            let _ = write!(b, r#"<span class="f-empty">{}</span>"#, text(empty));
        }
        for (i, value) in facet.values.iter().enumerate() {
            let swatch = match facet.key {
                "decision" => format!(r#"<span class="sw {}"></span>"#, attr(&value.value)),
                "lineage" => format!(
                    r#"<span class="cdot {}"></span>"#,
                    attr(&value.value.replace('/', ""))
                ),
                _ => String::new(),
            };
            let _ = write!(
                b,
                r#"<label class="{zero}"><input type="checkbox" id="f-{key}-{i}" name="{key}" value="{value}"{checked}>{swatch}<span class="f-label">{label}</span><span class="count">{count}</span></label>"#,
                zero = if value.count == 0 { "zero" } else { "" },
                key = attr(facet.key),
                value = attr(&value.value),
                checked = if value.selected { " checked" } else { "" },
                label = text(&value.label),
                count = value.count,
            );
        }
        if let Some(note) = &facet.note {
            let _ = write!(b, r#"<span class="f-note">{}</span>"#, text(note));
        }
        b.push_str("</fieldset>");
    }
    // The sort goes with the filters.
    if view.query.sort != "name" {
        let _ = write!(
            b,
            r#"<input type="hidden" name="sort" value="{}">"#,
            attr(&view.query.sort)
        );
    }
    if view.query.descending {
        b.push_str(r#"<input type="hidden" name="desc" value="1">"#);
    }
    b.push_str(r#"<button class="btn apply" type="submit">Apply</button></form></aside>"#);
}

fn toolbar(b: &mut String, view: &CatalogView) {
    let _ = write!(
        b,
        r#"<div class="cat-tools"><label class="cat-search">{SEARCH}<input id="catq" form="facets" name="q" value="{q}" aria-label="Search node names" placeholder="Search names" autocomplete="off"><kbd>/</kbd></label><span class="muted" data-shown="{shown}">{shown} of {total} nodes</span>"#,
        q = attr(view.query.search.as_deref().unwrap_or("")),
        shown = view.rows.len(),
        total = view.total,
    );
    b.push_str(r#"<span class="cat-basis">"#);
    let basis = &view.decisions;
    let (dot, message) = match (basis.state, basis.based_on, &basis.error) {
        (_, _, Some(error)) => ("dot none", format!("decisions unknown: {error}")),
        (StateStatus::Recorded, Some(snapshot), None) => {
            ("dot", format!("decisions against snapshot {snapshot}"))
        }
        (StateStatus::NoStore, _, _) => (
            "dot none",
            "no state store: every node is never built".to_owned(),
        ),
        (StateStatus::NoRuns, _, _) => (
            "dot none",
            "no run recorded: every node is never built".to_owned(),
        ),
        (StateStatus::Unreadable, _, _) => (
            "dot none",
            "the state store can't be read: decisions unknown".to_owned(),
        ),
        _ => ("dot none", "decisions unknown".to_owned()),
    };
    let _ = write!(
        b,
        r#"<span class="{dot}"></span>{}</span></div>"#,
        text(&message)
    );
    if !basis.warnings.is_empty() {
        b.push_str(r#"<ul class="warnings">"#);
        for warning in &basis.warnings {
            let _ = write!(b, "<li>{}</li>", text(warning));
        }
        b.push_str("</ul>");
    }
}

fn table(b: &mut String, view: &CatalogView) {
    b.push_str(r#"<div class="cat-table"><table class="cat"><thead><tr>"#);
    for (key, label, says) in COLUMNS {
        match key {
            Some(key) => {
                let current = view.query.sort == key;
                let mut next = view.query.clone();
                key.clone_into(&mut next.sort);
                next.descending = current && !view.query.descending;
                // Only the sorted column says how (ARIA: one `aria-sort` per table).
                let (sort, arrow) = match (current, view.query.descending) {
                    (true, false) => (r#" aria-sort="ascending""#, "↑"),
                    (true, true) => (r#" aria-sort="descending""#, "↓"),
                    _ => ("", "↕"),
                };
                let _ = write!(
                    b,
                    r#"<th{sort}><a href="{href}"{cls} aria-label="Sort by {says}">{label}<span class="arrow" aria-hidden="true">{arrow}</span></a></th>"#,
                    href = attr(&href(&next)),
                    cls = if current { r#" class="on""# } else { "" },
                    says = attr(says),
                    label = text(label),
                );
            }
            None => {
                let _ = write!(b, r#"<th title="{}">{}</th>"#, attr(says), text(label));
            }
        }
    }
    b.push_str("</tr></thead><tbody>");
    for row in &view.rows {
        row_html(b, row);
    }
    if view.rows.is_empty() {
        let message = if view.total == 0 {
            "No nodes: the project's artifacts list none, or couldn't be read."
        } else {
            "No node matches these filters."
        };
        let _ = write!(
            b,
            r#"<tr><td colspan="8" class="none">{}</td></tr>"#,
            text(message)
        );
    }
    b.push_str("</tbody></table></div>");
}

fn row_html(b: &mut String, row: &CatalogRow) {
    let na = r#"<span class="na">n/a</span>"#;
    let _ = write!(
        b,
        r#"<tr data-node="{id}"><td class="name"><span class="bar {kind}"></span><a class="mono" href="{href}" title="{id}">{name}</a></td><td>{type_label}</td><td>{layer}</td><td>{mat}</td><td>{conf}</td><td>{pill}</td><td><span class="placeholder faint" title="Health signals come later (#117)">[n]</span></td><td class="last">{last}</td></tr>"#,
        id = attr(&row.id),
        kind = attr(&row.resource_type),
        href = attr(&row.href),
        name = text(&row.name),
        type_label = text(&row.type_label),
        layer = row
            .layer
            .as_deref()
            .map_or_else(|| na.to_owned(), |l| text(l).into_owned()),
        mat = row
            .materialization
            .as_deref()
            .map_or_else(|| na.to_owned(), |m| text(m).into_owned()),
        conf = confidence(row.lineage),
        pill = decision_pill(&row.decision),
        last = last_built(row.last_build.as_ref()),
    );
}

fn legend(b: &mut String, view: &CatalogView) {
    b.push_str(
        r#"<div class="cat-legend"><span><span class="pill build">BUILD</span>will run</span><span><span class="pill reuse">REUSE</span>unchanged; its relation isn't checked by this plan (it is when a run starts)</span><span><span class="pill never_built">NEVER BUILT</span>no recorded build: builds</span><span><span class="pill unknown">UNKNOWN</span>not known: see why</span><span class="right">Last built: snapshot · run</span></div>"#,
    );
    let layer = view
        .layer_source
        .as_deref()
        .unwrap_or("No layers: the project doesn't say.");
    let _ = write!(
        b,
        r#"<div class="cat-legend"><span><b>Layer*</b></span><span>{}</span></div><div class="cat-legend"><span><b>Health</b></span><span><span class="placeholder faint">[n]</span>health signals from run, test and freshness evidence come later (#117)</span></div>"#,
        text(layer)
    );
}
