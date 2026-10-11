//! [`PluginProcess`] against a real process: this test binary itself, which, started
//! with `--ods-plugin-fixture <mode>`, plays a plugin that behaves (or misbehaves) as
//! `mode` says. So the tests need neither a shell nor an interpreter on any platform.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::sync::Arc;
use std::time::Duration;

use ods_core::SchemaVersion;
use ods_core::state::{DataVersion, Exactness};
use ods_provider_process::PluginProcess;
use ods_sdk::contracts::changes::{
    CHANGE_PROVIDER, ChangeProvider, RequestedSource, SourceVersion,
};
use ods_sdk::contracts::health_check::{
    CheckFinding, CheckInfo, CheckScope, HEALTH_CHECK, HealthCheck, NodeFacts, Severity, Status,
};
use ods_sdk::protocol::{
    CheckRequest, CheckResponse, DescribeResponse, Provided, SourceAnswer, VersionsRequest,
    VersionsResponse,
};

const FIXTURE: &str = "--ods-plugin-fixture";

/// Plays a plugin, as `mode` says.
fn fixture(mode: &str) {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    let request: serde_json::Value = serde_json::from_slice(&input).unwrap();
    let out = |json: Vec<u8>| std::io::stdout().write_all(&json).unwrap();
    match request["request"].as_str().unwrap() {
        "describe" => {
            let mut answer = DescribeResponse::new("acme", "1.2.0");
            let health = match mode {
                "old-contract" => SchemaVersion::new(0, 0),
                _ => HEALTH_CHECK.version,
            };
            if mode != "checks-without-contract" {
                answer
                    .provides
                    .push(Provided::new(HEALTH_CHECK.name, health));
            }
            answer
                .provides
                .push(Provided::new(CHANGE_PROVIDER.name, CHANGE_PROVIDER.version));
            answer
                .provides
                .push(Provided::new("added_later", SchemaVersion::new(3, 0)));
            answer.checks.push(CheckInfo::new(
                "acme.tagged",
                "every node is tagged",
                Severity::Warn,
            ));
            if mode != "no-warehouse" {
                answer.warehouse = Some("acme_db".to_owned());
            }
            if mode == "major" {
                answer.protocol = ods_sdk::protocol::ProtocolVersion::new(2, 0);
            }
            out(serde_json::to_vec(&answer).unwrap());
        }
        "check" => {
            let request: CheckRequest = serde_json::from_value(request).unwrap();
            let findings = request
                .scope
                .nodes
                .iter()
                .map(|n| {
                    CheckFinding::new(n.id.clone(), Status::Pass, "tagged")
                        .with_evidence("check", request.check.id.clone())
                        .with_evidence(
                            "manifest",
                            request
                                .artifacts
                                .get("manifest")
                                .cloned()
                                .unwrap_or_default(),
                        )
                })
                .collect();
            out(serde_json::to_vec(&CheckResponse::new(findings)).unwrap());
        }
        "versions" => {
            let request: VersionsRequest = serde_json::from_value(request).unwrap();
            let settings: Vec<&String> = request.settings.keys().collect();
            let mut sources: Vec<SourceAnswer> = request
                .sources
                .iter()
                .rev()
                .map(|s| {
                    let version = if s.name.starts_with("unreadable") || s.name.starts_with("none.")
                    {
                        SourceVersion::Unknown("it has no version".to_owned())
                    } else {
                        SourceVersion::Version(DataVersion::new(
                            format!("{}@{settings:?}", s.name),
                            Exactness::Exact,
                            "acme",
                        ))
                    };
                    SourceAnswer::new(s.id.clone(), version)
                })
                .collect();
            match mode {
                "versions-extra" => sources.push(SourceAnswer::new(
                    "source.shop.raw.other",
                    SourceVersion::Unknown("?".to_owned()),
                )),
                "versions-twice" => sources.push(sources[0].clone()),
                "versions-none" => sources.clear(),
                _ => {}
            }
            out(serde_json::to_vec(&VersionsResponse::new(sources)).unwrap());
        }
        other => panic!("no request `{other}`"),
    }
}

fn plugin(mode: &str) -> Arc<PluginProcess> {
    let me = std::env::current_exe().unwrap().display().to_string();
    Arc::new(PluginProcess::new(
        "acme",
        vec![me, FIXTURE.to_owned(), mode.to_owned()],
        std::env::temp_dir(),
        Duration::from_secs(30),
    ))
}

fn sources(names: &[&str]) -> Vec<RequestedSource> {
    names
        .iter()
        .map(|n| RequestedSource::new(format!("source.shop.{n}"), *n))
        .collect()
}

async fn a_plugin_describes_its_checks_and_warehouse() {
    let described = plugin("good").describe().await.unwrap();
    assert_eq!(described.answer.name, "acme");
    assert_eq!(described.answer.version, "1.2.0");
    assert_eq!(described.answer.checks[0].id, "acme.tagged");
    assert_eq!(described.answer.warehouse.as_deref(), Some("acme_db"));
    assert!(described.provides(&HEALTH_CHECK));
    assert!(described.provides(&CHANGE_PROVIDER));
    // A contract this host doesn't know is listed, and otherwise ignored.
    assert_eq!(described.unknown, ["added_later"]);
}

async fn a_contract_built_against_another_version_refuses_the_plugin() {
    let error = plugin("old-contract")
        .describe()
        .await
        .unwrap_err()
        .to_string();
    let ours = format!(
        "{}.{}",
        HEALTH_CHECK.version.major, HEALTH_CHECK.version.minor
    );
    assert!(
        error.contains(&format!(
            "was built against health_check 0.0; this ods has {ours}"
        )),
        "{error}"
    );
}

async fn a_malformed_description_refuses_the_plugin() {
    let error = plugin("checks-without-contract")
        .describe()
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("lists checks but doesn't implement `health_check`"),
        "{error}"
    );
    let error = plugin("no-warehouse")
        .describe()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("without naming its warehouse"), "{error}");
    let error = plugin("major").describe().await.unwrap_err().to_string();
    assert!(error.contains("answered in protocol 2.0"), "{error}");
}

async fn a_plugins_check_asks_it_with_the_projects_artifacts() {
    let acme = plugin("good");
    let described = acme.describe().await.unwrap();
    let artifacts = BTreeMap::from([("manifest".to_owned(), "target/manifest.json".to_owned())]);
    let checks = acme.checks(&described, &artifacts);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].describe().id, "acme.tagged");
    let scope = CheckScope::new(
        vec![NodeFacts::new("model.shop.orders", "orders", "model")],
        None,
    );
    let findings = checks[0].check(&scope).await.unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].status, Status::Pass);
    assert_eq!(findings[0].evidence["check"], "acme.tagged");
    assert_eq!(findings[0].evidence["manifest"], "target/manifest.json");
}

async fn versions_are_answered_in_request_order_with_the_warehouses_settings() {
    let acme = plugin("good");
    let described = acme.describe().await.unwrap();
    let settings = BTreeMap::from([(
        "warehouse".to_owned(),
        serde_json::json!({"kind": "acme_db"}),
    )]);
    let provider = acme.versions(&described, settings).unwrap();
    let asked = sources(&["raw.orders", "unreadable.events", "raw.customers"]);
    let report = provider.versions(&asked).await.unwrap();
    let ids: Vec<&str> = report.sources.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "source.shop.raw.orders",
            "source.shop.unreadable.events",
            "source.shop.raw.customers"
        ]
    );
    assert_eq!(
        report.sources[0].1,
        SourceVersion::Version(DataVersion::new(
            r#"raw.orders@["warehouse"]"#,
            Exactness::Exact,
            "acme"
        ))
    );
    assert!(matches!(report.sources[1].1, SourceVersion::Unknown(_)));
}

async fn an_answer_about_what_wasnt_asked_makes_every_source_unknown() {
    for mode in ["versions-extra", "versions-twice"] {
        let acme = plugin(mode);
        let described = acme.describe().await.unwrap();
        let provider = acme.versions(&described, BTreeMap::new()).unwrap();
        let error = provider
            .versions(&sources(&["raw.orders"]))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("the plugin `acme` answered about"),
            "{mode}: {error}"
        );
    }
    // Sources it didn't answer about are unknown; the call itself isn't an error.
    let acme = plugin("versions-none");
    let described = acme.describe().await.unwrap();
    let provider = acme.versions(&described, BTreeMap::new()).unwrap();
    let report = provider.versions(&sources(&["raw.orders"])).await.unwrap();
    assert!(
        matches!(&report.sources[0].1, SourceVersion::Unknown(why) if why.contains("didn't answer")),
        "{report:?}"
    );
}

/// The SDK's suites pass against a plugin's providers, as `ods plugin test` runs them.
async fn a_plugins_providers_pass_the_sdks_suites() {
    use async_trait::async_trait;
    use ods_sdk::conformance::{changes, health_check};

    struct Versions(Arc<PluginProcess>);
    #[async_trait]
    impl changes::ChangeHarness for Versions {
        async fn provider(&self) -> Arc<dyn ChangeProvider> {
            let described = self.0.describe().await.unwrap();
            Arc::new(self.0.versions(&described, BTreeMap::new()).unwrap())
        }
        fn readable(&self) -> Vec<RequestedSource> {
            sources(&["raw.orders", "raw.customers"])
        }
        async fn commit(&self, _: &RequestedSource) -> bool {
            false
        }
        fn unreadable(&self) -> Option<RequestedSource> {
            sources(&["unreadable.events"]).pop()
        }
    }
    struct Check(Arc<dyn HealthCheck>);
    impl health_check::HealthCheckHarness for Check {
        fn check(&self) -> Arc<dyn HealthCheck> {
            self.0.clone()
        }
        fn scope(&self) -> CheckScope {
            CheckScope::new(
                vec![
                    NodeFacts::new("model.shop.orders", "orders", "model"),
                    NodeFacts::new("model.shop.customers", "customers", "model"),
                ],
                None,
            )
        }
        fn undecidable(&self) -> Option<CheckScope> {
            None
        }
    }
    let acme = plugin("good");
    let report = changes::run(&Versions(acme.clone())).await;
    assert_eq!(report.skipped.len(), 1, "{report:?}");

    let described = acme.describe().await.unwrap();
    let check = acme.checks(&described, &BTreeMap::new()).remove(0);
    let report = health_check::run(&Check(Arc::new(check))).await;
    assert_eq!(report.skipped.len(), 1, "{report:?}");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(FIXTURE) {
        fixture(&args[2]);
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // The test harness's own flags (e.g. `--quiet`, a name filter) are ignored: every
    // test runs.
    macro_rules! tests {
        ($($name:ident),* $(,)?) => {
            $(
                runtime.block_on($name());
                println!("test {} ... ok", stringify!($name));
            )*
        };
    }
    tests!(
        a_plugin_describes_its_checks_and_warehouse,
        a_contract_built_against_another_version_refuses_the_plugin,
        a_malformed_description_refuses_the_plugin,
        a_plugins_check_asks_it_with_the_projects_artifacts,
        versions_are_answered_in_request_order_with_the_warehouses_settings,
        an_answer_about_what_wasnt_asked_makes_every_source_unknown,
        a_plugins_providers_pass_the_sdks_suites,
    );
}
