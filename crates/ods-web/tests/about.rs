//! The About page (ADR-0031 §3c): this `ods` and its plugins, as the binary detected
//! them; its JSON API, HTML and place in the navigation.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};

use ods_lineage::{GraphFilter, LineageProject, MemoryCache, build};
use ods_provider_fake::FakeSqlLineageAnalyzer;
use ods_web::about::{AboutInput, FeatureFacts, InheritedFacts, PluginFacts, PluginKind};
use ods_web::dashboard::Target;
use ods_web::{Dashboard, ServeOptions, Snapshot, router};

const HOSTILE: &str = "</script><img src=x onerror=alert(1)>";

/// What the released `ods` detects, roughly, and a check a custom `ods` added.
fn plugins() -> Vec<PluginFacts> {
    let mut databricks = PluginFacts::new(
        "databricks",
        PluginKind::Warehouse,
        "ods-provider-databricks 0.0.2",
        true,
    );
    let mut versions = FeatureFacts::new("source_versions");
    versions.contract = Some("change_provider".into());
    versions.contract_version = Some("0.3".into());
    versions.detail = Some("Delta table versions".into());
    let mut links = FeatureFacts::new("links");
    links.contract = Some("relation_linker".into());
    links.contract_version = Some("0.1".into());
    links.unavailable = Some("no `host` is set for the databricks provider".into());
    let mut dialect = FeatureFacts::new("dialect");
    dialect.detail = Some("databricks".into());
    databricks.features = vec![versions, links, dialect];

    let mut lakehouse = PluginFacts::new(
        "lakehouse",
        PluginKind::Warehouse,
        "acme-lakehouse 1.2.0",
        false,
    );
    lakehouse.parents = vec!["databricks".into()];
    lakehouse.parents_from = Some("plugin".into());
    let mut errors = InheritedFacts::new("errors", "databricks");
    errors.detail = Some("databricks catalogue 3".into());
    lakehouse.inherited = vec![errors];

    let mut check = PluginFacts::new(HOSTILE, PluginKind::HealthCheck, "acme-checks 0.4.0", false);
    let mut health = FeatureFacts::new("health_check");
    health.contract = Some("health_check".into());
    health.contract_version = Some("0.2".into());
    check.features = vec![health];
    // A custom plugin's kind is whatever it says.
    let quoted = PluginFacts::new(QUOTED, PluginKind::Warehouse, "acme-odd 0.1.0", false);
    vec![check, databricks, lakehouse, quoted]
}

const QUOTED: &str = r#"odd" onmouseover="alert(1)"#;

fn dashboard(warehouse: &str) -> Dashboard {
    Dashboard::new("shop", "dev")
        .with_target(Some(Target::new("dev", Some(warehouse.into()))))
        .with_about(AboutInput::new("0.0.3", "0.1", plugins()))
}

fn start(dashboard: Dashboard) -> SocketAddr {
    let (graph, _) = build(
        &LineageProject::new(vec![]),
        &FakeSqlLineageAnalyzer::new(),
        &MemoryCache::default(),
    )
    .unwrap();
    let document = graph.document(&|id: &str| id.to_owned(), &GraphFilter::default());
    let snapshot = Snapshot::new(document, graph, "fixture").with_dashboard(dashboard);
    let app = router(snapshot, &ServeOptions::new(([127, 0, 0, 1], 0).into()));
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
fn the_view_splits_warehouses_from_checks_and_names_the_projects_warehouse() {
    let view = dashboard("databricks").about();
    assert_eq!(view.ods_version, "0.0.3");
    assert_eq!(view.sdk_version, "0.1");
    assert_eq!(view.api_version, ods_web::API_VERSION);
    assert_eq!(view.warehouse.as_deref(), Some("databricks"));
    assert!(view.warehouse_served);
    let names =
        |plugins: &[PluginFacts]| plugins.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&view.warehouses), ["databricks", "lakehouse", QUOTED]);
    assert_eq!(names(&view.health_checks), [HOSTILE]);

    let view = dashboard("snowflake").about();
    assert!(!view.warehouse_served, "no plugin serves snowflake");
    // Before the project is read, its warehouse isn't known, and nothing is claimed.
    let view = Dashboard::new("shop", "dev").about();
    assert_eq!(view.warehouse, None);
    assert!(!view.warehouse_served);
    assert_eq!(view.warehouses, []);
}

#[test]
fn the_api_serves_the_view() {
    let addr = start(dashboard("databricks"));
    let (status, _, body) = get(addr, "/api/settings/about");
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        json,
        serde_json::to_value(dashboard("databricks").about()).unwrap()
    );
    assert_eq!(
        json["warehouses"][0]["features"][1]["unavailable"],
        "no `host` is set for the databricks provider"
    );
}

#[test]
fn the_page_shows_each_plugin_and_escapes_what_plugins_say() {
    let addr = start(dashboard("databricks"));
    let (status, _, page) = get(addr, "/settings/about");
    assert_eq!(status, 200);
    assert!(
        !page.contains(HOSTILE),
        "a plugin's name is text, never markup"
    );
    assert!(page.contains("&lt;/script&gt;&lt;img src=x onerror=alert(1)&gt;"));
    // A quote in a warehouse's name can't end the attribute it is put in.
    assert!(!page.contains(r#"onmouseover="alert(1)""#), "{page}");
    assert!(
        page.contains(r#"data-plugin="odd&quot; onmouseover=&quot;alert(1)""#),
        "{page}"
    );
    for expected in [
        r#"data-plugin="databricks""#,
        "ods-provider-databricks 0.0.2 · built in",
        "acme-lakehouse 1.2.0 · added",
        "this project's warehouse",
        // An offered feature that can't be used says why.
        r#"<span class="feature unusable" title="implements relation_linker 0.1">links"#,
        "not usable here: no `host` is set for the databricks provider",
        "Delta table versions",
        r#"Built on <span class="mono">databricks</span> (named by the plugin)"#,
        "databricks catalogue 3",
    ] {
        assert!(page.contains(expected), "{expected}\n{page}");
    }
    assert!(!page.contains("No plugin serves"), "databricks is served");
    // Settings is current, with About its current page beside Configuration.
    assert!(
        page.contains(r#"<a href="../settings" aria-current="page" data-section="settings">"#),
        "{page}"
    );
    assert!(page.contains(r#"data-item="about">About</a>"#));
    assert!(page.contains(r#"<a href="../settings" data-item="configuration">Configuration</a>"#));
}

#[test]
fn a_warehouse_no_plugin_serves_still_works_and_says_what_odss_knows_less_about() {
    let addr = start(dashboard("snowflake"));
    let (_, _, page) = get(addr, "/settings/about");
    assert!(
        page.contains(
            r#"No plugin serves <span class="mono">snowflake</span>, this project's warehouse."#
        ),
        "{page}"
    );
    assert!(
        !page.contains("this-project\""),
        "no plugin is marked as this project's"
    );
}

#[test]
fn every_page_links_settings() {
    let addr = start(dashboard("databricks"));
    let (_, _, home) = get(addr, "/");
    assert!(
        home.contains(r#"<a href="settings" data-section="settings">"#),
        "{home}"
    );
}
