//! The Impact simulator's HTML (#347), rendered on the server from [`ImpactView`]
//! inside the dashboard's shell. It works without script: the proposed changes are a
//! form whose values go in the URL, so a simulation is a link to share. A little script
//! only copies the build command.

use std::fmt::Write as _;

use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};
use ods_lineage::{Change, ColumnChangeKind};

use crate::catalog::node_href;
use crate::dashboard::ShellView;
use crate::home::{Frame, framed};
use crate::impact::{
    ChangeKind, ImpactResult, ImpactView, LineageStatus, MAX_CHANGES, MAX_TRAIL, MustRun,
    ProposedChange, Verdict,
};
use crate::state_pages::crumbs;

/// The page's stylesheet.
pub(crate) const CSS: &str = include_str!("../assets/impact.css");

/// From `lineage/impact` to the dashboard's root.
const ROOT: &str = "../";

/// The Impact simulator, inside the shell. `add` shows one more, empty row.
pub(crate) fn impact_page(
    shell: &ShellView,
    view: &ImpactView,
    query: &[(String, String)],
    add: bool,
    generation: u64,
) -> String {
    let mut b = String::with_capacity(24 * 1024);
    b.push_str(r#"<div class="impact">"#);
    b.push_str(r#"<div class="imp-main">"#);
    form(&mut b, view, add);
    match &view.result {
        Some(result) => results(&mut b, result, query),
        None if view.changes.is_empty() => b.push_str(
            r#"<p class="imp-intro">Pick a column of a model and a change (rename, type change or drop) to see which models must run downstream, which would break, and which can be skipped. The change is simulated on the column lineage: nothing is run or written.</p>"#,
        ),
        None => {}
    }
    b.push_str("</div>");
    if let Some(result) = &view.result {
        side(&mut b, result);
    }
    b.push_str("</div>");
    // Enter in a field submits with the form's first button, which must be Simulate,
    // not a row's Remove: hence the hidden default above the rows.
    b.push_str(SCRIPT);
    let frame = Frame {
        title: "Impact simulator",
        crumbs: Some(crumbs(
            &[("Lineage", Some("../lineage")), ("Impact simulator", None)],
            false,
        )),
        root: ROOT,
        status: Some(
            r#"<span class="imp-badge"><span class="dot"></span>column lineage · nothing is run</span>"#
                .to_owned(),
        ),
        sub: Some("impact"),
        search: true,
        css: CSS,
        js: "",
    };
    framed(shell, &frame, &b, generation)
}

/// Copies the build command; without script, it can be selected by hand.
const SCRIPT: &str = r#"<script>(function(){var b=document.getElementById("imp-copy");var s=document.getElementById("imp-selector");if(!b||!s||!navigator.clipboard)return;b.hidden=false;b.addEventListener("click",function(){navigator.clipboard.writeText(s.textContent).then(function(){b.textContent="Copied";setTimeout(function(){b.textContent="Copy command";},1500);});});})();</script>"#;

fn form(b: &mut String, view: &ImpactView, add: bool) {
    b.push_str(r#"<section class="card imp-form" aria-label="Proposed change"><form method="get" action="impact"><button type="submit" class="imp-default" tabindex="-1" aria-hidden="true">Simulate</button><datalist id="imp-columns">"#);
    for option in &view.column_options {
        let _ = write!(b, r#"<option value="{}">"#, attr(option));
    }
    b.push_str("</datalist>");
    let blank = ProposedChange::blank();
    let rows: Vec<&ProposedChange> = if view.changes.is_empty() {
        vec![&blank]
    } else if add && view.changes.len() < MAX_CHANGES {
        view.changes.iter().chain([&blank]).collect()
    } else {
        view.changes.iter().collect()
    };
    let many = rows.len() > 1;
    for (i, change) in rows.iter().enumerate() {
        row(b, i, change, many);
    }
    b.push_str(
        r#"<div class="imp-actions"><button type="submit" class="btn primary">Simulate</button>"#,
    );
    if rows.len() < MAX_CHANGES {
        b.push_str(
            r#"<button type="submit" class="btn" name="add" value="1">Add another column</button>"#,
        );
    }
    b.push_str("</div>");
    if view.cut {
        let _ = write!(
            b,
            r#"<p class="imp-note">Only the first {MAX_CHANGES} changes are simulated.</p>"#
        );
    }
    b.push_str("</form></section>");
}

fn row(b: &mut String, i: usize, change: &ProposedChange, many: bool) {
    let _ = write!(
        b,
        r#"<fieldset class="imp-row" data-row="{i}"><legend class="imp-sr">Change {n}</legend><label class="imp-field col"><span>Column</span><input name="column" list="imp-columns" value="{value}" placeholder="model.column" autocomplete="off" spellcheck="false" required{invalid}></label><div class="imp-field"><span id="imp-kind-{i}">Change</span><div class="seg" role="radiogroup" aria-labelledby="imp-kind-{i}">"#,
        n = i + 1,
        value = attr(&change.input),
        invalid = if change.problem.is_some() {
            format!(r#" aria-invalid="true" aria-describedby="imp-problem-{i}""#)
        } else {
            String::new()
        },
    );
    for kind in ChangeKind::ALL {
        let _ = write!(
            b,
            r#"<label><input type="radio" name="change-{i}" value="{key}"{checked}><span>{label}</span></label>"#,
            key = kind.key(),
            label = text(kind.label()),
            checked = if change.change == Some(kind) {
                " checked"
            } else {
                ""
            },
        );
    }
    let from = change
        .current_type
        .as_deref()
        .map_or_else(String::new, |t| {
            format!(
                r#"<span class="imp-from mono" title="its type now">{} →</span>"#,
                text(t)
            )
        });
    let _ = write!(
        b,
        r#"</div></div><label class="imp-field to"><span>New name or type</span><span class="imp-to">{from}<input name="to" value="{to}" placeholder="new name or type" autocomplete="off" spellcheck="false"></span></label>"#,
        to = attr(change.to.as_deref().unwrap_or("")),
    );
    if many {
        let _ = write!(
            b,
            r#"<button type="submit" class="btn ghost" name="remove" value="{i}" aria-label="Remove change {n}" formnovalidate>Remove</button>"#,
            n = i + 1
        );
    }
    b.push_str("</fieldset>");
    if let Some(problem) = &change.problem {
        let _ = write!(
            b,
            r#"<p class="imp-problem" id="imp-problem-{i}" role="alert">{}</p>"#,
            text(problem)
        );
    } else if change.change.is_none() && !change.input.is_empty() {
        b.push_str(r#"<p class="imp-note">Choose a change, then Simulate.</p>"#);
    }
    if let Some(note) = &change.note {
        let _ = write!(b, r#"<p class="imp-note">{}</p>"#, text(note));
    }
}

fn results(b: &mut String, result: &ImpactResult, query: &[(String, String)]) {
    let count = |v: Verdict| result.must_run.iter().filter(|m| m.verdict == v).count();
    let (breaking, unknown) = (count(Verdict::Breaks), count(Verdict::Unknown));
    let api = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(query.iter().filter(|(k, _)| k != "add" && k != "remove"))
        .finish();
    let _ = write!(
        b,
        r#"<div class="imp-results-head"><h2>Results</h2><span class="muted">{run} must run · {breaking} known to break · {unknown} unknown · {skipped} skipped · {other} not downstream</span><a class="btn" href="../api/lineage/impact?{api}">JSON</a></div>"#,
        run = result.must_run.len(),
        skipped = result.skipped.len(),
        other = result.not_downstream,
        api = attr(&api),
    );
    let retyped = result.engine_changes.iter().any(|c| {
        matches!(
            c,
            Change::Column {
                kind: ColumnChangeKind::Modified,
                ..
            }
        )
    });
    if retyped {
        b.push_str(r#"<p class="imp-note">A type change removes no column, so none is known to break from it. SQL that relies on the old type (a cast, a date or string function, a sum) can still fail: check the affected models.</p>"#);
    }
    let _ = write!(
        b,
        r#"<section class="card imp-table" aria-labelledby="imp-run-h"><div class="imp-table-head"><span class="tag run">MUST RUN</span><h3 id="imp-run-h">{n} node{s}</h3><span class="muted">read the changed column directly or through another one, or can't be told apart</span></div>"#,
        n = result.must_run.len(),
        s = if result.must_run.len() == 1 { "" } else { "s" },
    );
    b.push_str(r"<table><thead><tr><th>Node</th><th>Verdict</th><th>Columns affected</th><th>How it reads</th><th>Reason</th></tr></thead><tbody>");
    for m in &result.must_run {
        must_run_row(b, m);
    }
    b.push_str("</tbody></table></section>");
    let _ = write!(
        b,
        r#"<section class="card imp-table" aria-labelledby="imp-skip-h"><div class="imp-table-head"><span class="tag skip">SKIPPED</span><h3 id="imp-skip-h">{n} node{s}, with reason</h3></div>"#,
        n = result.skipped.len(),
        s = if result.skipped.len() == 1 { "" } else { "s" },
    );
    if result.skipped.is_empty() {
        b.push_str(r#"<p class="muted imp-empty">No reader of a changed model can be skipped: each one uses what changed.</p>"#);
    } else {
        b.push_str("<table><tbody>");
        for s in &result.skipped {
            let _ = write!(
                b,
                r#"<tr data-node="{id}"><td class="mono"><a href="{ROOT}{href}">{name}</a></td><td>{reason}</td></tr>"#,
                id = attr(&s.id),
                href = attr(&node_href(&s.id)),
                name = text(&s.name),
                reason = code(&s.reason),
            );
        }
        b.push_str("</tbody></table>");
    }
    if result.not_downstream > 0 {
        let _ = write!(
            b,
            r#"<p class="muted imp-empty">{n} other node{s} {verb} upstream of the change or {verb2} unrelated to it, so {pronoun} can't be reached.</p>"#,
            n = result.not_downstream,
            s = if result.not_downstream == 1 { "" } else { "s" },
            verb = if result.not_downstream == 1 {
                "is"
            } else {
                "are"
            },
            verb2 = if result.not_downstream == 1 {
                "is"
            } else {
                "are"
            },
            pronoun = if result.not_downstream == 1 {
                "it"
            } else {
                "they"
            },
        );
    }
    b.push_str("</section>");
}

fn must_run_row(b: &mut String, m: &MustRun) {
    let (class, label) = verdict_pill(m.verdict);
    let (lineage_class, lineage_label, lineage_title) = match m.lineage {
        LineageStatus::Parsed => ("parsed", "parsed", "its SQL was analyzed"),
        LineageStatus::Opaque => (
            "opaque",
            "opaque",
            "nothing is known about how it uses its inputs",
        ),
        LineageStatus::Inferred => (
            "inferred",
            "inferred",
            "reached only through a node whose lineage is unknown",
        ),
    };
    let columns = match &m.columns {
        None => r#"<span class="muted">[unknown]</span>"#.to_owned(),
        Some(_) if m.rows_changed => {
            r#"<span title="its rows may change, so every column may">all (rows may change)</span>"#
                .to_owned()
        }
        Some(columns) if columns.is_empty() => r#"<span class="muted">none</span>"#.to_owned(),
        Some(columns) => columns
            .iter()
            .map(|c| text(c).into_owned())
            .collect::<Vec<_>>()
            .join(",<br>"),
    };
    let how: String = m.how.iter().fold(String::new(), |mut out, h| {
        let _ = write!(out, r#"<span class="chip-how">{}</span>"#, text(h));
        out
    });
    let reasons: String = m.reasons.iter().fold(String::new(), |mut out, r| {
        let _ = write!(out, "<p>{}</p>", code(r));
        out
    });
    let _ = write!(
        b,
        r#"<tr data-node="{id}" data-verdict="{vkey}"><td><a class="mono" href="{ROOT}{href}">{name}</a><br><span class="lin {lineage_class}" title="{lineage_title}">{lineage_label}</span></td><td><span class="verdict {class}">{label}</span></td><td class="mono cols">{columns}</td><td>{how}</td><td class="why">{reasons}</td></tr>"#,
        id = attr(&m.id),
        vkey = verdict_key(m.verdict),
        href = attr(&node_href(&m.id)),
        name = text(&m.name),
    );
}

fn verdict_key(v: Verdict) -> &'static str {
    match v {
        Verdict::Breaks => "breaks",
        Verdict::Unknown => "unknown",
        Verdict::LosesColumn => "loses_column",
        Verdict::Changed => "changed",
        Verdict::Affected => "affected",
    }
}

fn verdict_pill(v: Verdict) -> (&'static str, &'static str) {
    match v {
        Verdict::Breaks => ("breaks", "BREAKS"),
        Verdict::Unknown => ("unknown", "UNKNOWN"),
        Verdict::LosesColumn => ("loses", "LOSES COLUMN"),
        Verdict::Changed => ("changed", "CHANGED HERE"),
        Verdict::Affected => ("affected", "AFFECTED"),
    }
}

fn side(b: &mut String, result: &ImpactResult) {
    b.push_str(r#"<aside class="imp-side" aria-label="Details">"#);
    b.push_str("<section><h2>Column trail</h2>");
    if result.trail.is_empty() {
        b.push_str(r#"<p class="muted">No column reads the changed one directly: what must run is reached through rows or unknown lineage.</p>"#);
    } else {
        b.push_str(r#"<ol class="trail">"#);
        for step in &result.trail {
            let _ = write!(
                b,
                r#"<li style="--depth:{d}"><code>{from}</code> → <code>{to}</code> <span class="muted">{how}</span></li>"#,
                d = step.depth.saturating_sub(1).min(6),
                from = text(&step.from),
                to = text(&step.to),
                how = text(&step.how),
            );
        }
        b.push_str("</ol>");
        if result.trail_cut {
            let _ = write!(
                b,
                r#"<p class="muted">Only the first {MAX_TRAIL} steps are shown.</p>"#
            );
        }
    }
    b.push_str("</section>");
    let _ = write!(
        b,
        r#"<section><h2>Build command</h2><pre class="selector"><code id="imp-selector">{sel}</code></pre><button type="button" id="imp-copy" class="btn" hidden>Copy command</button><p class="muted">Plans exactly what must run; the readers that can be skipped keep their last build. The models that break need their SQL updated first.</p>"#,
        sel = text(&result.selector),
    );
    if !result.selector_exact {
        b.push_str(r#"<p class="imp-note">A name in it is shared by more than one node, so it may select more than must run.</p>"#);
    }
    b.push_str("</section>");
    b.push_str("<section><h2>Tests that run with it</h2>");
    if result.tests.is_empty() {
        b.push_str(r#"<p class="muted">No tests are declared on the nodes that must run.</p>"#);
    } else {
        b.push_str(r#"<ul class="tests">"#);
        for t in &result.tests {
            let on = t
                .column
                .as_deref()
                .map_or_else(|| t.node.clone(), |c| format!("{}.{c}", t.node));
            let _ = write!(
                b,
                r#"<li><code>{on}</code> <span class="muted">{name}</span></li>"#,
                on = text(&on),
                name = text(&t.name),
            );
        }
        b.push_str("</ul>");
    }
    b.push_str("</section></aside>");
}

/// Text with `backticked` parts as code.
fn code(s: &str) -> String {
    let mut out = String::new();
    for (i, part) in s.split('`').enumerate() {
        if i % 2 == 1 {
            let _ = write!(out, "<code>{}</code>", text(part));
        } else {
            out.push_str(&text(part));
        }
    }
    out
}
