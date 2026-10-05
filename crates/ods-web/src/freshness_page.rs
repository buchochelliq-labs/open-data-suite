//! The Freshness evidence screen's HTML (#350), rendered on the server from
//! [`FreshnessView`] inside the dashboard's shell. Static: no script.

use std::fmt::Write as _;

use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};

use crate::catalog::{DecisionView, NodeLink};
use crate::catalog_page::{basis_line, decision_pill, warnings};
use crate::dashboard::ShellView;
use crate::freshness::{
    EvidenceView, FreshnessView, Grade, InputKind, InputView, ReaderView, RecordedVersion,
};
use crate::home::{Frame, framed};
use crate::state_pages::crumbs;

/// The page sits at `catalog/sources`, one level down.
const ROOT: &str = "../";

/// The stylesheet: the Catalog's, then the screen's own.
const CSS: &str = concat!(
    include_str!("../assets/catalog.css"),
    include_str!("../assets/freshness.css")
);

/// Readers or downstream nodes listed before the rest are folded away.
const SHOWN: usize = 6;

fn grade_dot(grade: Grade) -> String {
    format!(
        r#"<span class="grade"><span class="gdot {key}"></span>{key}</span>"#,
        key = grade.key()
    )
}

fn node_link(link: &NodeLink) -> String {
    match &link.href {
        Some(href) => format!(
            r#"<a class="mono" href="{ROOT}{href}" title="{id}">{name}</a>"#,
            href = attr(href),
            id = attr(&link.id),
            name = text(&link.name),
        ),
        None => format!(
            r#"<span class="mono" title="{id}">{name}</span>"#,
            id = attr(&link.id),
            name = text(&link.name),
        ),
    }
}

/// `names`, `SHOWN` of them, then the rest folded.
fn folded(items: &[String], sep: &str) -> String {
    if items.len() <= SHOWN {
        return items.join(sep);
    }
    format!(
        r#"{}<details class="more"><summary>{} more</summary>{}</details>"#,
        items[..SHOWN].join(sep),
        items.len() - SHOWN,
        items[SHOWN..].join(sep),
    )
}

/// The Freshness evidence screen, inside the shell.
pub(crate) fn freshness_page(shell: &ShellView, view: &FreshnessView, generation: u64) -> String {
    let mut b = String::with_capacity(32 * 1024);
    b.push_str(r#"<div class="fresh"><section class="fresh-main" aria-label="Inputs">"#);
    let _ = write!(
        b,
        r#"<div class="fresh-head"><h1>Freshness evidence</h1><p class="muted">How ODS knows whether each project input changed since the nodes reading it were built. {sources}, {seeds}.</p><div class="cat-tools">"#,
        sources = text(&crate::dashboard::state::count(view.sources, "source")),
        seeds = text(&crate::dashboard::state::count(view.seeds, "seed")),
    );
    basis_line(&mut b, &view.decisions);
    b.push_str("</div>");
    warnings(&mut b, &view.decisions);
    b.push_str("</div>");
    legend(&mut b, view);
    inputs(&mut b, view);
    if view.sources == 0 {
        b.push_str(
            r#"<div class="card note" data-note="no-sources"><h2>No sources declared</h2><p class="muted">When the project declares sources, each appears here with its evidence. A source that says how its new data is measured gets a version ODS can compare; one that doesn't is unknown, and everything reading it builds.</p></div>"#,
        );
    }
    b.push_str("</section>");
    rail(&mut b, view);
    b.push_str("</div>");
    let frame = Frame {
        title: "Freshness evidence",
        crumbs: Some(crumbs(
            &[
                ("Catalog", Some("../catalog")),
                ("Freshness evidence", None),
            ],
            false,
        )),
        root: ROOT,
        status: None,
        sub: Some("freshness"),
        search: true,
        css: CSS,
        js: "",
    };
    framed(shell, &frame, &b, generation)
}

fn legend(b: &mut String, view: &FreshnessView) {
    b.push_str(r#"<div class="card legend" aria-label="Grades"><ul>"#);
    for g in &view.grades {
        let _ = write!(
            b,
            r#"<li data-grade="{key}">{dot}<span class="muted">{meaning}</span></li>"#,
            key = g.grade.key(),
            dot = grade_dot(g.grade),
            meaning = text(g.meaning),
        );
    }
    b.push_str("</ul></div>");
}

fn inputs(b: &mut String, view: &FreshnessView) {
    let _ = write!(
        b,
        r#"<div class="card inputs"><div class="card-head"><h2>Inputs <span class="count muted">{sources} · {seeds}</span></h2><a class="btn" href="{ROOT}api/catalog/sources">JSON</a></div>"#,
        sources = text(&crate::dashboard::state::count(view.sources, "source")),
        seeds = text(&crate::dashboard::state::count(view.seeds, "seed")),
    );
    if view.inputs.is_empty() {
        b.push_str(
            r#"<p class="muted">Nothing to list: the project reads no sources or seeds.</p></div>"#,
        );
        return;
    }
    b.push_str(
        r#"<div class="cat-table"><table class="fresh-table"><thead><tr><th scope="col" class="c-in">Input</th><th scope="col" class="c-ev">Evidence now</th><th scope="col" class="c-rec">Last recorded</th><th scope="col" class="c-rd">Read by</th><th scope="col" class="c-dn" title="What a change to it can reach; whether each rebuilds is up to its policy">Downstream</th></tr></thead><tbody>"#,
    );
    for input in &view.inputs {
        row(b, input);
    }
    b.push_str("</tbody></table></div></div>");
}

fn row(b: &mut String, input: &InputView) {
    let kind = match input.kind {
        InputKind::Source => "source",
        InputKind::Seed => "seed",
    };
    let name = match &input.href {
        Some(href) => format!(
            r#"<a class="mono" href="{ROOT}{href}">{name}</a>"#,
            href = attr(href),
            name = text(&input.name)
        ),
        None => format!(r#"<span class="mono">{}</span>"#, text(&input.name)),
    };
    let detail = input
        .file
        .as_ref()
        .or(input.relation.as_ref())
        .map(|d| format!(" · {}", text(d)))
        .unwrap_or_default();
    let _ = write!(
        b,
        r#"<tr data-input="{id}" data-kind="{kind}"><td><div class="in-name">{name}</div><div class="sub muted">{kind}{detail}</div></td>"#,
        id = attr(&input.id),
    );
    evidence(b, input.evidence.as_ref(), input.decision.as_ref());
    recorded(b, &input.recorded);
    readers(b, &input.readers);
    let downstream: Vec<String> = input.downstream.iter().map(node_link).collect();
    let _ = write!(
        b,
        r#"<td data-downstream="{n}"><div class="sub muted">{count}</div><div class="names">{names}</div></td></tr>"#,
        n = input.downstream.len(),
        count = text(&crate::dashboard::state::count(
            input.downstream.len(),
            "node"
        )),
        names = folded(&downstream, " · "),
    );
}

fn evidence(b: &mut String, evidence: Option<&EvidenceView>, decision: Option<&DecisionView>) {
    let Some(e) = evidence else {
        let _ = write!(
            b,
            r#"<td data-grade="unknown"><div>{}</div><div class="sub muted">not compared: no plan yet</div></td>"#,
            grade_dot(Grade::Unknown)
        );
        return;
    };
    let value = e.value.as_deref().map_or_else(
        || r#"<span class="unknown-v">unknown</span>"#.to_owned(),
        |v| format!(r#"<span class="mono">{}</span>"#, text(v)),
    );
    let observed = e
        .observed_at
        .map(|at| {
            format!(
                r#"<div class="sub muted">observed {}</div>"#,
                text(&at.to_string())
            )
        })
        .unwrap_or_default();
    let mut notes = String::new();
    for note in &e.notes {
        let _ = write!(notes, r#"<div class="sub muted">{}</div>"#, text(note));
    }
    let decision = decision
        .map(|d| format!(r#"<div class="sub">{}</div>"#, decision_pill(d)))
        .unwrap_or_default();
    let _ = write!(
        b,
        r#"<td data-grade="{key}"><div>{dot}</div><div class="sub">{method}</div><div class="val">{value}</div>{observed}{notes}{decision}</td>"#,
        method = text(&e.method),
        key = e.grade.key(),
        dot = grade_dot(e.grade),
    );
}

fn recorded(b: &mut String, recorded: &[RecordedVersion]) {
    if recorded.is_empty() {
        b.push_str(r#"<td><span class="muted">nothing recorded</span></td>"#);
        return;
    }
    b.push_str("<td>");
    for r in recorded {
        let value = r.value.as_deref().map_or_else(
            || r#"<span class="unknown-v">unknown</span>"#.to_owned(),
            |v| format!(r#"<span class="mono">{}</span>"#, text(v)),
        );
        let readers = if r.readers > 1 {
            format!(" · {} readers", r.readers)
        } else {
            String::new()
        };
        let _ = write!(
            b,
            r#"<div class="rec"><div>{dot}</div><div class="val">{value}</div><div class="sub muted">built {at} · run {run}{readers}</div></div>"#,
            dot = grade_dot(r.grade),
            at = text(&r.built_at.to_string()),
            run = text(&r.run_id),
        );
    }
    b.push_str("</td>");
}

fn readers(b: &mut String, readers: &[ReaderView]) {
    if readers.is_empty() {
        b.push_str(r#"<td><span class="muted">none</span></td>"#);
        return;
    }
    let items: Vec<String> = readers
        .iter()
        .map(|r| {
            let policy = r
                .policy
                .as_deref()
                .map(|p| format!(r#" title="policy: {}""#, attr(p)))
                .unwrap_or_default();
            format!(
                r#"<div class="reader" data-reader="{id}"{policy}>{pill} {link} <a class="why" href="{ROOT}{why}">Why</a></div>"#,
                id = attr(&r.node.id),
                pill = decision_pill(&r.decision),
                link = node_link(&r.node),
                why = attr(&crate::lineage::why_href(&r.node.id)),
            )
        })
        .collect();
    let _ = write!(b, "<td>{}</td>", folded(&items, ""));
}

fn rail(b: &mut String, view: &FreshnessView) {
    b.push_str(
        r#"<aside class="fresh-rail" aria-label="Rule"><div class="eyebrow">Rule</div><h2>Unknown evidence ⇒ build</h2><p class="muted">ODS reuses a node only when it can show the node's inputs are unchanged. When the evidence is missing, or not good enough, the answer is build, never a silent reuse.</p>"#,
    );
    for g in &view.grades {
        let _ = write!(
            b,
            r#"<div class="card rule" data-grade="{key}"><div class="card-head">{dot}<span class="pill {class}">{plan}</span></div><span class="muted">{meaning}</span></div>"#,
            key = g.grade.key(),
            dot = grade_dot(g.grade),
            class = if g.allows_reuse { "reuse" } else { "build" },
            plan = text(g.plan),
            meaning = text(g.meaning),
        );
    }
    let _ = write!(
        b,
        "<h3>In this project</h3><p data-summary>{}</p>",
        text(&view.summary)
    );
    if let Some(at) = view.measured_at.filter(|_| view.sources > 0) {
        let _ = write!(
            b,
            r#"<p class="muted">Sources measured {at}{by}.</p>"#,
            at = text(&at.to_string()),
            by = view
                .measured_by
                .as_deref()
                .map(|by| format!(" by {}", text(by)))
                .unwrap_or_default(),
        );
    }
    let _ = write!(
        b,
        r#"<div class="rail-actions"><a class="btn" href="{ROOT}state/plan">Open plan</a></div></aside>"#
    );
}
