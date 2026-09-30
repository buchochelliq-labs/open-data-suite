//! A model page's HTML (#313): one node, with a tab each for its overview, code,
//! columns, lineage, State and tests, rendered on the server from [`ModelView`]. Tabs
//! are links (`?tab=code`), so every tab works without script and can be linked to.
//!
//! The page lives at `catalog/<id>`, one level below the dashboard's root, so its links
//! start with `../`.

use std::fmt::Write as _;

use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};

use ods_sdk::contracts::relation_link::RelationLinkFields;

use crate::catalog::{
    ColumnView, Decision, LINK_TITLE, ModelView, NodeLink, REUSE_CAVEAT, REUSE_RELATION, TestKind,
    TypeSource,
};
use crate::catalog_page::{CSS, confidence, decision_pill, pill, why};
use crate::dashboard::ShellView;
use crate::home::{Frame, framed};
use crate::state_pages::crumbs;

/// The page's links resolve from one level down.
const ROOT: &str = "../";

/// The tabs, in the design's order: key, label, built.
const TABS: [(&str, &str, bool); 8] = [
    ("overview", "Overview", true),
    ("code", "Code", true),
    ("columns", "Columns", true),
    ("lineage", "Lineage", true),
    ("state", "State", true),
    ("tests", "Tests", true),
    ("relationships", "Relationships", false),
    ("usage", "Usage", false),
];

const CUBE: &str = r#"<svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 16V8a2 2 0 0 0-1-1.7l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.7l7 4a2 2 0 0 0 2 0l7-4a2 2 0 0 0 1-1.7z"></path><path d="M3.3 7 12 12l8.7-5M12 22V12"></path></svg>"#;

/// Filters the columns as you type, and copies the page's address.
const SCRIPT: &str = r#"<script>(function(){var q=document.getElementById("colfilter");if(q){q.hidden=false;q.addEventListener("input",function(){var v=q.value.toLowerCase();document.querySelectorAll("[data-column]").forEach(function(r){r.hidden=v&&r.getAttribute("data-column").toLowerCase().indexOf(v)<0;});});}var c=document.getElementById("copylink");if(c&&navigator.clipboard){c.hidden=false;c.addEventListener("click",function(){navigator.clipboard.writeText(location.href).then(function(){c.textContent="Copied";setTimeout(function(){c.textContent="Copy link";},1500);});});}})();</script>"#;

/// The tab shown for a `?tab=` value: a built tab, else Overview.
pub(crate) fn tab(requested: Option<&str>) -> &'static str {
    TABS.iter()
        .find(|(key, _, built)| *built && Some(*key) == requested)
        .map_or("overview", |(key, _, _)| key)
}

/// The page of `view`, on `tab`, inside the shell.
pub(crate) fn model_page(
    shell: &ShellView,
    view: &ModelView,
    tab: &str,
    generation: u64,
) -> String {
    let mut b = String::with_capacity(32 * 1024);
    head(&mut b, view, tab);
    b.push_str(r#"<div class="model-body">"#);
    match tab {
        "code" => code(&mut b, view),
        "columns" => {
            b.push_str(r#"<div class="stack">"#);
            columns(&mut b, view, true);
            b.push_str("</div>");
        }
        "lineage" => lineage(&mut b, view),
        "state" => state(&mut b, view),
        "tests" => tests(&mut b, view),
        _ => overview(&mut b, view),
    }
    b.push_str("</div>");
    b.push_str(SCRIPT);
    let title = format!("{} · Catalog", view.name);
    let frame = Frame {
        title: &title,
        crumbs: Some(crumbs(
            &[
                ("Catalog", Some("../catalog")),
                ("Models", Some("../catalog")),
                (&view.name, None),
            ],
            true,
        )),
        root: ROOT,
        status: None,
        sub: Some("models"),
        search: true,
        css: CSS,
        js: "",
    };
    framed(shell, &frame, &b, generation)
}

/// No such node.
pub(crate) fn not_found_page(shell: &ShellView, id: &str, generation: u64) -> String {
    let body = format!(
        r#"<div class="content"><section class="card empty" data-state="not_found"><h2>No such node</h2><p>The project has no node <code>{id}</code>. It may have been renamed or removed since the link was made.</p><p><a href="{ROOT}catalog">Back to the Catalog</a></p></section></div>"#,
        id = text(id),
    );
    let frame = Frame {
        title: "Not found",
        crumbs: Some(crumbs(
            &[("Catalog", Some("../catalog")), ("Not found", None)],
            false,
        )),
        root: ROOT,
        status: None,
        sub: Some("models"),
        search: true,
        css: CSS,
        js: "",
    };
    framed(shell, &frame, &body, generation)
}

/// `3 h ago`, with the exact time in the tooltip (the script fills in the relative
/// time; without it, the exact time shows).
fn when(at: &str) -> String {
    format!(
        r#"<time datetime="{at}" title="{at}" data-relative>{at}</time>"#,
        at = attr(at)
    )
}

/// The link to the Plan page's Why panel for this node.
fn why_link(view: &ModelView) -> String {
    format!(
        r#"<a href="{ROOT}{}">Why this decision</a>"#,
        attr(&view.links.why)
    )
}

/// Whether the node's relation was checked, for the decision it has.
fn relation(view: &ModelView) -> &'static str {
    match view.decision.decision {
        Decision::Reuse => REUSE_RELATION,
        Decision::Build | Decision::NeverBuilt => "not needed: it builds",
        Decision::Unknown => "unknown",
    }
}

/// `←&nbsp;orders.amount`, marked when inferred.
fn computed_from(column: &ColumnView) -> String {
    if column.upstream.is_empty() {
        return String::new();
    }
    let inputs = column
        .upstream
        .iter()
        .map(|u| format!("←&nbsp;{}", text(u)))
        .collect::<Vec<_>>()
        .join(", ");
    if column.upstream_inferred {
        format!(
            r#"{inputs} <span class="inferred" title="Not parsed from the code: inferred">inferred</span>"#
        )
    } else {
        inputs
    }
}

fn link(node: &NodeLink) -> String {
    match &node.href {
        Some(href) => format!(
            r#"<a class="mono" href="{ROOT}{href}" title="{id}">{name}</a>"#,
            href = attr(href),
            id = attr(&node.id),
            name = text(&node.name),
        ),
        None => format!(
            r#"<span class="mono" title="{id}">{name}</span>"#,
            id = attr(&node.id),
            name = text(&node.name),
        ),
    }
}

/// The big pill: `BUILD next run · code changed`.
fn headline(view: &ModelView) -> String {
    let (class, label) = pill(view.decision.decision);
    let detail = match (view.decision.decision, view.decision.reasons.first()) {
        (Decision::Unknown, _) => "next run unknown".to_owned(),
        (Decision::NeverBuilt, _) => "builds next run".to_owned(),
        (_, Some(reason)) => format!("next run · {}", reason.label),
        (_, None) => "next run".to_owned(),
    };
    format!(
        r#"<span class="pill big {class}" title="{why}">{label} {detail}</span>"#,
        why = attr(&why(&view.decision)),
        detail = text(&detail),
    )
}

/// The link to where the relation is expected to be in the warehouse's UI (#329): it
/// opens in a new tab, and never passes this page's address on. `class` styles it,
/// `shown` gives its text from the provider's label, and `note` says what the link is.
pub(crate) fn warehouse_link(
    fields: &RelationLinkFields,
    class: &str,
    shown: &dyn Fn(&str) -> String,
    note: &str,
) -> String {
    let Some(url) = &fields.relation_url else {
        return String::new();
    };
    // Only ever an https:// link (the contract's rule); anything else isn't shown.
    if !url.starts_with("https://") {
        return String::new();
    }
    let label = fields
        .relation_url_label
        .as_deref()
        .unwrap_or("Open in warehouse");
    format!(
        r#"<a class="{class}" href="{url}" target="_blank" rel="noopener noreferrer" title="{title}" aria-label="{label_attr} (expected location, opens in a new tab)" data-relation-link>{shown}</a>"#,
        class = attr(class),
        url = attr(url),
        title = attr(&format!("{label}. {note}")),
        label_attr = attr(label),
        shown = text(&shown(label)),
    )
}

/// Why there is no warehouse link, as a sentence; empty when there is one or nobody
/// asked.
pub(crate) fn no_link_reason(fields: &RelationLinkFields) -> String {
    if fields.relation_url.is_some() {
        return String::new();
    }
    fields
        .relation_url_unavailable
        .as_deref()
        .map(|why| {
            let mut chars = why.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .unwrap_or_default()
}

/// `text` escaped, with each `` `quoted` `` part as code, as reasons write names.
pub(crate) fn with_code(value: &str) -> String {
    value
        .split('`')
        .enumerate()
        .map(|(i, part)| {
            if i % 2 == 1 {
                format!("<code>{}</code>", text(part))
            } else {
                text(part).into_owned()
            }
        })
        .collect()
}

fn head(b: &mut String, view: &ModelView, current: &str) {
    let _ = write!(
        b,
        r#"<div class="model-head"><div class="mh-row"><span class="cube">{CUBE}</span><h1 class="mono" title="{id}">{name}</h1>{pill}"#,
        id = attr(&view.id),
        name = text(&view.name),
        pill = headline(view),
    );
    let mut chips: Vec<(String, &str)> = vec![(view.type_label.clone(), "Type")];
    if let Some(m) = &view.materialization {
        chips.push((m.clone(), "Materialization"));
    }
    if let Some(l) = &view.layer {
        chips.push((l.clone(), view.layer_source.as_deref().unwrap_or("Layer")));
    }
    for tag in &view.tags {
        chips.push((format!("#{tag}"), "Tag"));
    }
    for (chip, title) in chips {
        let _ = write!(
            b,
            r#"<span class="tagchip" title="{title}">{chip}</span>"#,
            title = attr(title),
            chip = text(&chip),
        );
    }
    let _ = write!(
        b,
        r#"<span class="mh-actions">{warehouse}<a class="btn" href="{ROOT}{lineage}">View lineage</a><button class="btn" id="copylink" type="button" hidden>Copy link</button></span></div><div class="mh-meta">"#,
        // The label is the provider's; the arrow says the link leaves ODS.
        warehouse = warehouse_link(
            &view.relation_link,
            "btn",
            &|label| format!("{label} ↗"),
            LINK_TITLE
        ),
        lineage = attr(&view.links.lineage),
    );
    if let Some(relation) = &view.relation {
        let expected = if view.relation_link.relation_url.is_some() {
            format!(
                r#" <span class="muted" title="{}">expected location</span>"#,
                attr(LINK_TITLE)
            )
        } else {
            String::new()
        };
        let _ = write!(
            b,
            r#"<span>Relation <code class="fg">{}</code>{expected}</span>"#,
            text(relation)
        );
    }
    let why = no_link_reason(&view.relation_link);
    if !why.is_empty() {
        let _ = write!(
            b,
            r#"<span class="nolink" data-state="no_relation_link">{}</span>"#,
            with_code(&why)
        );
    }
    let build = match &view.last_build {
        Some(build) => format!(
            r#"<span class="fg">{snap}run <span class="mono" title="{run}">{short}</span> · {at}</span>"#,
            snap = build
                .snapshot
                .map_or_else(String::new, |s| format!("snapshot {s} · ")),
            run = attr(&build.run_id),
            short = text(&build.short_run_id),
            at = when(&build.built_at.to_string()),
        ),
        None => r#"<span class="fg">never</span>"#.to_owned(),
    };
    let _ = write!(b, "<span>Last successful build {build}</span>");
    if let Some(file) = &view.file {
        let _ = write!(
            b,
            r#"<span>File <code class="fg">{}</code></span>"#,
            text(file)
        );
    }
    b.push_str(r#"</div><nav class="tabs" aria-label="Sections of this node">"#);
    for (key, label, built) in TABS {
        if built {
            let _ = write!(
                b,
                r#"<a href="?tab={key}"{current} data-tab="{key}">{label}</a>"#,
                current = if key == current {
                    r#" aria-current="page""#
                } else {
                    ""
                },
            );
        } else {
            let _ = write!(
                b,
                r#"<span class="tab-planned" title="{label} is planned" data-tab="{key}">{label}<span class="chip">Planned</span></span>"#
            );
        }
    }
    b.push_str("</nav></div>");
}

fn overview(b: &mut String, view: &ModelView) {
    b.push_str(r#"<div class="model-grid"><div class="stack">"#);
    b.push_str(r#"<section class="card" aria-label="Description"><h2>Description</h2>"#);
    match &view.description {
        Some(d) if !d.trim().is_empty() => {
            let _ = write!(b, r#"<p class="desc">{}</p>"#, text(d));
        }
        _ => b.push_str(r#"<p class="desc muted">No description.</p>"#),
    }
    b.push_str("</section>");
    columns(b, view, false);
    b.push_str(r#"</div><div class="stack">"#);
    state_card(b, view);
    lineage_card(b, view);
    b.push_str("</div></div>");
}

fn state_card(b: &mut String, view: &ModelView) {
    let (class, _) = pill(view.decision.decision);
    let next = match view.decision.decision {
        Decision::Build => "Build",
        Decision::Reuse => "Reuse",
        Decision::NeverBuilt => "Build (never built)",
        Decision::Unknown => "Unknown",
    };
    let _ = write!(
        b,
        r#"<section class="card" aria-label="State"><h2>State</h2><div class="kv"><span class="muted">Next run</span><span class="next {class}">{next}</span></div><div class="kv"><span class="muted">Because</span><span>{because}</span></div><div class="kv"><span class="muted">Relation</span><span>{relation}</span></div>{link}</section>"#,
        next = text(next),
        because = text(&view.decision.summary),
        relation = text(relation(view)),
        link = why_link(view),
    );
}

fn lineage_card(b: &mut String, view: &ModelView) {
    b.push_str(r#"<section class="card" aria-label="Lineage"><h2>Lineage</h2><div class="mini">"#);
    let list = |b: &mut String, nodes: &[NodeLink], none: &str| {
        b.push_str(r#"<span class="mini-col">"#);
        if nodes.is_empty() {
            let _ = write!(b, r#"<span class="muted mini-none">{}</span>"#, text(none));
        }
        for node in nodes.iter().take(4) {
            let _ = write!(b, r#"<span class="mini-node">{}</span>"#, link(node));
        }
        if nodes.len() > 4 {
            let _ = write!(b, r#"<span class="muted">+{} more</span>"#, nodes.len() - 4);
        }
        b.push_str("</span>");
    };
    list(b, &view.upstream, "no parents");
    b.push_str(r#"<span class="arrow">→</span>"#);
    let _ = write!(
        b,
        r#"<span class="mini-node self mono">{}</span><span class="arrow">→</span>"#,
        text(&view.name)
    );
    list(b, &view.downstream, "no children");
    let _ = write!(
        b,
        r#"</div><a href="{ROOT}{href}">Expand</a></section>"#,
        href = attr(&view.links.lineage)
    );
}

fn type_cell(column: &ColumnView) -> String {
    match (&column.data_type, column.type_source) {
        (Some(t), Some(TypeSource::Declared)) => format!(
            r#"<span class="mono" title="Declared in the project; not checked against the warehouse">{} <span class="muted">(declared)</span></span>"#,
            text(t)
        ),
        (Some(t), _) => format!(
            r#"<span class="mono" title="{title}">{}</span>"#,
            text(t),
            title = attr(&column.type_as_of.as_ref().map_or_else(
                || "From the warehouse catalog, as of when it was written".to_owned(),
                |at| format!("From the warehouse catalog, as of {at}")
            )),
        ),
        (None, _) => r#"<span class="unknown-type" title="Not in the artifacts: generate the warehouse catalog, or declare a data type">unknown</span>"#.to_owned(),
    }
}

/// The columns: compact on the overview (name, description, tests · lineage), in full
/// on their own tab.
fn columns(b: &mut String, view: &ModelView, full: bool) {
    let _ = write!(
        b,
        r#"<section class="card" aria-label="Columns"><div class="card-head"><h2>Columns <span class="muted count">{n}</span></h2><input id="colfilter" class="colfilter" aria-label="Filter columns" placeholder="Filter columns" autocomplete="off" hidden></div>"#,
        n = view.columns.len(),
    );
    if view.columns.is_empty() {
        b.push_str(r#"<p class="muted">No columns are known: the artifacts list none for this node.</p></section>"#);
        return;
    }
    if full {
        b.push_str(r#"<table class="cols full"><thead><tr><th>Column</th><th>Type</th><th>Description</th><th>Tests</th><th>Constraints</th><th>Computed from</th></tr></thead><tbody>"#);
    } else {
        b.push_str(r#"<table class="cols"><thead><tr><th>Column</th><th>Description</th><th>Tests · lineage</th></tr></thead><tbody>"#);
    }
    for column in &view.columns {
        let description = column
            .description
            .as_deref()
            .filter(|d| !d.trim().is_empty())
            .map_or_else(String::new, |d| text(d).into_owned());
        let tests = text(&column.tests.join(" · ")).into_owned();
        let from = computed_from(column);
        let name = if column.possibly_stale {
            format!(
                r#"{} <span class="stale" title="Only the warehouse catalog lists it; the project and the lineage of its current code don't, so it may have been dropped since">possibly dropped</span>"#,
                text(&column.name)
            )
        } else {
            text(&column.name).into_owned()
        };
        if full {
            let _ = write!(
                b,
                r#"<tr data-column="{name_attr}"><td class="mono">{name}</td><td>{ty}</td><td class="muted">{description}</td><td>{tests}</td><td>{constraints}</td><td class="muted">{from}</td></tr>"#,
                name_attr = attr(&column.name),
                ty = type_cell(column),
                constraints = text(&column.constraints.join(" · ")),
            );
        } else {
            // Tests, then lineage, each on its own line.
            let meta = [tests, from].into_iter().filter(|s| !s.is_empty()).fold(
                String::new(),
                |mut out, s| {
                    let _ = write!(out, "<span>{s}</span>");
                    out
                },
            );
            let _ = write!(
                b,
                r#"<tr data-column="{name_attr}"><td class="mono">{name}</td><td class="muted">{description}</td><td class="muted meta">{meta}</td></tr>"#,
                name_attr = attr(&column.name),
            );
        }
    }
    b.push_str("</tbody></table>");
    if full {
        let as_of = view
            .columns
            .iter()
            .find_map(|c| c.type_as_of.as_deref())
            .map_or_else(String::new, |at| {
                format!(" Warehouse types are as of its catalog, written {at}.")
            });
        let _ = write!(
            b,
            r#"<p class="f-note">A type is only shown when the artifacts record it: from the warehouse catalog, or declared in the project (marked declared).{}</p>"#,
            text(&as_of)
        );
    }
    if view
        .columns
        .iter()
        .any(|c| c.upstream_inferred && !c.upstream.is_empty())
    {
        b.push_str(r#"<p class="f-note">Column lineage marked inferred wasn't parsed from the code; it may be incomplete or wrong.</p>"#);
    }
    b.push_str("</section>");
}

fn code(b: &mut String, view: &ModelView) {
    let language = view.code.language.as_deref().unwrap_or("code");
    b.push_str(r#"<div class="stack">"#);
    match &view.code.raw {
        Some(raw) => {
            let _ = write!(
                b,
                r#"<section class="card" aria-label="Code"><div class="card-head"><h2>Code</h2><span class="muted">{language}, as written</span></div><pre class="code"><code>{raw}</code></pre></section>"#,
                language = text(language),
                raw = text(raw),
            );
        }
        None => b.push_str(r#"<section class="card" aria-label="Code"><h2>Code</h2><p class="muted">The artifacts carry no code for this node (a seed is loaded from its file).</p></section>"#),
    }
    if view.code.raw.is_some() {
        // Compiled code is never served: it can hold values resolved from the
        // environment or variables, such as credentials (AGENTS rule 9).
        b.push_str(r#"<section class="card" aria-label="Compiled code"><h2>Compiled</h2><p class="muted">Not shown: compiled code can contain resolved secrets (from environment variables, variables or macros). It is in the project's <code>target/compiled/</code> folder on the machine that compiled it.</p></section>"#);
    }
    b.push_str("</div>");
}

fn lineage(b: &mut String, view: &ModelView) {
    b.push_str(r#"<div class="model-grid"><div class="stack">"#);
    for (title, nodes, none) in [
        ("Upstream", &view.upstream, "It reads no other node."),
        ("Downstream", &view.downstream, "No node reads it."),
    ] {
        let _ = write!(
            b,
            r#"<section class="card" aria-label="{title}"><h2>{title} <span class="muted count">{n}</span></h2>"#,
            n = nodes.len(),
        );
        if nodes.is_empty() {
            let _ = write!(b, r#"<p class="muted">{none}</p>"#);
        } else {
            b.push_str(r#"<ul class="nodes">"#);
            for node in nodes {
                let _ = write!(b, "<li>{}</li>", link(node));
            }
            b.push_str("</ul>");
        }
        b.push_str("</section>");
    }
    let _ = write!(
        b,
        r#"</div><div class="stack"><section class="card" aria-label="Column lineage"><h2>Column lineage</h2><div class="kv"><span class="muted">Confidence</span>{conf}</div>"#,
        conf = confidence(view.lineage),
    );
    for note in &view.lineage_notes {
        let _ = write!(b, r#"<p class="f-note">{}</p>"#, text(note));
    }
    let _ = write!(
        b,
        r#"<p class="f-note">Upstream and downstream are the build graph's edges: which node reads which. They are not relationships between tables.</p><a href="{ROOT}{href}">Open in the lineage explorer</a></section></div></div>"#,
        href = attr(&view.links.lineage),
    );
}

fn state(b: &mut String, view: &ModelView) {
    let _ = write!(
        b,
        r#"<div class="model-grid"><div class="stack"><section class="card" aria-label="Next run"><div class="card-head"><h2>Next run</h2>{pill}</div>"#,
        pill = decision_pill(&view.decision),
    );
    if view.decision.reasons.is_empty() {
        let _ = write!(b, "<p>{}</p>", text(&view.decision.summary));
    } else {
        b.push_str(r#"<ol class="reasons">"#);
        for reason in &view.decision.reasons {
            let _ = write!(
                b,
                r#"<li><code title="{code}">{label}</code> {message}</li>"#,
                code = attr(&reason.label.replace(' ', "_")),
                label = text(&reason.label),
                message = text(&reason.message),
            );
        }
        b.push_str("</ol>");
    }
    let basis = match (view.decisions.based_on, &view.decisions.error) {
        (_, Some(error)) => format!("The plan couldn't be made: {error}"),
        (Some(snapshot), None) => format!(
            "From the plan against snapshot {snapshot}, made offline for this request (ods state plan)."
        ),
        (None, None) => "No plan: nothing is recorded to compare with.".to_owned(),
    };
    let _ = write!(
        b,
        r#"<div class="kv"><span class="muted">Relation</span><span>{relation}</span></div><p class="f-note">{basis}</p>{caveat}{link}</section>"#,
        relation = text(relation(view)),
        basis = text(&basis),
        caveat = if view.decision.decision == Decision::Reuse {
            format!(r#"<p class="f-note">{}</p>"#, text(REUSE_CAVEAT))
        } else {
            String::new()
        },
        link = why_link(view),
    );
    b.push_str(r#"</div><div class="stack"><section class="card" aria-label="Last build"><h2>Last successful build</h2>"#);
    match &view.last_build {
        Some(build) => {
            let _ = write!(
                b,
                r#"<div class="kv"><span class="muted">Snapshot</span><span>{snap}</span></div><div class="kv"><span class="muted">Run</span><span class="mono" title="{run}">{short}</span></div><div class="kv"><span class="muted">Finished</span>{at}</div>"#,
                snap = build
                    .snapshot
                    .map_or_else(|| "unknown".to_owned(), |s| s.to_string()),
                run = attr(&build.run_id),
                short = text(&build.short_run_id),
                at = when(&build.built_at.to_string()),
            );
        }
        None => b.push_str(r#"<p class="muted">Never: no successful build is recorded.</p>"#),
    }
    b.push_str("</section></div></div>");
}

fn tests(b: &mut String, view: &ModelView) {
    let _ = write!(
        b,
        r#"<div class="stack"><section class="card" aria-label="Tests"><h2>Tests <span class="muted count">{}</span></h2>"#,
        view.tests.len()
    );
    // The checks changed since they last passed: nothing vouches for any test now.
    let changed = view
        .checks_passed
        .as_ref()
        .filter(|p| p.checks_changed_since)
        .map(|p| p.run_id.chars().take(8).collect::<String>());
    match &view.checks_passed {
        Some(passed) => {
            let _ = write!(
                b,
                r#"<p class="checks">The last build's recorded checks passed together in run <span class="mono" title="{run}">{short}</span>, {at}. Outcomes aren't kept per test yet: a test reads <i>passed</i> when it is one of those checks.{changed}</p>"#,
                run = attr(&passed.run_id),
                short = text(&passed.run_id.chars().take(8).collect::<String>()),
                at = when(&passed.at.to_string()),
                changed = if passed.checks_changed_since {
                    " The checks changed since, so that record no longer vouches for any of them."
                } else {
                    ""
                },
            );
        }
        None if view.tests.is_empty() => {}
        None => b.push_str(r#"<p class="checks muted">No passing run of its checks is recorded for its current build.</p>"#),
    }
    if view.tests.is_empty() {
        b.push_str(r#"<p class="muted">No tests on this node.</p>"#);
    } else {
        b.push_str(r#"<table class="cols full"><thead><tr><th>Test</th><th>Column</th><th>Kind</th><th>Last outcome</th></tr></thead><tbody>"#);
        for test in &view.tests {
            let _ = write!(
                b,
                r#"<tr><td class="mono" title="{id}">{name}</td><td class="mono">{column}</td><td>{kind}</td><td>{outcome}</td></tr>"#,
                id = attr(&test.id),
                name = text(&test.name),
                column = text(test.column.as_deref().unwrap_or("")),
                kind = match test.kind {
                    TestKind::Unit => "unit",
                    _ => "data",
                },
                outcome = test.last_outcome.as_ref().map_or_else(
                    || match &changed {
                        Some(run) => format!(
                            r#"<span class="muted" title="A test was added, removed or edited since the checks last passed">checks changed since run <span class="mono">{}</span>: not recorded</span>"#,
                            text(run)
                        ),
                        None => r#"<span class="muted" title="Not among the checks a recorded build passed">not recorded</span>"#.to_owned(),
                    },
                    |o| format!(
                        r#"<span class="passed" title="Passed with the node's other checks, {at}">{outcome} · run <span class="mono">{short}</span></span>"#,
                        at = attr(&o.at.to_string()),
                        outcome = text(o.outcome),
                        short = text(&o.run_id.chars().take(8).collect::<String>()),
                    )
                ),
            );
        }
        b.push_str("</tbody></table>");
    }
    b.push_str("</section></div>");
}
