//! A warehouse plugin's requests out of process (ADR-0031 §3d) against a real process:
//! this test binary itself, which, started with `--ods-warehouse-fixture <mode>`, plays
//! a warehouse plugin: links built from three-part names, a catalogue that recognises
//! one message, lineage read from an export, and probes and privileges over a small
//! pretend warehouse. The SDK's suites then run against what it answers.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ods_core::{ColumnRef, RelationName};
use ods_provider_process::{AnsweredCatalogue, AnsweredLinker, PluginProcess};
use ods_sdk::contracts::error_catalogue::{CatalogueInfo, Classification, ErrorCatalogue};
use ods_sdk::contracts::observed_lineage::ObservedLineage;
use ods_sdk::contracts::privileges::{Access, RELATION_PRIVILEGES, RelationPrivileges};
use ods_sdk::contracts::probe::{
    ProbeAnswer, ProbeFilter, ProbeRequest, ProbeStatement, ProbeTarget, RELATION_PROBE,
    RelationProbe,
};
use ods_sdk::contracts::relation_link::{
    NoRelationLink, RELATION_LINKER, RelationLink, RelationLinker, path_segment, split_relation,
};
use ods_sdk::contracts::run_events::ErrorSummary;
use ods_sdk::protocol::{
    AccessAnswer, ClassifyRequest, ClassifyResponse, DescribeResponse, LinkAnswer, LinkRequest,
    LinkResponse, ObservedLineageRequest, ObservedLineageResponse, PrivilegesRequest,
    PrivilegesResponse, ProbeCall, ProbeResponse, ProbeTargetAnswer, Provided,
};

const FIXTURE: &str = "--ods-warehouse-fixture";

/// The link a three-part name gets, or why it gets none.
fn link_for(relation: &str, configured: bool) -> Result<RelationLink, NoRelationLink> {
    if !configured {
        return Err(NoRelationLink::NotConfigured {
            setting: "account".to_owned(),
        });
    }
    let parts = split_relation(relation, '"').map_err(|why| NoRelationLink::InvalidName {
        relation: relation.to_owned(),
        why,
    })?;
    if parts.len() != 3 {
        return Err(NoRelationLink::NotQualified {
            relation: relation.to_owned(),
            parts: parts.len(),
            needed: 3,
        });
    }
    let segments = parts
        .iter()
        .map(|p| path_segment(p))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|why| NoRelationLink::InvalidName {
            relation: relation.to_owned(),
            why,
        })?;
    Ok(RelationLink::new(
        format!("https://acme.example/explore/{}", segments.join("/")),
        "Open in Acme",
    ))
}

/// Plays a warehouse plugin, as `mode` says.
#[allow(
    clippy::too_many_lines,
    reason = "one pretend plugin: an arm per request, read in one place"
)]
fn fixture(mode: &str) {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    let request: serde_json::Value = serde_json::from_slice(&input).unwrap();
    let out = |json: Vec<u8>| std::io::stdout().write_all(&json).unwrap();
    match request["request"].as_str().unwrap() {
        "describe" => {
            let mut answer = DescribeResponse::new("acme", "1.2.0");
            for contract in [
                RELATION_LINKER,
                ods_sdk::contracts::error_catalogue::ERROR_CATALOGUE,
                ods_sdk::contracts::observed_lineage::OBSERVED_LINEAGE_SOURCE,
                RELATION_PROBE,
                RELATION_PRIVILEGES,
            ] {
                if mode == "privileges-alone" && contract.name == RELATION_PROBE.name {
                    continue;
                }
                answer
                    .provides
                    .push(Provided::new(contract.name, contract.version));
            }
            answer.warehouse = Some("acme_db".to_owned());
            answer.dialect = Some("ansi".to_owned());
            if mode != "no-catalogue" {
                answer.catalogue = Some(CatalogueInfo::new("acme", "1", "Acme"));
            }
            out(serde_json::to_vec(&answer).unwrap());
        }
        "link" => {
            let request: LinkRequest = serde_json::from_value(request).unwrap();
            let configured = request.settings.values().any(|s| s["account"] == "acme");
            let mut links: Vec<LinkAnswer> = request
                .relations
                .iter()
                .map(|r| LinkAnswer::new(r.clone(), link_for(r, configured)))
                .collect();
            match mode {
                "unsafe-link" => {
                    links[0] = LinkAnswer::new(
                        request.relations[0].clone(),
                        Ok(RelationLink::new("javascript:alert(1)", "Open")),
                    );
                }
                "link-unasked" => links.push(LinkAnswer::new(
                    "\"other\"",
                    Err(NoRelationLink::NoRelation),
                )),
                "link-missing" => {
                    links.pop();
                }
                _ => {}
            }
            out(serde_json::to_vec(&LinkResponse::new(links)).unwrap());
        }
        "classify" => {
            let request: ClassifyRequest = serde_json::from_value(request).unwrap();
            let mut classifications: Vec<Classification> = request
                .errors
                .iter()
                .map(|e| {
                    if e.message().contains("no such table") {
                        serde_json::from_value(serde_json::json!({
                            "kind": "recognised", "id": "acme-missing-table",
                            "symptom": "missing_relation", "category": "database",
                            "suggestions": []
                        }))
                        .unwrap()
                    } else {
                        serde_json::from_value(
                            serde_json::json!({"kind": "not_recognised", "category": "unknown"}),
                        )
                        .unwrap()
                    }
                })
                .collect();
            if mode == "classify-short" {
                classifications.pop();
            }
            out(serde_json::to_vec(&ClassifyResponse::new(classifications)).unwrap());
        }
        "observed_lineage" => {
            let request: ObservedLineageRequest = serde_json::from_value(request).unwrap();
            let text = std::fs::read_to_string(&request.export).unwrap();
            let mut lineage = ObservedLineage::default();
            for line in text.lines() {
                let (from, to) = line.split_once(" -> ").unwrap();
                let col = |s: &str| {
                    let (rel, col) = s.rsplit_once('.').unwrap();
                    ColumnRef::new(RelationName::new(rel.split('.')).unwrap(), col)
                };
                lineage.add_column_edge(col(from), col(to));
            }
            out(serde_json::to_vec(&ObservedLineageResponse::new(lineage)).unwrap());
        }
        "probe" => {
            let call: ProbeCall = serde_json::from_value(request).unwrap();
            let targets = call
                .targets
                .iter()
                .map(|t| {
                    // Only the shop's relations exist in this pretend warehouse.
                    let answer = if !t.id.starts_with("model.shop.") {
                        ProbeAnswer::Unknown("no such relation".to_owned())
                    } else if t.id.contains("view") {
                        ProbeAnswer::Skipped("a view, and the filter asks for tables".to_owned())
                    } else if t
                        .relation
                        .as_deref()
                        .is_some_and(|r| r.contains("elsewhere"))
                    {
                        ProbeAnswer::Unknown("its relation is another one".to_owned())
                    } else {
                        ProbeAnswer::Rows(
                            call.probe
                                .statements()
                                .iter()
                                .map(|s| {
                                    s.columns()
                                        .iter()
                                        .map(|c| (c.clone(), "3".to_owned()))
                                        .collect()
                                })
                                .collect(),
                        )
                    };
                    ProbeTargetAnswer::new(t.id.clone(), answer)
                })
                .collect();
            out(serde_json::to_vec(&ProbeResponse::new(targets)).unwrap());
        }
        "privileges" => {
            let request: PrivilegesRequest = serde_json::from_value(request).unwrap();
            let targets = request
                .targets
                .iter()
                .map(|t| {
                    let access = if !t.id.starts_with("model.shop.") {
                        Access::Unknown("no such relation".to_owned())
                    } else if t.id.contains("owned") {
                        Access::Elevated(vec!["OWNERSHIP".to_owned()])
                    } else {
                        Access::ReadOnly
                    };
                    AccessAnswer::new(t.id.clone(), access)
                })
                .collect();
            out(serde_json::to_vec(&PrivilegesResponse::new(
                Some("health_ro".to_owned()),
                targets,
            ))
            .unwrap());
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

fn configured() -> ods_sdk::protocol::WarehouseSettings {
    BTreeMap::from([(
        "warehouse".to_owned(),
        serde_json::json!({"account": "acme"}),
    )])
}

fn summary(message: &str) -> ErrorSummary {
    ErrorSummary::from_message(message).unwrap()
}

async fn the_handshake_checks_what_a_warehouse_needs() {
    let described = plugin("good").describe().await.unwrap();
    assert!(described.provides(&RELATION_LINKER));
    assert_eq!(described.answer.dialect.as_deref(), Some("ansi"));
    let error = plugin("no-catalogue")
        .describe()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("without naming its catalogue"), "{error}");
    // Only the connection that probes can say what its login may do.
    let error = plugin("privileges-alone")
        .describe()
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("without `relation_probe`"), "{error}");
}

/// The relations the SDK's link suite asks about, quoted as ANSI SQL quotes them.
struct Linking {
    linker: Arc<dyn RelationLinker>,
    unconfigured: Arc<dyn RelationLinker>,
}

const QUALIFIED: &str = "\"shop\".\"main\".\"orders\"";
const UNDER: &str = "\"main\".\"orders\"";
const AWKWARD: &str = "\"my shop\".\"ma?in\".\"or#d/e%rs\"";
const MALFORMED: [&str; 5] = [
    "\"shop\".\"main\".\".\"",
    "\"shop\".\"main\".\"..\"",
    "\"shop\".\"main\".\"\"",
    "\"shop\".\"main\".\"orders\"x",
    "shop.main.ord ers",
];

impl ods_sdk::conformance::relation_link::RelationLinkHarness for Linking {
    fn linker(&self) -> Arc<dyn RelationLinker> {
        self.linker.clone()
    }
    fn unconfigured(&self) -> Option<Arc<dyn RelationLinker>> {
        Some(self.unconfigured.clone())
    }
    fn qualified(&self) -> String {
        QUALIFIED.to_owned()
    }
    fn under_qualified(&self) -> String {
        UNDER.to_owned()
    }
    fn awkward(&self) -> String {
        AWKWARD.to_owned()
    }
    fn malformed(&self) -> Vec<String> {
        MALFORMED.iter().map(|m| (*m).to_owned()).collect()
    }
}

async fn links_asked_in_one_call_pass_the_sdks_suite() {
    let acme = plugin("good");
    let relations: Vec<String> = [QUALIFIED, UNDER, AWKWARD]
        .into_iter()
        .chain(MALFORMED)
        .map(str::to_owned)
        .collect();
    let linker = AnsweredLinker::new("acme", acme.links(&relations, configured()).await.unwrap());
    let unconfigured = AnsweredLinker::new(
        "acme",
        acme.links(&relations, BTreeMap::new()).await.unwrap(),
    );
    let report = ods_sdk::conformance::relation_link::run(&Linking {
        linker: Arc::new(linker.clone()),
        unconfigured: Arc::new(unconfigured),
    });
    assert!(report.skipped.is_empty(), "{report:?}");
    // A relation nobody asked about has no link, saying so.
    assert!(matches!(
        linker.link("\"shop\".\"main\".\"never_asked\""),
        Err(NoRelationLink::NotAnswered { .. })
    ));
}

async fn a_link_a_page_cant_safely_show_is_none() {
    let relations = vec![QUALIFIED.to_owned(), UNDER.to_owned()];
    let links = plugin("unsafe-link")
        .links(&relations, configured())
        .await
        .unwrap();
    let why = links[QUALIFIED].clone().unwrap_err().to_string();
    assert!(why.contains("isn't an `https://` URL"), "{why}");
    // A relation it didn't answer about has none either.
    let links = plugin("link-missing")
        .links(&relations, configured())
        .await
        .unwrap();
    assert!(matches!(
        &links[UNDER],
        Err(NoRelationLink::NotAnswered { why, .. }) if why.contains("didn't answer")
    ));
    // One it wasn't asked about fails the call.
    let error = plugin("link-unasked")
        .links(&relations, configured())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("wasn't asked about"), "{error}");
}

async fn errors_are_classified_in_one_call_and_served_from_the_answers() {
    let acme = plugin("good");
    let errors = vec![
        summary("Acme Error: no such table"),
        summary("Acme Error: something else"),
    ];
    let classifications = acme.classify(&errors).await.unwrap();
    let catalogue = AnsweredCatalogue::new(
        "acme",
        CatalogueInfo::new("acme", "1", "Acme"),
        errors.iter().cloned().zip(classifications).collect(),
    );
    assert!(matches!(
        catalogue.classify(&errors[0]),
        Classification::Recognised(m) if m.id == "acme-missing-table"
    ));
    assert!(matches!(
        catalogue.classify(&errors[1]),
        Classification::NotRecognised { .. }
    ));
    // An error it wasn't asked about isn't recognised: dbt's catalogue explains it.
    assert!(matches!(
        catalogue.classify(&summary("Acme Error: no such table here")),
        Classification::NotRecognised { .. }
    ));
    let error = plugin("classify-short")
        .classify(&errors)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("classified 1 errors, asked about 2"),
        "{error}"
    );
}

async fn observed_lineage_is_read_from_the_export() {
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("lineage.txt");
    std::fs::write(&export, "shop.raw.orders.id -> shop.main.orders.order_id\n").unwrap();
    let lineage = plugin("good")
        .observed_lineage(&export.display().to_string(), BTreeMap::new())
        .await
        .unwrap();
    assert_eq!(lineage.targets().len(), 1, "{lineage:?}");
}

/// The pretend warehouse's probe: tables answer, a view is skipped, a relation that
/// isn't the one expected is unknown.
struct Probing(Arc<PluginProcess>);

fn request() -> ProbeRequest {
    ProbeRequest::new(
        ProbeFilter::kinds(["table"]).unwrap(),
        vec![ProbeStatement::new("select count(*) as n from {relation}", ["n"]).unwrap()],
    )
    .unwrap()
}

#[async_trait]
impl ods_sdk::conformance::probe::ProbeHarness for Probing {
    async fn probe(&self) -> Arc<dyn RelationProbe> {
        let described = self.0.describe().await.unwrap();
        Arc::new(self.0.probe(&described, configured()).unwrap())
    }
    fn request(&self) -> ProbeRequest {
        request()
    }
    fn matching(&self) -> Vec<ProbeTarget> {
        vec![
            ProbeTarget::new("model.shop.orders", "orders").expecting("shop.main.orders"),
            ProbeTarget::new("model.shop.items", "items").expecting("shop.main.items"),
        ]
    }
    fn excluded(&self) -> Option<ProbeTarget> {
        Some(ProbeTarget::new("model.shop.orders_view", "orders_view"))
    }
    fn elsewhere(&self) -> Option<ProbeTarget> {
        Some(ProbeTarget::new("model.shop.moved", "moved").expecting("shop.elsewhere.moved"))
    }
}

#[async_trait]
impl ods_sdk::conformance::privileges::PrivilegesHarness for Probing {
    async fn provider(&self) -> Arc<dyn RelationPrivileges> {
        let described = self.0.describe().await.unwrap();
        Arc::new(self.0.probe(&described, configured()).unwrap())
    }
    fn read_only(&self) -> Vec<ProbeTarget> {
        vec![
            ProbeTarget::new("model.shop.orders", "orders"),
            ProbeTarget::new("model.shop.items", "items"),
        ]
    }
    fn elevated(&self) -> Option<ProbeTarget> {
        Some(ProbeTarget::new("model.shop.owned", "owned"))
    }
}

async fn probes_over_its_own_connection_pass_the_sdks_suites() {
    let acme = plugin("good");
    let described = acme.describe().await.unwrap();
    let probe = acme.probe(&described, configured()).unwrap();
    // The login checked is the login probed: one provider does both.
    assert!(probe.reports_privileges());
    let harness = Probing(acme);
    let report = ods_sdk::conformance::probe::run(&harness).await;
    assert!(report.passed.len() >= 3, "{report:?}");
    let report = ods_sdk::conformance::privileges::run(&harness).await;
    assert!(report.skipped.is_empty(), "{report:?}");
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
        the_handshake_checks_what_a_warehouse_needs,
        links_asked_in_one_call_pass_the_sdks_suite,
        a_link_a_page_cant_safely_show_is_none,
        errors_are_classified_in_one_call_and_served_from_the_answers,
        observed_lineage_is_read_from_the_export,
        probes_over_its_own_connection_pass_the_sdks_suites,
    );
}
