//! The About page's HTML (ADR-0031 §3c), rendered on the server from [`AboutView`]
//! inside the dashboard's shell. Static: no script.

use std::fmt::Write as _;

use html_escape::encode_text as text;

use crate::about::{AboutView, FeatureFacts, PluginFacts};
use crate::dashboard::ShellView;
use crate::home::{Frame, framed};
use crate::state_pages::crumbs;

/// The page sits at `settings/about`, one level down.
const ROOT: &str = "../";

const CSS: &str = include_str!("../assets/about.css");

/// The About page, inside the shell.
pub(crate) fn about_page(shell: &ShellView, view: &AboutView, generation: u64) -> String {
    let mut b = String::with_capacity(16 * 1024);
    b.push_str(r#"<div class="about"><div class="about-head"><h1>About</h1><p class="muted">This <code>ods</code> and the plugins it runs with, as <code>ods plugin list</code> shows them. Plugins are built in; a custom <code>ods</code> adds its own. Nothing here connects to a warehouse.</p></div>"#);
    let _ = write!(
        b,
        r#"<section class="card" aria-label="This ods"><div class="card-head"><h2>This <code>ods</code></h2><span class="where">ods version</span></div><dl><dt>Version</dt><dd class="mono">{ods}</dd><dt>Plugin SDK</dt><dd class="mono">{sdk}</dd><dt>Dashboard API</dt><dd class="mono">{api}</dd><dt>This project's warehouse</dt><dd>{warehouse}</dd></dl></section>"#,
        ods = text(&view.ods_version),
        sdk = text(&view.sdk_version),
        api = view.api_version,
        warehouse = match &view.warehouse {
            Some(w) => format!("<span class=\"mono\">{}</span>", text(w)),
            None => r#"<span class="muted">not known until the project is read</span>"#.to_owned(),
        },
    );
    warehouses(&mut b, view);
    health_checks(&mut b, view);
    b.push_str("</div>");
    let frame = Frame {
        title: "About",
        crumbs: Some(crumbs(&[("Settings", None), ("About", None)], false)),
        root: ROOT,
        status: None,
        sub: Some("about"),
        search: false,
        css: CSS,
        js: "",
    };
    framed(shell, &frame, &b, generation)
}

fn warehouses(b: &mut String, view: &AboutView) {
    b.push_str(r#"<section class="card" aria-label="Warehouse plugins"><div class="card-head"><h2>Warehouse plugins</h2><span class="where">ods plugin list</span></div>"#);
    if let Some(warehouse) = view.warehouse.as_ref().filter(|_| !view.warehouse_served) {
        let _ = write!(
            b,
            r#"<p class="notice" role="status">No plugin serves <span class="mono">{}</span>, this project's warehouse. ODS still builds through the project's own connection; it only knows less about the warehouse: no source versions, links or observed lineage, and no error patterns of its own.</p>"#,
            text(warehouse)
        );
    }
    if view.warehouses.is_empty() {
        b.push_str(r#"<p class="muted">This <code>ods</code> runs with no warehouse plugins.</p>"#);
    }
    for plugin in &view.warehouses {
        let serves = view.warehouse.as_deref() == Some(plugin.name.as_str());
        row(b, plugin, serves);
    }
    b.push_str(r#"<p class="note">A warehouse plugin never connects: it adds what ODS can know about a warehouse the project already reaches. Features that can't be used with this configuration say why.</p></section>"#);
}

fn health_checks(b: &mut String, view: &AboutView) {
    b.push_str(r#"<section class="card" aria-label="Health-check plugins"><div class="card-head"><h2>Health-check plugins</h2><span class="where">[health.plugins.&lt;id&gt;]</span></div>"#);
    if view.health_checks.is_empty() {
        b.push_str(r#"<p class="muted">None. The built-in checks are configured under <code>[health]</code>; a custom <code>ods</code> can add its own.</p>"#);
    }
    for plugin in &view.health_checks {
        row(b, plugin, false);
    }
    b.push_str("</section>");
}

/// One plugin: who it is on the left, what it offers on the right.
fn row(b: &mut String, plugin: &PluginFacts, serves: bool) {
    let _ = write!(
        b,
        r#"<div class="plugin" data-plugin="{name}"><div class="who"><span class="name mono">{name}</span><span class="from">{from} · {origin}</span>{this}</div><div class="what"><div class="offers">"#,
        name = text(&plugin.name),
        from = text(&plugin.from),
        origin = if plugin.builtin { "built in" } else { "added" },
        this = if serves {
            r#"<span class="this-project">this project's warehouse</span>"#
        } else {
            ""
        },
    );
    for feature in &plugin.features {
        chip(b, feature);
    }
    b.push_str(r#"</div><ul class="notes">"#);
    for feature in &plugin.features {
        if let Some(detail) = &feature.detail {
            let _ = write!(
                b,
                r#"<li><span class="feat">{}</span> {}</li>"#,
                text(&feature.name),
                text(detail)
            );
        }
        if let Some(why) = &feature.unavailable {
            let _ = write!(
                b,
                r#"<li class="off"><span class="feat">{}</span> not usable here: {}</li>"#,
                text(&feature.name),
                text(why)
            );
        }
    }
    if !plugin.parents.is_empty() {
        let _ = write!(
            b,
            r#"<li>Built on <span class="mono">{}</span>{}</li>"#,
            text(&plugin.parents.join(" → ")),
            match plugin.parents_from.as_deref() {
                Some("configuration") =>
                    " (named by <code>[warehouses]</code> <code>extends</code>)",
                Some(_) => " (named by the plugin)",
                None => "",
            },
        );
    }
    for inherited in &plugin.inherited {
        let _ = write!(
            b,
            r#"<li><span class="feat">{feature}</span> from <span class="mono">{from}</span>{detail}</li>"#,
            feature = text(&inherited.feature),
            from = text(&inherited.from),
            detail = inherited
                .detail
                .as_ref()
                .map(|d| format!(": {}", text(d)))
                .unwrap_or_default(),
        );
    }
    b.push_str("</ul></div></div>");
}

/// A feature: its name, and its contract's version when it implements one.
fn chip(b: &mut String, feature: &FeatureFacts) {
    let title = match (&feature.contract, &feature.contract_version) {
        (Some(c), Some(v)) => format!("implements {c} {v}"),
        (Some(c), None) => format!("implements {c}"),
        _ => "no SDK contract".to_owned(),
    };
    let _ = write!(
        b,
        r#"<span class="feature{unusable}" title="{title}">{name}{ver}</span>"#,
        unusable = if feature.unavailable.is_some() {
            " unusable"
        } else {
            ""
        },
        title = html_escape::encode_double_quoted_attribute(&title),
        name = text(&feature.name),
        ver = feature
            .contract_version
            .as_ref()
            .map(|v| format!(r#" <span class="ver">{}</span>"#, text(v)))
            .unwrap_or_default(),
    );
}
