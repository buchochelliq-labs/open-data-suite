//! The dashboard's HTML: the shell and Home, rendered on the server from the view
//! models in [`crate::dashboard`], with the stylesheet and script inlined (ADR-0009).

use std::fmt::Write as _;

use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};

use crate::dashboard::{
    AttentionKind, HomeView, ModuleState, NavSection, SectionStatus, ShellView, StateStatus,
};

const CSS: &str = include_str!("../assets/dashboard.css");
const JS: &str = include_str!("../assets/dashboard.js");

/// What a recorded run's outcome does and doesn't say.
const OUTCOME_NOTE: &str = "Its successful builds were recorded as the new state. \
    Whether other nodes failed isn't stored yet.";
/// What "kept earlier build" covers.
const KEPT_NOTE: &str = "Not rebuilt by this run: reused, not selected, or failed. \
    The snapshot keeps the last good build either way.";

/// Inline icons (Lucide-style strokes, as in the design boards), by section key.
fn icon(key: &str) -> &'static str {
    match key {
        "home" => {
            r#"<rect x="3" y="3" width="7" height="9" rx="1"></rect><rect x="14" y="3" width="7" height="5" rx="1"></rect><rect x="14" y="12" width="7" height="9" rx="1"></rect><rect x="3" y="16" width="7" height="5" rx="1"></rect>"#
        }
        "catalog" => {
            r#"<path d="M21 12h-8M21 6H8M21 18h-8M3 6v4c0 1.1.9 2 2 2h3M3 10v6c0 1.1.9 2 2 2h3"></path>"#
        }
        "lineage" => {
            r#"<rect x="3" y="3" width="8" height="8" rx="2"></rect><path d="M7 11v4a2 2 0 0 0 2 2h4"></path><rect x="13" y="13" width="8" height="8" rx="2"></rect>"#
        }
        "state" => {
            r#"<path d="M3 12a9 9 0 0 1 15-6.7L21 8M21 3v5h-5M21 12a9 9 0 0 1-15 6.7L3 16M3 21v-5h5"></path>"#
        }
        "erd" => {
            r#"<circle cx="8" cy="15" r="4"></circle><path d="M10.9 12.1 20 3M17 6l3 3M15 8l2 2"></path>"#
        }
        "usage" => r#"<path d="M22 12h-4l-3 9L9 3l-3 9H2"></path>"#,
        "ci" => {
            r#"<circle cx="6" cy="6" r="3"></circle><circle cx="18" cy="18" r="3"></circle><path d="M6 9v12M18 15V9a3 3 0 0 0-3-3h-4M13 3 10 6l3 3"></path>"#
        }
        "agent" => {
            r#"<rect x="4" y="8" width="16" height="12" rx="2"></rect><path d="M12 8V4M9 13v2M15 13v2"></path>"#
        }
        "settings" => {
            r#"<circle cx="12" cy="12" r="3"></circle><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1A1.7 1.7 0 0 0 9 19.4a1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1A1.7 1.7 0 0 0 9 4.6 1.7 1.7 0 0 0 10 3.1V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"></path>"#
        }
        _ => "",
    }
}

fn svg(paths: &str, size: u32) -> String {
    format!(
        r#"<svg width="{size}" height="{size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{paths}</svg>"#
    )
}

const LOGO: &str = r#"<svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="6" cy="6" r="3"></circle><circle cx="18" cy="18" r="3"></circle><circle cx="18" cy="6" r="3"></circle><path d="M9 6h6M18 9v6M8.2 8.2l7.6 7.6"></path></svg>"#;
const SEARCH: &str = r#"<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7"></circle><path d="m21 21-4.3-4.3"></path></svg>"#;

fn nav_item(out: &mut String, section: &NavSection) {
    let icon = svg(icon(section.key), 18);
    let label = format!(r#"<span class="label">{}</span>"#, text(section.label));
    match (section.status, section.href) {
        (SectionStatus::Planned, _) | (_, None) => {
            let _ = write!(
                out,
                r#"<span class="planned" title="{note}" data-section="{key}">{icon}{label}<span class="chip">Planned</span></span>"#,
                note = attr(section.note.unwrap_or("Planned: not built yet")),
                key = attr(section.key),
            );
        }
        (status, Some(href)) => {
            let current = if status == SectionStatus::Current {
                r#" aria-current="page""#
            } else {
                ""
            };
            let _ = write!(
                out,
                r#"<a href="{href}"{current} data-section="{key}">{icon}{label}</a>"#,
                href = attr(href),
                key = attr(section.key),
            );
        }
    }
}

/// The whole page: the shell around `body`, titled `title`.
fn shell(shell: &ShellView, title: &str, body: &str, generation: u64) -> String {
    let mut out = String::with_capacity(32 * 1024);
    let target = match &shell.target.kind {
        Some(kind) => format!("{} · {}", shell.target.name, kind),
        None => shell.target.name.clone(),
    };
    let dot = if shell.target.recorded {
        "dot"
    } else {
        "dot none"
    };
    let _ = write!(
        out,
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="ods-generation" content="{generation}">
<title>{title} · {project} · ODS</title>
<style>{fonts}{CSS}</style>
</head>
<body>
<div class="app">
<nav class="side" aria-label="Main">
<div class="brand">{LOGO}<span>OpenDataSuite</span></div>
<div class="pickers">
<label for="proj">Project</label>
<button id="proj" class="picker" type="button" aria-disabled="true" title="Switching projects comes later"><span>{project}</span><span class="caret">▾</span></button>
<label for="tgt">Target</label>
<button id="tgt" class="picker" type="button" aria-disabled="true" title="Switching targets comes later"><span><span class="{dot}"></span>{target}</span><span class="caret">▾</span></button>
</div>
<div class="sections">"#,
        title = text(title),
        project = text(&shell.project),
        target = text(&target),
        fonts = crate::fonts::font_faces(),
    );
    let (bottom, main): (Vec<_>, Vec<_>) = shell.sections.iter().partition(|s| s.key == "settings");
    for section in main {
        nav_item(&mut out, section);
    }
    out.push_str("</div>\n<div class=\"bottom\">");
    for section in bottom {
        nav_item(&mut out, section);
    }
    let snapshot = match &shell.snapshot {
        Some(s) => format!(
            r#"<span class="pill-snap" title="The latest recorded state"><span class="dot"></span>snapshot {id} · <time datetime="{at}" data-relative>{at}</time></span>"#,
            id = s.id,
            at = attr(&s.recorded_at.to_string()),
        ),
        None => r#"<span class="pill-snap"><span class="dot none"></span>no snapshot yet</span>"#
            .to_owned(),
    };
    let _ = write!(
        out,
        r#"<span class="badge-local" title="Served from this machine; nothing here writes configuration or state">Local · read-only</span></div>
</nav>
<main>
<header class="top">
<span class="crumb">{project}</span><span class="crumb-sep">/</span><span class="crumb-here">{title}</span>
<label class="search" title="Opens the lineage explorer's search; selectors such as +orders are planned">{SEARCH}<input id="search" aria-label="Search models and columns" placeholder="Search models and columns" autocomplete="off"><kbd>/</kbd></label>
{snapshot}
</header>
{body}
</main>
</div>
<script>{JS}</script>
</body>
</html>
"#,
        project = text(&shell.project),
        title = text(title),
    );
    out
}

/// Home, inside the shell. `generation` lets the page notice reloads.
pub(crate) fn home_page(shell_view: &ShellView, home: &HomeView, generation: u64) -> String {
    let mut b = String::with_capacity(16 * 1024);
    b.push_str(r#"<div class="content">"#);
    title_row(&mut b, home);
    tiles(&mut b, home);
    b.push_str(r#"<div class="grid3">"#);
    runs(&mut b, home);
    attention(&mut b, home);
    b.push_str("</div>");
    panels(&mut b, home);
    b.push_str("</div>");
    shell(shell_view, "Home", &b, generation)
}

/// The heading and the last run.
fn title_row(b: &mut String, home: &HomeView) {
    b.push_str(r#"<div class="title-row"><h1>Project health</h1>"#);
    if let Some(run) = &home.last_run {
        let _ = write!(
            b,
            r#"<span class="muted">Last run: <span class="mono" title="run {run_id}">{short}</span> · snapshot {snap} · <span title="{note}">recorded</span></span>"#,
            note = attr(OUTCOME_NOTE),
            run_id = attr(&run.run_id),
            short = text(&run.short_run_id),
            snap = run.snapshot,
        );
    }
    b.push_str("</div>");
}

/// The four tiles.
fn tiles(b: &mut String, home: &HomeView) {
    // Tiles.
    b.push_str(r#"<div class="grid4">"#);
    for tile in &home.tiles {
        let (value, missing) = tile
            .value
            .map_or_else(|| ("–".to_owned(), " missing"), |v| (v.to_string(), ""));
        let _ = write!(
            b,
            r#"<section class="card tile" data-tile="{key}"><span class="label">{label}</span><span class="value{missing}">{value}</span><span class="note">{note}</span></section>"#,
            key = attr(tile.key),
            label = text(tile.label),
            note = text(&tile.note),
        );
    }
    b.push_str("</div>");
}

/// Recent runs, or what to do when there are none.
fn runs(b: &mut String, home: &HomeView) {
    // Recent runs (or the empty state) and needs attention.
    if let Some(empty) = &home.empty {
        let _ = write!(
            b,
            r#"<section class="card empty span2" data-state="{state}"><h2>{title}</h2><p>{message}</p>"#,
            state = match home.state {
                StateStatus::NoStore => "no_store",
                StateStatus::NoRuns => "no_runs",
                StateStatus::Unreadable => "unreadable",
                StateStatus::ProjectUnreadable => "project_unreadable",
                StateStatus::Recorded => "recorded",
            },
            title = text(&empty.title),
            message = text(&empty.message),
        );
        if let Some(store) = &home.store {
            let _ = write!(
                b,
                r#"<p class="store">Store: <code title="{full}">{shown}</code></p>"#,
                full = attr(&store.full),
                shown = text(&store.shown),
            );
        }
        b.push_str(r#"<div class="cmds">"#);
        for command in &empty.commands {
            let _ = write!(
                b,
                r#"<div class="cmd"><code>{c}</code><span>{d}</span></div>"#,
                c = text(&command.command),
                d = text(&command.does),
            );
        }
        b.push_str("</div></section>");
    } else {
        b.push_str(
            r#"<section class="card gap12 span2"><div class="card-head"><h2>Recent runs</h2><span class="soon" title="The Runs page is planned">All runs<span class="chip">Planned</span></span></div><table class="runs"><thead><tr><th>Snapshot</th><th>Command</th><th>Built · kept earlier build</th><th>Outcome</th></tr></thead><tbody>"#,
        );
        for run in &home.runs {
            let all = run.built + run.kept;
            let built_pct = (run.built * 100).checked_div(all).unwrap_or(0);
            let counts = if run.kept == 0 {
                format!("{} built", run.built)
            } else {
                format!("{} built · {} kept", run.built, run.kept)
            };
            let command = run.command.as_deref().map_or_else(
                || r#"<span class="placeholder" title="Runs don't record their command yet">—</span>"#.to_owned(),
                |c| text(c).into_owned(),
            );
            let _ = write!(
                b,
                r#"<tr><td class="mono" title="run {run_id}">{snap} · {short}</td><td>{command}</td><td><span class="built"><span class="bar" title="{counts}"><span class="b" style="width:{built_pct}%"></span><span class="r" style="width:{reuse_pct}%"></span></span>{counts}</span></td><td class="outcome" title="{outcome_note}">{outcome}</td></tr>"#,
                run_id = attr(&run.run_id),
                snap = run.snapshot,
                short = text(&run.short_run_id),
                counts = text(&counts),
                reuse_pct = if all == 0 { 0 } else { 100 - built_pct },
                outcome = text(run.outcome),
                outcome_note = attr(OUTCOME_NOTE),
            );
        }
        let _ = write!(
            b,
            r#"</tbody></table><div class="legend"><span><span class="sw" style="background:var(--build)"></span>Built</span><span title="{note}"><span class="sw" style="background:var(--reuse)"></span>Kept earlier build</span></div></section>"#,
            note = attr(KEPT_NOTE)
        );
    }
}

/// Needs attention.
fn attention(b: &mut String, home: &HomeView) {
    b.push_str(
        r#"<section class="card gap12" aria-label="Needs attention"><h2>Needs attention</h2>"#,
    );
    // The all-clear rests on the plan building nothing, never on the list being empty:
    // the list shows some reasons to build, not all of them (AGENTS rules 3 and 4).
    let plan = home.plan.as_ref().filter(|p| p.error.is_none());
    if home.attention.is_empty() {
        let message = match (&home.state, &home.plan, plan) {
            (StateStatus::Recorded, Some(p), _) if p.error.is_some() => {
                Some("The plan couldn't be made, so what would be built isn't known.")
            }
            (StateStatus::Recorded, _, Some(p)) if p.build == 0 => {
                Some("Nothing: the plan reuses every node.")
            }
            (StateStatus::Recorded, _, _) => None,
            _ => Some("Nothing to compare with until a first run is recorded."),
        };
        if let Some(message) = message {
            let _ = write!(b, r#"<p class="all-clear">{}</p>"#, text(message));
        }
    }
    for item in &home.attention {
        let (class, label) = match item.kind {
            AttentionKind::Changed => ("changed", "Changed"),
            AttentionKind::Unknown => ("unknown", "Unknown"),
            AttentionKind::Opaque => ("opaque", "Opaque"),
        };
        let _ = write!(
            b,
            r#"<div class="att" data-kind="{class}"><span class="kind {class}">{label}</span><span class="body"><a class="node" href="lineage#node={href}">{node}</a><span class="why">{why}</span></span></div>"#,
            href = attr(&url_component(&item.node_id)),
            node = text(&item.node),
            why = text(&item.why),
        );
    }
    if home.attention_more > 0 {
        let _ = write!(
            b,
            r#"<p class="all-clear">and {} more; <code>ods state plan</code> lists them all</p>"#,
            home.attention_more
        );
    }
    if let Some(plan) = plan
        && plan.build > 0
    {
        let reasons = plan
            .builds_for
            .iter()
            .map(|r| format!("{} {}", r.count, r.label))
            .collect::<Vec<_>>()
            .join(" · ");
        let _ = write!(
            b,
            r#"<p class="plan-builds" data-build="{n}">The plan builds {nodes} (<code>ods state plan</code>): {reasons}</p>"#,
            n = plan.build,
            nodes = if plan.build == 1 {
                "1 node".to_owned()
            } else {
                format!("{} nodes", plan.build)
            },
            reasons = text(&reasons),
        );
    }
    if let Some(plan) = &home.plan
        && !plan.warnings.is_empty()
    {
        b.push_str(r#"<ul class="warnings">"#);
        for warning in &plan.warnings {
            let _ = write!(b, "<li>{}</li>", text(warning));
        }
        b.push_str("</ul>");
    }
    b.push_str("</section>");
}

/// Health, coverage and modules.
fn panels(b: &mut String, home: &HomeView) {
    // Health, coverage, modules.
    b.push_str(r#"<div class="grid3"><section class="card" aria-label="Health"><h2>Health</h2>"#);
    for row in &home.health {
        let _ = write!(
            b,
            r#"<div class="row"><span class="hdot {key}"></span><span class="grow">{label}</span><span class="num">{count}</span></div>"#,
            key = attr(row.key),
            label = text(row.label),
            count = row
                .count
                .map_or_else(|| "[n]".to_owned(), |n| n.to_string()),
        );
    }
    b.push_str(r#"</section><section class="card" aria-label="Coverage"><h2>Coverage</h2>"#);
    for row in &home.coverage {
        let (count, width) = match row.count {
            Some(n) => (n.to_string(), (n * 100).checked_div(row.total).unwrap_or(0)),
            None => (format!("[{}]", row.placeholder), 0),
        };
        let _ = write!(
            b,
            r#"<div class="cov"><span class="top"><span>{label}</span><span>{count} / {total}</span></span><span class="track"><span style="width:{width}%"></span></span></div>"#,
            label = text(row.label),
            count = text(&count),
            total = row.total,
        );
    }
    b.push_str(r#"</section><section class="card" aria-label="Modules"><h2>Modules</h2>"#);
    for module in &home.modules {
        let status = match module.state {
            ModuleState::Ready => r#"<span class="ready">Ready</span>"#.to_owned(),
            ModuleState::Available => {
                r#"<span class="available" title="Works from the CLI; not checked for this project here">Available</span>"#.to_owned()
            }
            ModuleState::NotSetUp => r#"<span class="not_set_up">Not set up</span>"#.to_owned(),
            _ => r#"<span class="chip">Planned</span>"#.to_owned(),
        };
        let note = match (&module.note, module.state) {
            (Some(note), ModuleState::Ready | ModuleState::Available) => {
                format!(r#"<span class="mod-note mono">· {}</span>"#, text(note))
            }
            (Some(note), _) => format!(r#"<span class="mod-note">· {}</span>"#, text(note)),
            (None, _) => String::new(),
        };
        let _ = write!(
            b,
            r#"<div class="mod"><span>{name}</span><span class="mod-status">{status}{note}</span></div>"#,
            name = text(&module.name),
        );
    }
    b.push_str("</section></div>");
}

/// Percent-encodes a URL fragment value.
fn url_component(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// A page at the dashboard's root, inside the shell: for pages built in their own
/// modules, such as Lineage (#312).
pub(crate) fn root_page(view: &ShellView, title: &str, body: &str, generation: u64) -> String {
    shell(view, title, body, generation)
}
