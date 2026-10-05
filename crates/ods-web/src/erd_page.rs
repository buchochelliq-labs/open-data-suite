//! The ERD page's HTML (#64), rendered on the server from [`ErdView`] inside the
//! dashboard's shell. The diagram is drawn in the page from the embedded ERD (with
//! dagre, as the Lineage page is); the scope is a form whose values go in the URL; the
//! missing relationships, what is already tested and a table of every relationship
//! are plain HTML, so the page says what it knows without script.

use std::fmt::Write as _;

use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};
use ods_erd::{Basis, Cardinality, Erd};

use crate::dashboard::ShellView;
use crate::erd::{ErdView, cardinality_sentence};
use crate::home::{Frame, framed};
use crate::state_pages::crumbs;

/// The page's stylesheet.
pub(crate) const CSS: &str = include_str!("../assets/erd.css");
/// The page's script.
const JS: &str = include_str!("../assets/erd.js");

/// The ERD page, inside the shell.
pub(crate) fn erd_page(
    shell: &ShellView,
    view: &ErdView,
    query: &[(String, String)],
    generation: u64,
) -> Result<String, serde_json::Error> {
    let mut b = String::with_capacity(32 * 1024);
    b.push_str(r#"<div class="erd"><div class="erd-main">"#);
    toolbar(&mut b, view, query);
    match (&view.erd, &view.unavailable) {
        (Some(erd), _) => {
            notices(&mut b, view, erd);
            canvas(&mut b, erd);
            legend(&mut b);
            table(&mut b, view, erd);
        }
        (None, Some(why)) => {
            let _ = write!(
                b,
                r#"<section class="card erd-empty" role="status"><h2>No ERD to show</h2><p>{}</p></section>"#,
                text(why)
            );
        }
        (None, None) => {}
    }
    b.push_str("</div>");
    if view.erd.is_some() {
        side(&mut b, view);
    }
    b.push_str("</div>");
    if let Some(erd) = &view.erd {
        let data =
            serde_json::json!({ "erd": erd, "numbers": view.numbers, "proven": view.proven });
        let _ = write!(
            b,
            r#"<script type="application/json" id="erd-data">{data}</script><script>{dagre}</script><script>{JS}</script>"#,
            data = crate::lineage::embeddable(&data)?,
            dagre = crate::page::DAGRE,
        );
    }
    let frame = Frame {
        title: "ERD",
        crumbs: Some(crumbs(&[("ERD", None)], false)),
        root: "",
        status: Some(
            r#"<span class="erd-note">Relationships only. Lineage edges are on the <a href="lineage">lineage graph</a>.</span>"#
                .to_owned(),
        ),
        sub: None,
        search: false,
        css: CSS,
        js: "",
    };
    Ok(framed(shell, &frame, &b, generation))
}

fn toolbar(b: &mut String, view: &ErdView, query: &[(String, String)]) {
    let api = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(
            query
                .iter()
                .filter(|(k, _)| matches!(k.as_str(), "select" | "depth" | "all")),
        )
        .finish();
    let _ = write!(
        b,
        r#"<form class="erd-tools" method="get" action="erd"><label class="erd-select"><span class="erd-sr">Select entities</span><input name="select" value="{select}" placeholder="--select orders customers" autocomplete="off" spellcheck="false" list="erd-names"></label><label class="erd-depth">Depth <input type="number" name="depth" min="0" max="10" value="{depth}"></label><label class="erd-check"><input type="checkbox" name="all" value="1"{all}> Entities without relationships</label><button type="submit" class="btn">Apply</button><span class="erd-sep" aria-hidden="true"></span><label class="erd-check erd-js"><input type="checkbox" id="erd-inferred" checked> Inferred edges</label><label class="erd-check erd-js"><input type="checkbox" id="erd-columns" checked> All columns</label><button type="button" class="btn erd-js" id="erd-fit">Fit</button><button type="button" class="btn erd-js" id="erd-export">Export SVG</button><a class="btn" href="api/erd{q}{api}">JSON</a>"#,
        select = attr(&view.select.join(" ")),
        depth = view.depth,
        all = if view.all { " checked" } else { "" },
        q = if api.is_empty() { "" } else { "?" },
        api = attr(&api),
    );
    if let Some(erd) = &view.erd {
        b.push_str(r#"<datalist id="erd-names">"#);
        for entity in &erd.entities {
            let _ = write!(b, r#"<option value="{}">"#, attr(&entity.name));
        }
        b.push_str("</datalist>");
    }
    b.push_str("</form>");
}

fn notices(b: &mut String, view: &ErdView, erd: &Erd) {
    if !view.unknown.is_empty() {
        let _ = write!(
            b,
            r#"<p class="erd-problem" role="alert">No model, seed, snapshot or source is called {}.</p>"#,
            view.unknown
                .iter()
                .map(|u| format!("<code>{}</code>", text(u)))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !view.ambiguous.is_empty() {
        let _ = write!(
            b,
            r#"<p class="erd-problem" role="alert">{} more than one entity: select one by its id.</p>"#,
            view.ambiguous
                .iter()
                .map(|u| format!("<code>{}</code> names", text(u)))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if erd.relationships.is_empty() && view.unknown.is_empty() && view.ambiguous.is_empty() {
        let _ = write!(
            b,
            r#"<p class="erd-empty-note" role="status">No relationship is tested, declared, joined in SQL or inferred among {}. {}</p>"#,
            if view.select.is_empty() {
                "the project's entities".to_owned()
            } else {
                "the selected entities".to_owned()
            },
            if view.all {
                String::new()
            } else {
                format!(
                    "Tick <em>Entities without relationships</em> to see all {} entities.",
                    view.total_entities
                )
            }
        );
    }
}

fn canvas(b: &mut String, erd: &Erd) {
    let _ = write!(
        b,
        r#"<div class="erd-canvas" id="erd-canvas"><svg id="erd-svg" role="group" aria-label="Entity-relationship diagram: {n} entities, {m} relationships. Each entity and relationship can be focused; Enter shows its evidence."><g id="erd-viewport"></g></svg><noscript><p class="erd-noscript">The diagram needs script. Every relationship is listed below the legend.</p></noscript></div>"#,
        n = erd.entities.len(),
        m = erd.relationships.len(),
    );
}

const LEGEND: &str = r#"<section class="card erd-legend" aria-label="Legend"><div class="erd-legend-row"><strong>Edge evidence</strong><span><svg width="34" height="10" aria-hidden="true"><line x1="0" y1="5" x2="34" y2="5" class="lg tested"/></svg>tested</span><span><svg width="34" height="10" aria-hidden="true"><line x1="0" y1="5" x2="34" y2="5" class="lg declared"/></svg>declared constraint</span><span><svg width="34" height="10" aria-hidden="true"><line x1="0" y1="5" x2="34" y2="5" class="lg joined"/></svg>joined in SQL</span><span><svg width="34" height="10" aria-hidden="true"><line x1="0" y1="5" x2="34" y2="5" class="lg inferred"/></svg>inferred</span></div><p class="muted">One edge per relationship, by its strongest evidence: declared, then tested, joined, inferred. Inferred edges come only from matching names.</p><div class="erd-legend-row"><strong>Cardinality</strong><span><svg width="30" height="14" aria-hidden="true"><line x1="0" y1="7" x2="30" y2="7" class="lgg"/><line x1="6" y1="1" x2="6" y2="13" class="lgg"/><line x1="11" y1="1" x2="11" y2="13" class="lgg"/></svg>exactly one (a key, never null)</span><span><svg width="30" height="14" aria-hidden="true"><line x1="0" y1="7" x2="30" y2="7" class="lgg"/><line x1="6" y1="1" x2="6" y2="13" class="lgg"/><circle cx="14" cy="7" r="4" class="lgg-o"/></svg>zero or one (a key, may be null)</span><span><svg width="30" height="14" aria-hidden="true"><line x1="0" y1="7" x2="30" y2="7" class="lgg"/><line x1="12" y1="7" x2="0" y2="1" class="lgg"/><line x1="12" y1="7" x2="0" y2="13" class="lgg"/></svg>many</span><span><svg width="30" height="14" aria-hidden="true"><line x1="0" y1="7" x2="30" y2="7" class="lgg"/><circle cx="12" cy="7" r="6" class="lgg-o"/><text x="12" y="10" text-anchor="middle" font-size="9">?</text></svg>unknown: no key on that side</span><span><b class="pk tested">PK</b> tested key</span><span><b class="pk declared">PK</b> declared key</span><span><b class="pk inferred">PK</b> inferred key</span></div></section>"#;

fn legend(b: &mut String) {
    b.push_str(LEGEND);
}

fn basis_word(basis: Basis) -> &'static str {
    match basis {
        Basis::Tested => "tested",
        Basis::Declared => "declared",
        Basis::Joined => "joined",
        Basis::Inferred => "inferred",
        _ => "other",
    }
}

fn cardinality_word(c: Cardinality, optional: bool) -> &'static str {
    match (c, optional) {
        (Cardinality::ManyToOne, false) => "many to exactly one",
        (Cardinality::ManyToOne, true) => "many to zero or one",
        (Cardinality::OneToOne, _) => "one to one",
        _ => "unknown",
    }
}

/// Every relationship as a table: what the diagram shows, without script.
fn table(b: &mut String, view: &ErdView, erd: &Erd) {
    if erd.relationships.is_empty() {
        return;
    }
    let name = |id: &str| {
        erd.entities
            .iter()
            .find(|e| e.id == id)
            .map_or(id, |e| e.name.as_str())
            .to_owned()
    };
    let _ = write!(
        b,
        r#"<details class="card erd-all"><summary>Every relationship ({})</summary><table><thead><tr><th>#</th><th>From</th><th>To</th><th>Evidence</th><th>Cardinality</th><th>Basis</th></tr></thead><tbody>"#,
        erd.relationships.len()
    );
    for (i, r) in erd.relationships.iter().enumerate() {
        let proven = view.proven.get(i).copied().unwrap_or(false);
        let number = view
            .numbers
            .get(i)
            .copied()
            .flatten()
            .map_or_else(String::new, |n| n.to_string());
        let _ = write!(
            b,
            r#"<tr data-basis="{basis}"><td>{number}</td><td class="mono">{from}.{fc}</td><td class="mono">{to}.{tc}</td><td class="mono">{evidence}</td><td title="{sentence}">{card}</td><td><span class="basis {basis}">{basis}</span></td></tr>"#,
            basis = basis_word(r.basis),
            from = text(&name(&r.from)),
            fc = text(&r.from_columns.join(", ")),
            to = text(&name(&r.to)),
            tc = text(&r.to_columns.join(", ")),
            evidence = text(&r.evidence.join(", ")),
            sentence = attr(&cardinality_sentence(r, proven)),
            card = if proven || r.cardinality == Cardinality::Unknown {
                cardinality_word(r.cardinality, r.optional)
            } else {
                "unproven"
            },
        );
    }
    b.push_str("</tbody></table></details>");
}

fn side(b: &mut String, view: &ErdView) {
    b.push_str(r#"<aside class="erd-side" aria-label="Details"><section id="erd-detail" class="erd-detail" aria-live="polite" hidden></section>"#);
    b.push_str(r"<section><h2>Missing relationships</h2>");
    if view.missing.is_empty() {
        b.push_str(r#"<p class="muted">Every relationship in scope is tested or declared.</p>"#);
    } else {
        b.push_str(r#"<p class="muted">Keys used together, or named alike, with no relationship test. Add the test to make the edge tested.</p><ol class="erd-missing">"#);
        for m in &view.missing {
            let _ = write!(
                b,
                r#"<li data-number="{n}"><div class="erd-m-head"><span class="erd-num" aria-hidden="true">{n}</span><span class="mono">{from} ↔ {to}</span><span class="basis {basis}">{basis}</span></div><p>{summary}</p>"#,
                n = m.number,
                from = text(&m.from),
                to = text(&m.to),
                basis = basis_word(m.basis),
                summary = code(&m.summary),
            );
            if let Some(s) = &m.suggestion {
                let _ = write!(
                    b,
                    r#"<p class="muted">Add {under}</p><pre class="erd-yaml"><code>{snippet}</code></pre><button type="button" class="btn erd-copy erd-js" data-copy="{copy}">Copy</button>"#,
                    under = text(&s.add_under),
                    snippet = text(&s.snippet),
                    copy = attr(&s.snippet),
                );
            }
            b.push_str("</li>");
        }
        b.push_str("</ol>");
    }
    b.push_str("</section>");
    let _ = write!(b, "<section><h2>Already tested ({})</h2>", view.known.len());
    if view.known.is_empty() {
        b.push_str(r#"<p class="muted">No relationship in scope is tested or declared yet.</p>"#);
    } else {
        b.push_str(r#"<ul class="erd-known">"#);
        for k in &view.known {
            let _ = write!(
                b,
                "<li><code>{from}</code> → {to}{declared}</li>",
                from = text(&k.from),
                to = text(&k.to),
                declared = if k.basis == Basis::Declared {
                    r#" <span class="muted">(declared)</span>"#
                } else {
                    ""
                },
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
