//! The Settings page's HTML (#351), rendered on the server from [`SettingsView`]
//! inside the dashboard's shell. Static: no script, and nothing that writes.

use std::fmt::Write as _;

use html_escape::{encode_double_quoted_attribute as attr, encode_text as text};
use ods_core::CheckResult;

use crate::dashboard::ShellView;
use crate::home::{Frame, framed};
use crate::settings::{Resolved, SettingsView};
use crate::state_pages::crumbs;

const CSS: &str = include_str!("../assets/settings.css");

/// What stands for a value left out beyond loopback.
const HIDDEN: &str = r#"<span class="muted" title="Shown on this machine only">hidden</span>"#;

/// The Settings page, inside the shell.
pub(crate) fn settings_page(shell: &ShellView, view: &SettingsView, generation: u64) -> String {
    let s = &view.settings;
    let mut b = String::with_capacity(32 * 1024);
    b.push_str(
        r#"<div class="settings"><div class="settings-head"><h1>Settings</h1><p class="muted">"#,
    );
    loaded_from(&mut b, view);
    b.push_str(" Change them in the files or the environment; the dashboard never writes configuration.</p></div>");
    if let Some(why) = &s.unreadable {
        let _ = write!(
            b,
            r#"<p class="notice bad" role="alert">The configuration couldn't be read{}. <code>ods doctor</code> says why.</p>"#,
            if why.is_empty() {
                String::new()
            } else {
                format!(": {}", text(why))
            }
        );
    }
    if !view.details {
        b.push_str(r#"<p class="notice" role="status">Served beyond this machine: values, paths and the checks' details are shown on this machine only.</p>"#);
    }
    b.push_str(r#"<div class="settings-grid">"#);
    resolved_card(
        &mut b,
        "Project and target",
        "[providers.*.settings]",
        &s.project,
    );
    resolved_card(&mut b, "State store", "[state]", &s.state);
    secrets(&mut b, view);
    providers(&mut b, view);
    b.push_str("</div>");
    entries(&mut b, view);
    checks(&mut b, &s.checks, view.details);
    server_mode(&mut b);
    b.push_str("</div>");
    let frame = Frame {
        title: "Settings",
        crumbs: Some(crumbs(&[("Settings", None)], false)),
        root: "",
        status: Some(
            r#"<span class="pill-snap" title="Nothing here writes"><span class="dot none"></span>read-only view of the configuration</span>"#
                .to_owned(),
        ),
        sub: Some("configuration"),
        search: false,
        css: CSS,
        js: "",
    };
    framed(shell, &frame, &b, generation)
}

/// "Loaded from …, profile …."
fn loaded_from(b: &mut String, view: &SettingsView) {
    let s = &view.settings;
    let loaded: Vec<String> = s
        .files
        .iter()
        .filter(|f| f.loaded)
        .map(|f| {
            if view.details {
                format!(
                    r#"<span class="mono" title="{kind} file">{path}</span>"#,
                    kind = attr(&f.kind),
                    path = text(&f.path)
                )
            } else {
                format!("the {} file", text(&f.kind))
            }
        })
        .collect();
    if loaded.is_empty() {
        b.push_str("No configuration file: built-in defaults, the environment and flags apply.");
    } else {
        let _ = write!(b, "Loaded from {}", loaded.join(", "));
        match &s.profile {
            Some(p) => {
                let _ = write!(
                    b,
                    ", profile <strong>{}</strong> (from {}).",
                    text(&p.name),
                    text(&p.selected_by)
                );
            }
            None => b.push('.'),
        }
    }
}

fn resolved_card(b: &mut String, title: &str, keys: &str, rows: &[Resolved]) {
    let _ = write!(
        b,
        r#"<section class="card" aria-label="{t}"><div class="card-head"><h2>{t}</h2><span class="where">{k}</span></div><dl>"#,
        t = text(title),
        k = text(keys),
    );
    for row in rows {
        let value = match &row.value {
            Some(v) => format!(r#"<span class="mono">{}</span>"#, text(v)),
            None if row.origin.is_some() => HIDDEN.to_owned(),
            None => r#"<span class="muted">not set</span>"#.to_owned(),
        };
        let _ = write!(
            b,
            r#"<dt>{label}</dt><dd>{value}<span class="key">{key}{origin}</span></dd>"#,
            label = text(&row.label),
            key = text(&row.key),
            origin = row
                .origin
                .as_ref()
                .map(|o| format!(" · {}", text(o)))
                .unwrap_or_default(),
        );
    }
    b.push_str("</dl></section>");
}

fn secrets(b: &mut String, view: &SettingsView) {
    b.push_str(r#"<section class="card" aria-label="Secrets"><div class="card-head"><h2>Secrets</h2><span class="where">references only</span></div>"#);
    let secrets: Vec<_> = view.settings.entries.iter().filter(|e| e.secret).collect();
    if secrets.is_empty() {
        b.push_str(r#"<p class="muted">No credential is configured here.</p>"#);
    }
    for entry in secrets {
        let _ = write!(
            b,
            r#"<div class="secret"><span class="key">{key}</span><span class="mono">{value}</span></div>"#,
            key = text(&entry.key),
            value = if view.details {
                text(&entry.value).into_owned()
            } else {
                HIDDEN.to_owned()
            },
        );
    }
    b.push_str(r#"<p class="note"><strong>Values are never stored or shown.</strong> ODS keeps the reference; the value is read from where it points only when a command needs it, and never written to state, events or logs.</p></section>"#);
}

fn providers(b: &mut String, view: &SettingsView) {
    b.push_str(r#"<section class="card" aria-label="Providers"><div class="card-head"><h2>Providers</h2><span class="where">[providers.*]</span></div>"#);
    if view.settings.providers.is_empty() {
        b.push_str(r#"<p class="muted">None configured: the defaults apply.</p>"#);
    }
    for p in &view.settings.providers {
        let _ = write!(
            b,
            r#"<div class="provider" data-provider="{name_attr}"><div><span class="name">{name}</span><span class="key">kind = "{kind}"</span></div><div class="caps">"#,
            name_attr = attr(&p.name),
            name = text(&p.name),
            kind = text(&p.kind),
        );
        for capability in &p.capabilities {
            let _ = write!(b, r#"<span class="cap">{}</span>"#, text(capability));
        }
        if p.warehouse_plugin {
            b.push_str(r#"<a href="settings/about">what its warehouse plugin offers</a>"#);
        } else if p.capabilities.is_empty() {
            b.push_str(r#"<span class="muted">no plugin for this kind: nothing more is known about it</span>"#);
        }
        b.push_str("</div></div>");
    }
    b.push_str(
        r#"<p class="note">Planners choose by capability, never by provider name.</p></section>"#,
    );
}

fn entries(b: &mut String, view: &SettingsView) {
    let entries = &view.settings.entries;
    let _ = write!(
        b,
        r#"<section class="card" aria-label="Effective configuration"><div class="card-head"><h2>Effective configuration</h2><span class="where">ods config explain</span></div>"#
    );
    if entries.is_empty() {
        b.push_str(r#"<p class="muted">No value is set: built-in defaults apply.</p></section>"#);
        return;
    }
    b.push_str(r#"<table class="settings-table"><thead><tr><th>Key</th><th>Value</th><th>Source</th></tr></thead><tbody>"#);
    for entry in entries {
        let value = if view.details {
            format!(r#"<span class="mono">{}</span>"#, text(&entry.value))
        } else {
            HIDDEN.to_owned()
        };
        let _ = write!(
            b,
            r#"<tr data-key="{key_attr}"><td class="mono">{key}</td><td>{value}</td><td title="{full}">{source}{over}</td></tr>"#,
            key_attr = attr(&entry.key),
            key = text(&entry.key),
            full = attr(&entry.source),
            source = text(if entry.source_short.is_empty() {
                &entry.source
            } else {
                &entry.source_short
            }),
            over = match entry.overrides {
                0 => String::new(),
                n => format!(r#" <span class="muted">(over {n} other)</span>"#),
            },
        );
    }
    b.push_str("</tbody></table></section>");
}

fn checks(b: &mut String, checks: &[CheckResult], details: bool) {
    b.push_str(r#"<section class="card" aria-label="Checks"><div class="card-head"><h2>Checks</h2><span class="where">ods doctor</span></div>"#);
    b.push_str(r#"<p class="note">The checks that only read: the configuration, the project's files and the state store. <code>ods doctor</code> also checks the tools, the target and, with <code>--connect</code>, the warehouse.</p><ul class="checks">"#);
    for check in checks {
        let status = check.status.name();
        let _ = write!(
            b,
            r#"<li data-check="{id_attr}" data-status="{status}"><span class="status {status}">{status}</span><span class="mono">{id}</span>"#,
            id_attr = attr(&check.id),
            id = text(&check.id),
        );
        if details {
            let _ = write!(b, r#"<span class="msg">{}</span>"#, text(&check.message));
            if let Some(hint) = &check.hint {
                let _ = write!(b, r#"<span class="hint">{}</span>"#, text(hint));
            }
        }
        b.push_str("</li>");
    }
    b.push_str("</ul></section>");
}

fn server_mode(b: &mut String) {
    b.push_str(r#"<section class="card planned-card" aria-label="Server mode"><div class="card-head"><h2>Server mode <span class="chip">Planned</span></h2><span class="where">Not in <code>ods serve</code> today; nothing here can be turned on.</span></div><div class="planned-grid">"#);
    for (title, body) in [
        (
            "Identity & access",
            "Sign-in and roles for a shared server. Local <code>ods serve</code> needs neither.",
        ),
        (
            "Webhooks",
            "Event sinks that post run and plan events to a URL you choose.",
        ),
        (
            "Audit log",
            "An event sink recording who ran or exported what, and when.",
        ),
    ] {
        let _ = write!(
            b,
            r#"<div class="planned-item"><h3>{title}<span class="chip">Planned</span></h3><p class="muted">{body}</p></div>"#
        );
    }
    b.push_str(r#"</div><p class="note">Webhooks and the audit log are event sinks on the same event stream the CLI already emits, not a second record.</p></section>"#);
}
