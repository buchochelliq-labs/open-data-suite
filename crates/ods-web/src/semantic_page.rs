//! The Semantic layer page's HTML (#352), rendered on the server from [`SemanticView`]
//! inside the dashboard's shell. Static: no script, and nothing that queries.

use std::fmt::Write as _;

use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};

use crate::catalog::NodeLink;
use crate::dashboard::ShellView;
use crate::dashboard::state::count;
use crate::home::{Frame, framed};
use crate::semantic::{MetricView, SemanticField, SemanticModelView, SemanticView};
use crate::state_pages::crumbs;

/// The page sits at `catalog/semantic`, one level down.
const ROOT: &str = "../";

const CSS: &str = include_str!("../assets/semantic.css");

/// The Semantic layer page, inside the shell.
pub(crate) fn semantic_page(shell: &ShellView, view: &SemanticView, generation: u64) -> String {
    let mut b = String::with_capacity(16 * 1024);
    b.push_str(r#"<div class="sem"><div class="sem-head"><h1>Semantic layer <span class="chip">Planned</span></h1><p class="muted">Semantic models and metrics, as the project declares them.</p></div>"#);
    source_row(&mut b, view);
    b.push_str(r#"<p class="sem-notice" role="note"><strong>Read-only placeholder.</strong> ODS lists what the project declares and the models each is defined on. It doesn't query, validate or serve metrics, and lineage doesn't go through metrics yet.</p>"#);
    unreadable(&mut b, &view.unreadable);
    if view.models.is_empty() && view.metrics.is_empty() {
        empty(&mut b, view);
    } else {
        b.push_str(r#"<div class="sem-grid">"#);
        models(&mut b, &view.models);
        metrics(&mut b, &view.metrics);
        b.push_str("</div>");
    }
    b.push_str(r#"<section class="card sem-scope" aria-label="What ODS does with it"><div><h2>ODS reads semantic definitions; it doesn't serve or query metrics.</h2><p class="muted">No metric values, no query endpoint, no warehouse calls from this page. Use your own tools to compute metrics; the Impact simulator shows what a change to a model reaches.</p></div><a class="btn" href="../lineage/impact">Open impact</a></section>"#);
    b.push_str("</div>");
    let frame = Frame {
        title: "Semantic layer",
        crumbs: Some(crumbs(
            &[("Catalog", Some("../catalog")), ("Semantic layer", None)],
            false,
        )),
        root: ROOT,
        status: Some(
            r#"<span class="pill-snap" title="Definitions only: nothing here queries"><span class="dot none"></span>read-only · planned</span>"#
                .to_owned(),
        ),
        sub: Some("semantic"),
        search: true,
        css: CSS,
        js: "",
    };
    framed(shell, &frame, &b, generation)
}

/// Where the definitions come from; other build tools are planned.
fn source_row(b: &mut String, view: &SemanticView) {
    let _ = write!(
        b,
        r#"<div class="sem-source"><span class="sem-label">Source</span><span class="sem-pill{class}">{source}</span><span class="sem-pill planned" aria-disabled="true">other build tools (planned)</span></div>"#,
        class = if view.source.is_some() && view.unavailable.is_none() {
            ""
        } else {
            " none"
        },
        source = text(
            view.source
                .as_deref()
                .unwrap_or("the project's definitions couldn't be read")
        ),
    );
}

fn empty(b: &mut String, view: &SemanticView) {
    // Only artifacts that can declare a semantic layer, read whole, say there is none.
    let (title, why) = match (&view.unavailable, &view.source) {
        (Some(why), _) => ("Not available from these artifacts", why.as_str()),
        (None, None) => (
            "Not available",
            "Nothing could be read, so nothing is listed: the server log says why.",
        ),
        (None, Some(_)) if !view.unreadable.is_empty() => (
            "Nothing readable",
            "Every definition the project declares is listed above as unreadable.",
        ),
        (None, Some(_)) => (
            "No semantic layer",
            "This project declares no semantic models or metrics. When it does, they are listed here with the models each is defined on.",
        ),
    };
    let _ = write!(
        b,
        r#"<section class="card sem-empty" aria-label="{t}"><h2>{t}</h2><p class="muted">{w}</p></section>"#,
        t = text(title),
        w = text(why)
    );
}

/// The definitions left out because their entries couldn't be read.
fn unreadable(b: &mut String, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    let names: Vec<String> = ids
        .iter()
        .map(|id| format!(r#"<span class="mono">{}</span>"#, text(id)))
        .collect();
    let _ = write!(
        b,
        r#"<p class="sem-notice bad" role="alert">{} couldn't be read, so the lists below leave {} out: {}.</p>"#,
        text(&count(ids.len(), "definition")),
        if ids.len() == 1 { "it" } else { "them" },
        names.join(", "),
    );
}

fn link(l: &NodeLink) -> String {
    match &l.href {
        Some(href) => format!(
            r#"<a class="mono" href="{ROOT}{href}" title="{id}">{name}</a>"#,
            href = attr(href),
            id = attr(&l.id),
            name = text(&l.name),
        ),
        None => format!(
            r#"<span class="mono" title="{id}">{name}</span>"#,
            id = attr(&l.id),
            name = text(&l.name),
        ),
    }
}

fn links(ls: &[NodeLink]) -> String {
    if ls.is_empty() {
        return r#"<span class="muted">—</span>"#.to_owned();
    }
    ls.iter().map(link).collect::<Vec<_>>().join(", ")
}

/// `name (kind)`, …, or a dash.
fn fields(fs: &[SemanticField]) -> String {
    if fs.is_empty() {
        return r#"<span class="muted">none</span>"#.to_owned();
    }
    fs.iter()
        .map(|f| {
            let kind = f
                .kind
                .as_deref()
                .map(|k| format!(r#" <span class="muted">{}</span>"#, text(k)))
                .unwrap_or_default();
            match &f.description {
                Some(d) => format!(
                    r#"<span class="mono" title="{}">{}</span>{kind}"#,
                    attr(d),
                    text(&f.name)
                ),
                None => format!(r#"<span class="mono">{}</span>{kind}"#, text(&f.name)),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn models(b: &mut String, models: &[SemanticModelView]) {
    let _ = write!(
        b,
        r#"<section class="card sem-models" aria-label="Semantic models"><div class="card-head"><h2>Semantic models</h2><span class="muted">{}</span></div>"#,
        text(&count(models.len(), "semantic model")),
    );
    if models.is_empty() {
        b.push_str(r#"<p class="muted">None declared.</p>"#);
    }
    for m in models {
        let _ = write!(
            b,
            r#"<div class="sem-model" data-semantic-model="{id_attr}"><span class="mono name" title="{id_attr}">{name}</span><span class="on">on model {on}</span>"#,
            id_attr = attr(&m.id),
            name = text(&m.name),
            on = links(&m.defined_on),
        );
        if let Some(d) = &m.description {
            let _ = write!(b, r#"<span class="desc">{}</span>"#, text(d));
        }
        let _ = write!(
            b,
            r#"<span class="counts muted">{} · {} · {}</span><dl><dt>Measures</dt><dd>{}</dd><dt>Dimensions</dt><dd>{}</dd><dt>Entities</dt><dd>{}</dd></dl></div>"#,
            match m.entities.len() {
                1 => "1 entity".to_owned(),
                n => format!("{n} entities"),
            },
            text(&count(m.measures.len(), "measure")),
            text(&count(m.dimensions.len(), "dimension")),
            fields(&m.measures),
            fields(&m.dimensions),
            fields(&m.entities),
        );
    }
    b.push_str("</section>");
}

fn metrics(b: &mut String, metrics: &[MetricView]) {
    let _ = write!(
        b,
        r#"<section class="sem-metrics" aria-label="Metrics"><div class="card-head"><h2>Metrics</h2><span class="muted">{}</span></div>"#,
        text(&count(metrics.len(), "metric")),
    );
    if metrics.is_empty() {
        b.push_str(r#"<p class="card muted">None declared.</p></section>"#);
        return;
    }
    b.push_str(r#"<div class="card sem-table-wrap"><table class="sem-table"><thead><tr><th>Name</th><th>Type</th><th>Computed from</th><th>Dimensions</th><th>Depends on</th></tr></thead><tbody>"#);
    for m in metrics {
        let label = m
            .label
            .as_deref()
            .filter(|l| *l != m.name)
            .map(|l| format!(r#"<span class="label">{}</span>"#, text(l)))
            .unwrap_or_default();
        let _ = write!(
            b,
            r#"<tr data-metric="{id_attr}"><td><span class="mono" title="{desc}">{name}</span>{label}</td><td>{kind}</td><td class="mono">{from}</td><td class="mono">{dims}</td><td>{deps}</td></tr>"#,
            id_attr = attr(&m.id),
            desc = attr(m.description.as_deref().unwrap_or(&m.id)),
            name = text(&m.name),
            kind = m.kind.as_deref().map_or_else(
                || r#"<span class="muted">not declared</span>"#.to_owned(),
                |k| format!(r#"<span class="sem-kind">{}</span>"#, text(k))
            ),
            from = m.computed_from.as_deref().map_or_else(
                || r#"<span class="muted">—</span>"#.to_owned(),
                |f| text(f).into_owned()
            ),
            dims = if m.dimensions.is_empty() {
                r#"<span class="muted">—</span>"#.to_owned()
            } else {
                text(&m.dimensions.join(", ")).into_owned()
            },
            deps = links(&m.depends_on),
        );
    }
    b.push_str(r#"</tbody></table></div><p class="note muted">Depends on: the models its semantic models are defined on, through the metrics it reads. Lineage and Impact don't show metrics yet.</p></section>"#);
}
