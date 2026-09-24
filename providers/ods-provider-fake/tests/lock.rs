//! The fake lock provider against the SDK conformance suite, the registry, and
//! capability-driven strategy choice.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ods_config::ProviderConfig;
use ods_core::{Capability, CapabilitySet, SchemaVersion, Strategy, choose};
use ods_provider_fake::{FakeClock, FakeLockFactory, FakeLockProvider};
use ods_sdk::conformance::lock::{LockHarness, run};
use ods_sdk::contracts::lock::{LOCK_PROVIDER, LockProvider};
use ods_sdk::{Provider, ProviderError, ProviderFactory, Registry, RegistryError};

/// Builds fresh fakes sharing one clock, optionally without some capabilities.
struct Harness {
    clock: FakeClock,
    without: Vec<Capability>,
    controllable_time: bool,
}

impl Harness {
    fn new(without: &[Capability]) -> Self {
        Self {
            clock: FakeClock::new(),
            without: without.to_vec(),
            controllable_time: true,
        }
    }
}

#[async_trait]
impl LockHarness for Harness {
    async fn provider(&self) -> Arc<dyn LockProvider> {
        let provider = self
            .without
            .iter()
            .fold(FakeLockProvider::new(self.clock.clone()), |p, c| {
                p.without(c)
            });
        Arc::new(provider)
    }

    fn advance_time(&self, by: Duration) -> bool {
        if self.controllable_time {
            self.clock.advance(by);
        }
        self.controllable_time
    }
}

fn skipped(report: &ods_sdk::conformance::Report) -> Vec<(&str, &str)> {
    report
        .skipped
        .iter()
        .map(|(case, reason)| (*case, reason.as_str()))
        .collect()
}

#[tokio::test]
async fn full_capabilities_pass_every_case_that_applies() {
    let report = run(&Harness::new(&[])).await;
    assert_eq!(
        skipped(&report),
        [(
            "leases_without_expiry_never_expire",
            "provider has lease_expiry"
        )]
    );
    assert_eq!(report.passed.len(), 10, "{report:?}");
}

#[tokio::test]
async fn cases_for_missing_capabilities_are_skipped_not_failed() {
    let report = run(&Harness::new(&[Capability::FencingTokens])).await;
    assert_eq!(
        skipped(&report),
        [
            ("fencing_tokens_increase", "provider lacks fencing_tokens"),
            (
                "leases_without_expiry_never_expire",
                "provider has lease_expiry"
            ),
        ]
    );

    let report = run(&Harness::new(&[Capability::LeaseExpiry])).await;
    let lacks = "provider lacks lease_expiry";
    assert_eq!(
        skipped(&report),
        [
            ("leases_hold_until_ttl_and_renewal_extends", lacks),
            ("expired_leases_cannot_be_renewed", lacks),
            ("expired_leases_can_be_taken_over", lacks),
        ]
    );
    assert!(
        report
            .passed
            .contains(&"leases_without_expiry_never_expire")
    );

    let mut fixed_clock = Harness::new(&[]);
    fixed_clock.controllable_time = false;
    let report = run(&fixed_clock).await;
    let no_time = "harness cannot advance time";
    assert_eq!(
        skipped(&report),
        [
            ("leases_hold_until_ttl_and_renewal_extends", no_time),
            ("expired_leases_cannot_be_renewed", no_time),
            ("expired_leases_can_be_taken_over", no_time),
            (
                "leases_without_expiry_never_expire",
                "provider has lease_expiry"
            ),
        ]
    );
}

/// A harness whose providers don't all advertise the same capabilities.
struct Inconsistent(std::sync::atomic::AtomicBool);

#[async_trait]
impl LockHarness for Inconsistent {
    async fn provider(&self) -> Arc<dyn LockProvider> {
        let first = self.0.swap(false, std::sync::atomic::Ordering::SeqCst);
        let provider = FakeLockProvider::new(FakeClock::new());
        Arc::new(if first {
            provider
        } else {
            provider.without(&Capability::FencingTokens)
        })
    }

    fn advance_time(&self, _: Duration) -> bool {
        false
    }
}

#[tokio::test]
#[should_panic(expected = "different capabilities")]
async fn the_suite_checks_every_provider_advertises_the_same_capabilities() {
    run(&Inconsistent(true.into())).await;
}

fn provider_config(toml_text: &str) -> ProviderConfig {
    toml::from_str(toml_text).expect("valid provider config")
}

fn registry() -> Registry<dyn LockProvider> {
    let mut registry = Registry::new(LOCK_PROVIDER);
    registry
        .register(Box::new(FakeLockFactory::default()))
        .unwrap();
    registry
}

#[test]
fn providers_are_created_from_configuration_by_kind() {
    let provider = registry()
        .create(
            "locks",
            &provider_config("kind = \"fake\"\nsettings = { fencing_tokens = false }"),
        )
        .unwrap();
    let info = provider.info();
    assert_eq!(
        (info.kind.as_str(), info.instance.as_str()),
        ("fake", "locks")
    );
    assert_eq!(
        info.capabilities,
        CapabilitySet::from([Capability::LeaseExpiry])
    );
}

#[test]
fn unknown_kinds_and_bad_settings_are_reported() {
    let err = registry()
        .create("x", &provider_config("kind = \"postgres\""))
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "no `lock_provider` provider of kind `postgres` is available; available kinds: fake"
    );

    let err = registry()
        .create(
            "x",
            &provider_config("kind = \"fake\"\nsettings = { colour = true }"),
        )
        .err()
        .unwrap();
    assert!(
        matches!(err, ProviderError::InvalidSettings { ref key, .. } if key == "colour"),
        "{err}"
    );

    let err = registry()
        .create(
            "x",
            &provider_config("kind = \"fake\"\nsettings = { lease_expiry = \"yes\" }"),
        )
        .err()
        .unwrap();
    assert!(err.to_string().contains("must be a boolean"), "{err}");
}

struct FromTheFuture;

impl ProviderFactory<dyn LockProvider> for FromTheFuture {
    fn kind(&self) -> &'static str {
        "future"
    }

    fn contract_version(&self) -> SchemaVersion {
        SchemaVersion::new(LOCK_PROVIDER.version.major, LOCK_PROVIDER.version.minor + 1)
    }

    fn create(&self, _: &str, _: &toml::Table) -> Result<Box<dyn LockProvider>, ProviderError> {
        unreachable!("never registered")
    }
}

/// Built against an older minor, which a pre-1.0 host must reject.
struct FromThePast;

impl ProviderFactory<dyn LockProvider> for FromThePast {
    fn kind(&self) -> &'static str {
        "past"
    }

    fn contract_version(&self) -> SchemaVersion {
        SchemaVersion::new(0, LOCK_PROVIDER.version.minor - 1)
    }

    fn create(&self, _: &str, _: &toml::Table) -> Result<Box<dyn LockProvider>, ProviderError> {
        unreachable!("never registered")
    }
}

/// Creates providers that misreport their instance name.
struct Misnamed;

impl ProviderFactory<dyn LockProvider> for Misnamed {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn contract_version(&self) -> SchemaVersion {
        LOCK_PROVIDER.version
    }

    fn create(&self, _: &str, _: &toml::Table) -> Result<Box<dyn LockProvider>, ProviderError> {
        Ok(Box::new(FakeLockProvider::new(FakeClock::new())))
    }
}

#[test]
fn created_providers_must_report_the_configured_kind_and_instance() {
    let mut registry = Registry::new(LOCK_PROVIDER);
    registry.register(Box::new(Misnamed)).unwrap();
    let err = registry
        .create("locks", &provider_config("kind = \"fake\""))
        .err()
        .unwrap();
    assert!(err.to_string().contains("instance `fake`"), "{err}");
    assert_eq!(
        format!("{:?}", self::registry()),
        r#"Registry { contract: Contract { name: "lock_provider", version: SchemaVersion { major: 0, minor: 1 } }, kinds: ["fake"] }"#
    );
}

#[test]
fn registration_checks_kinds_and_contract_versions() {
    let mut registry = registry();
    assert!(matches!(
        registry.register(Box::new(FakeLockFactory::default())),
        Err(RegistryError::Duplicate { kind: "fake", .. })
    ));
    assert!(matches!(
        registry.register(Box::new(FromTheFuture)),
        Err(RegistryError::Incompatible { kind: "future", .. })
    ));
    assert!(matches!(
        registry.register(Box::new(FromThePast)),
        Err(RegistryError::Incompatible { kind: "past", .. })
    ));
}

/// How a run could coordinate writers, most preferred first.
#[derive(Debug, PartialEq)]
enum Coordination {
    FencedLease,
    ExpiringLease,
    SingleWriter,
}

fn coordination_strategies() -> Vec<Strategy<Coordination>> {
    vec![
        Strategy::new(
            "fenced_lease",
            [Capability::LeaseExpiry, Capability::FencingTokens],
            Coordination::FencedLease,
        ),
        Strategy::new(
            "expiring_lease",
            [Capability::LeaseExpiry],
            Coordination::ExpiringLease,
        ),
        Strategy::fallback("single_writer", Coordination::SingleWriter),
    ]
}

#[test]
fn planners_choose_strategies_from_advertised_capabilities_only() {
    let strategies = coordination_strategies();
    let cases = [
        (vec![], Coordination::FencedLease),
        (vec![Capability::FencingTokens], Coordination::ExpiringLease),
        (
            vec![Capability::FencingTokens, Capability::LeaseExpiry],
            Coordination::SingleWriter,
        ),
    ];
    for (without, expected) in cases {
        let provider = without.iter().fold(
            FakeLockProvider::new(FakeClock::new()),
            FakeLockProvider::without,
        );
        let choice = choose(&provider.info().capabilities, &strategies).unwrap();
        assert_eq!(choice.chosen.value, expected, "without {without:?}");
    }
}
