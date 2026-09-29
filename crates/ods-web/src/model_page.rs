//! A model page's HTML (#313): one node, with a tab each for its overview, code,
//! columns, lineage, State and tests, rendered on the server from [`ModelView`]. Tabs
//! are links (`?tab=code`), so every tab works without script and can be linked to.
//!
//! The page lives at `catalog/<id>`, one level below the dashboard's root, so its links
//! start with `../`.

use std::fmt::Write as _;

use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};

use crate::catalog::{ColumnView, Decision, ModelView, NodeLink, TestKind, TypeSource};
use crate::catalog_page::{CSS, confidence, decision_pill, last_built, pill};
use crate::dashboard::ShellView;

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
    let title = format!("Catalog / {}", view.name);
    crate::home::shell_at(shell, &title, &b, generation, ROOT, CSS)
}

/// No such node.
pub(crate) fn not_found_page(shell: &ShellView, id: &str, generation: u64) -> String {
    let body = format!(
        r#"<div class="content"><section class="card empty" data-state="not_found"><h2>No such node</h2><p>The project has no node <code>{id}</code>. It may have been renamed or removed since the link was made.</p><p><a href="{ROOT}catalog">Back to the Catalog</a></p></section></div>"#,
        id = text(id),
    );
    crate::home::shell_at(shell, "Not found", &body, generation, ROOT, CSS)
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
        why = attr(&view.decision.summary),
        detail = text(&detail),
    )
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
        r#"<span class="mh-actions"><a class="btn" href="{ROOT}{lineage}">View lineage</a><button class="btn" id="copylink" type="button" hidden>Copy link</button></span></div><div class="mh-meta">"#,
        lineage = attr(&view.links.lineage),
    );
    if let Some(relation) = &view.relation {
        let _ = write!(
            b,
            r#"<span>Relation <code class="fg">{}</code></span>"#,
            text(relation)
        );
    }
    let build = match &view.last_build {
        Some(build) => format!(
            r#"<span class="fg">{snap}run <span class="mono" title="{run}">{short}</span> · <time datetime="{at}" data-relative>{at}</time></span>"#,
            snap = build
                .snapshot
                .map_or_else(String::new, |s| format!("snapshot {s} · ")),
            run = attr(&build.run_id),
            short = text(&build.short_run_id),
            at = attr(&build.built_at.to_string()),
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
        r#"<section class="card" aria-label="State"><h2>State</h2><div class="kv"><span class="muted">Next run</span><span class="next {class}">{next}</span></div><div class="kv"><span class="muted">Because</span><span>{why}</span></div><div class="kv"><span class="muted">Last build</span><span>{last}</span></div><a href="{ROOT}{href}">Why this decision</a></section>"#,
        next = text(next),
        why = text(&view.decision.summary),
        last = last_built(view.last_build.as_ref()),
        href = attr(&view.links.why),
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
            r#"<span class="mono" title="From the warehouse catalog">{}</span>"#,
            text(t)
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
        let tests = column.tests.join(" · ");
        let from = if column.upstream.is_empty() {
            String::new()
        } else {
            format!("← {}", column.upstream.join(", "))
        };
        if full {
            let _ = write!(
                b,
                r#"<tr data-column="{name_attr}"><td class="mono">{name}</td><td>{ty}</td><td class="muted">{description}</td><td>{tests}</td><td>{constraints}</td><td class="muted">{from}</td></tr>"#,
                name_attr = attr(&column.name),
                name = text(&column.name),
                ty = type_cell(column),
                tests = text(&tests),
                constraints = text(&column.constraints.join(" · ")),
                from = text(&from),
            );
        } else {
            let meta = [tests, from]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            let _ = write!(
                b,
                r#"<tr data-column="{name_attr}"><td class="mono">{name}</td><td class="muted">{description}</td><td class="muted">{meta}</td></tr>"#,
                name_attr = attr(&column.name),
                name = text(&column.name),
                meta = text(&meta),
            );
        }
    }
    b.push_str("</tbody></table>");
    if full && view.columns.iter().any(|c| c.data_type.is_none()) {
        b.push_str(r#"<p class="f-note">A type is only shown when the artifacts record it: from the warehouse catalog, or declared in the project (marked declared).</p>"#);
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
    match &view.code.compiled {
        Some(compiled) => {
            let _ = write!(
                b,
                r#"<section class="card" aria-label="Compiled code"><div class="card-head"><h2>Compiled</h2><span class="muted">as compiled for the target</span></div><pre class="code"><code>{}</code></pre></section>"#,
                text(compiled),
            );
        }
        None if view.code.raw.is_some() => b.push_str(r#"<section class="card" aria-label="Compiled code"><h2>Compiled</h2><p class="muted">Not in the artifacts: they were written before the code was compiled.</p></section>"#),
        None => {}
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
            "From the plan against snapshot {snapshot}, made for this request (ods state plan)."
        ),
        (None, None) => "No plan: nothing is recorded to compare with.".to_owned(),
    };
    let _ = write!(
        b,
        r#"<p class="f-note">{basis}</p><a href="{ROOT}{href}">Why this decision</a></section>"#,
        basis = text(&basis),
        href = attr(&view.links.why),
    );
    b.push_str(r#"</div><div class="stack"><section class="card" aria-label="Last build"><h2>Last successful build</h2>"#);
    match &view.last_build {
        Some(build) => {
            let _ = write!(
                b,
                r#"<div class="kv"><span class="muted">Snapshot</span><span>{snap}</span></div><div class="kv"><span class="muted">Run</span><span class="mono" title="{run}">{short}</span></div><div class="kv"><span class="muted">Finished</span><time datetime="{at}">{at}</time></div>"#,
                snap = build
                    .snapshot
                    .map_or_else(|| "unknown".to_owned(), |s| s.to_string()),
                run = attr(&build.run_id),
                short = text(&build.short_run_id),
                at = attr(&build.built_at.to_string()),
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
    match &view.checks_passed {
        Some(passed) => {
            let _ = write!(
                b,
                r#"<p class="checks">All of its checks last passed on its current build in run <span class="mono" title="{run}">{short}</span>, <time datetime="{at}" data-relative>{at}</time>.{changed}</p>"#,
                run = attr(&passed.run_id),
                short = text(&passed.run_id.chars().take(8).collect::<String>()),
                at = attr(&passed.at.to_string()),
                changed = if passed.checks_changed_since {
                    " Its checks changed since, so that no longer vouches for them."
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
                outcome = test.last_outcome.as_deref().map_or_else(
                    || r#"<span class="muted" title="Outcomes aren't recorded per test yet">not recorded</span>"#.to_owned(),
                    |o| text(o).into_owned()
                ),
            );
        }
        b.push_str("</tbody></table>");
    }
    b.push_str("</section></div>");
}
