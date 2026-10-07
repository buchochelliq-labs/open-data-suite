//! The Settings page (#351): the configuration as the binary resolved it, shown
//! read-only; on loopback with its values, beyond it without values, paths or the
//! checks' details.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};

use ods_core::{CheckCategory, CheckResult, Evidence};
use ods_lineage::{GraphFilter, LineageProject, MemoryCache, build};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_web::about::{AboutInput, PluginFacts, PluginKind};
use ods_web::settings::{
    FileFacts, ProfileFacts, ProviderFacts, Resolved, SettingEntry, SettingsInput,
};
use ods_web::{Dashboard, ServeOptions, Snapshot, router};

const HOSTILE: &str = "</script><img src=x onerror=alert(1)>";
/// A path that must not leave this machine.
const PRIVATE: &str = "/home/someone/private-project";

fn input() -> SettingsInput {
    let mut input = SettingsInput::default();
    input.profile = Some(ProfileFacts::new("dev", "ODS_PROFILE"));
    input.files = vec![
        FileFacts::new("user", "/home/someone/.config/ods/config.toml", false),
        FileFacts::new("project", format!("{PRIVATE}/ods.toml"), true),
    ];
    let mut target = SettingEntry::new(
        "providers.dbt.settings.target",
        "\"dev\"",
        format!("project file {PRIVATE}/ods.toml"),
    );
    target.source_short = "project file".into();
    target.overrides = 1;
    let mut secret = SettingEntry::new(
        "providers.uc.settings.client_secret",
        "secret(env:DATABRICKS_CLIENT_SECRET)",
        format!("project file {PRIVATE}/ods.toml"),
    );
    secret.source_short = "project file".into();
    secret.secret = true;
    let mut hostile = SettingEntry::new(format!("providers.{HOSTILE}.kind"), "\"x\"", "flag --x");
    hostile.source_short = "flag --x".into();
    input.entries = vec![target, secret, hostile];
    input.project = vec![
        Resolved::new(
            "Project dir",
            "providers.dbt.settings.project_dir",
            Some(PRIVATE.into()),
            Some("flag".into()),
        )
        .path(),
        Resolved::new("Profile", "providers.dbt.settings.profile", None, None),
    ];
    input.state = vec![Resolved::new(
        "Environment",
        "state.environment",
        Some("dev".into()),
        Some("target".into()),
    )];
    let mut dbt = ProviderFacts::new("dbt", "dbt");
    dbt.capabilities = vec!["relation_existence".into()];
    input.providers = vec![
        dbt,
        ProviderFacts::new("uc", "databricks"),
        ProviderFacts::new("pg", "postgres"),
    ];
    input.checks = vec![
        CheckResult::ok(
            "config.load",
            CheckCategory::Config,
            "1 configuration file(s) loaded",
        )
        .evidence(Evidence::new("file", format!("{PRIVATE}/ods.toml"))),
        CheckResult::error(
            "project.dbt_project",
            CheckCategory::Project,
            "ODS-E0101",
            format!("no dbt project in {PRIVATE}"),
        )
        .hint("run ODS in your dbt project"),
    ];
    input
}

fn dashboard() -> Dashboard {
    Dashboard::new("shop", "dev")
        .with_settings(input())
        .with_about(AboutInput::new(
            "0.0.3",
            "0.9",
            vec![PluginFacts::new(
                "databricks",
                PluginKind::Warehouse,
                "ods-provider-databricks 0.0.3",
                true,
            )],
        ))
}

fn start(dashboard: Dashboard, addr: [u8; 4]) -> SocketAddr {
    start_with(dashboard, &ServeOptions::new((addr, 0).into()))
}

fn start_with(dashboard: Dashboard, options: &ServeOptions) -> SocketAddr {
    let (graph, _) = build(
        &LineageProject::new(vec![]),
        &FakeSqlLineageAnalyzer::new(),
        &MemoryCache::default(),
    )
    .unwrap();
    let document = graph.document(&|id: &str| id.to_owned(), &GraphFilter::default());
    let snapshot = Snapshot::new(document, graph, "fixture").with_dashboard(dashboard);
    let app = router(snapshot, options);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

/// GET `path`: (status, head, body).
fn get(addr: SocketAddr, path: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n",
        addr.port()
    )
    .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let response = String::from_utf8_lossy(&bytes);
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (
        head.split(' ').nth(1).unwrap().parse().unwrap(),
        head.to_owned(),
        body.to_owned(),
    )
}

#[test]
fn on_loopback_the_view_is_the_input() {
    let view = dashboard().settings(true);
    assert!(view.details);
    let mut expected = input();
    // A provider whose kind a warehouse plugin serves says so.
    expected.providers[1].warehouse_plugin = true;
    assert_eq!(view.settings, expected);
}

#[test]
fn beyond_loopback_no_value_path_or_check_detail_is_kept() {
    let view = dashboard().settings(false);
    assert!(!view.details);
    let json = serde_json::to_string(&view).unwrap();
    assert!(!json.contains(PRIVATE), "{json}");
    assert!(!json.contains("DATABRICKS_CLIENT_SECRET"), "{json}");
    // What remains: keys, short sources, names and statuses.
    let s = &view.settings;
    assert_eq!(s.entries[0].key, "providers.dbt.settings.target");
    assert_eq!(s.entries[0].source, "project file");
    assert_eq!(s.entries[0].value, "");
    assert!(s.project.iter().all(|r| r.value.is_none()));
    assert_eq!(s.checks[1].id, "project.dbt_project");
    assert_eq!(s.checks[1].message, "");
    assert!(
        s.checks
            .iter()
            .all(|c| c.evidence.is_empty() && c.hint.is_none())
    );
}

#[test]
fn the_api_serves_the_view() {
    let addr = start(dashboard(), [127, 0, 0, 1]);
    let (status, _, body) = get(addr, "/api/settings");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        json,
        serde_json::to_value(dashboard().settings(true)).unwrap()
    );
}

#[test]
fn the_page_shows_each_part_and_escapes_keys() {
    let addr = start(dashboard(), [127, 0, 0, 1]);
    let (status, _, page) = get(addr, "/settings");
    assert_eq!(status, 200);
    assert!(!page.contains(HOSTILE), "a key is text, never markup");
    for expected in [
        r"profile <strong>dev</strong> (from ODS_PROFILE)",
        r#"<section class="card" aria-label="Project and target">"#,
        r#"<section class="card" aria-label="State store">"#,
        // A secret, by its reference only.
        r#"<span class="mono">secret(env:DATABRICKS_CLIENT_SECRET)</span>"#,
        r#"<span class="cap">relation_existence</span>"#,
        // A kind a warehouse plugin serves leads to About; one no plugin serves says so.
        r#"<a href="settings/about">what its warehouse plugin offers</a>"#,
        "no plugin for this kind",
        r#"<tr data-key="providers.dbt.settings.target">"#,
        r"(over 1 other)",
        r#"<li data-check="project.dbt_project" data-status="error">"#,
        "run ODS in your dbt project",
        r#"aria-label="Server mode""#,
        // Nothing to submit.
    ] {
        assert!(page.contains(expected), "{expected}\n{page}");
    }
    assert!(!page.contains("<form"), "nothing on the page writes");
    // Settings is current, with Configuration its current page.
    assert!(
        page.contains(r#"<a href="settings" aria-current="page" data-section="settings">"#),
        "{page}"
    );
    assert!(
        page.contains(
            r#"<a href="settings" aria-current="page" data-item="configuration">Configuration</a>"#
        ),
        "{page}"
    );
    assert!(page.contains(r#"<a href="settings/about" data-item="about">About</a>"#));
}

#[test]
fn beyond_loopback_the_page_says_what_it_leaves_out() {
    let addr = start(dashboard(), [0, 0, 0, 0]);
    let (status, _, page) = get(addr, "/settings");
    assert_eq!(status, 200);
    assert!(!page.contains(PRIVATE), "{page}");
    assert!(!page.contains("DATABRICKS_CLIENT_SECRET"), "{page}");
    assert!(page.contains("shown on this machine only"), "{page}");
    assert!(page.contains(r#"<tr data-key="providers.dbt.settings.target">"#));
}

#[test]
fn a_trailing_slash_leads_to_the_page() {
    let addr = start(dashboard(), [127, 0, 0, 1]);
    let (status, head, _) = get(addr, "/settings/");
    assert_eq!(status, 308);
    assert!(
        head.to_ascii_lowercase().contains("location: /settings\r"),
        "{head}"
    );
}

#[test]
fn behind_a_proxy_on_loopback_nothing_private_is_shown_either() {
    // Bound to loopback, but `--allow-host` names a proxy's host: requests may come
    // from anywhere.
    let options = ServeOptions::new(([127, 0, 0, 1], 0).into())
        .with_allowed_hosts(["ods.example.com".to_owned()]);
    let addr = start_with(dashboard(), &options);
    let (status, _, body) = get(addr, "/api/settings");
    assert_eq!(status, 200, "{body}");
    assert!(!body.contains(PRIVATE), "{body}");
    assert!(!body.contains("DATABRICKS_CLIENT_SECRET"), "{body}");
    let (_, _, page) = get(addr, "/settings");
    assert!(!page.contains(PRIVATE), "{page}");
    assert!(page.contains("shown on this machine only"), "{page}");
}
