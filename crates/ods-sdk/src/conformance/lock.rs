//! Conformance suite for [`LockProvider`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ods_core::{Capability, CapabilitySet};

use super::Report;
use crate::contracts::lock::{LeaseTtl, LockKey, LockProvider};
use crate::error::ProviderError;

/// What the suite needs from a provider under test.
#[async_trait]
pub trait LockHarness: Send + Sync {
    /// A fresh provider with no locks held. Called once per case. Every provider must
    /// advertise the same capabilities; the suite checks this.
    async fn provider(&self) -> Arc<dyn LockProvider>;

    /// Moves time forward by `by` for every provider this harness has created or will
    /// create. Returns `false` if time cannot be controlled, which skips the cases that
    /// need it. The suite first calls it with [`Duration::ZERO`] to ask.
    fn advance_time(&self, by: Duration) -> bool;
}

const SECOND: Duration = Duration::from_secs(1);
const TTL_SECS: u64 = 30;
const TTL_DURATION: Duration = Duration::from_secs(TTL_SECS);
/// Just inside the ttl.
const ALMOST_TTL: Duration = Duration::from_secs(TTL_SECS - 1);

fn ttl() -> LeaseTtl {
    LeaseTtl::new(TTL_DURATION).expect("the suite's ttl is in range")
}

fn key(name: &str) -> LockKey {
    LockKey::new(name).expect("suite keys are valid")
}

fn is_conflict<T>(result: &Result<T, ProviderError>) -> bool {
    matches!(result, Err(ProviderError::Conflict(_)))
}

/// Hands out providers and checks each advertises what the first one did.
struct Suite<'h> {
    harness: &'h dyn LockHarness,
    capabilities: CapabilitySet,
}

impl Suite<'_> {
    async fn provider(&self, case: &str) -> Arc<dyn LockProvider> {
        let provider = self.harness.provider().await;
        assert_eq!(
            provider.info().capabilities,
            self.capabilities,
            "{case}: the harness returned providers with different capabilities"
        );
        provider
    }

    fn has(&self, capability: &Capability) -> bool {
        self.capabilities.contains(capability)
    }

    fn advance(&self, by: Duration) {
        assert!(
            self.harness.advance_time(by),
            "the harness stopped being able to advance time"
        );
    }
}

/// When a case can run.
enum Needs {
    Nothing,
    Capability(Capability),
    /// Controllable time and this capability.
    TimeWith(Capability),
    /// Controllable time and not this capability.
    TimeWithout(Capability),
}

impl Report {
    /// Records `case` as skipped if its needs aren't met, and returns whether to run it.
    fn gate(&mut self, case: &'static str, needs: &Needs, suite: &Suite<'_>, time: bool) -> bool {
        let reason = match needs {
            Needs::Capability(c) | Needs::TimeWith(c) if !suite.has(c) => {
                Some(format!("provider lacks {c}"))
            }
            Needs::TimeWithout(c) if suite.has(c) => Some(format!("provider has {c}")),
            Needs::TimeWith(_) | Needs::TimeWithout(_) if !time => {
                Some("harness cannot advance time".to_owned())
            }
            _ => None,
        };
        match reason {
            Some(reason) => {
                self.skipped.push((case, reason));
                false
            }
            None => true,
        }
    }
}

/// Runs a case if its needs are met, recording the outcome.
macro_rules! case {
    ($report:ident, $suite:ident, $time:ident, $needs:expr, $case:ident) => {
        if $report.gate(stringify!($case), &$needs, &$suite, $time) {
            $case(&$suite).await;
            $report.passed.push(stringify!($case));
        }
    };
}

/// Runs every case against providers from `harness`.
///
/// # Panics
/// Panics with the case name when a provider violates the contract.
pub async fn run(harness: &dyn LockHarness) -> Report {
    let suite = Suite {
        harness,
        capabilities: harness.provider().await.info().capabilities,
    };
    let time = harness.advance_time(Duration::ZERO);
    let mut report = Report::default();

    case!(report, suite, time, Needs::Nothing, exclusive_acquire);
    case!(report, suite, time, Needs::Nothing, release_frees_the_key);
    case!(
        report,
        suite,
        time,
        Needs::Nothing,
        stale_leases_of_other_owners_are_rejected
    );
    case!(
        report,
        suite,
        time,
        Needs::Nothing,
        stale_leases_of_the_same_owner_are_rejected
    );
    case!(report, suite, time, Needs::Nothing, renew_keeps_the_token);
    case!(report, suite, time, Needs::Nothing, keys_are_independent);
    case!(
        report,
        suite,
        time,
        Needs::Capability(Capability::FencingTokens),
        fencing_tokens_increase
    );
    case!(
        report,
        suite,
        time,
        Needs::TimeWith(Capability::LeaseExpiry),
        leases_hold_until_ttl_and_renewal_extends
    );
    case!(
        report,
        suite,
        time,
        Needs::TimeWith(Capability::LeaseExpiry),
        expired_leases_cannot_be_renewed
    );
    case!(
        report,
        suite,
        time,
        Needs::TimeWith(Capability::LeaseExpiry),
        expired_leases_can_be_taken_over
    );
    case!(
        report,
        suite,
        time,
        Needs::TimeWithout(Capability::LeaseExpiry),
        leases_without_expiry_never_expire
    );
    report
}

async fn exclusive_acquire(s: &Suite<'_>) {
    let p = s.provider("exclusive_acquire").await;
    let k = key("exclusive");
    let lease = p
        .acquire(&k, "a", ttl())
        .await
        .unwrap()
        .expect("exclusive_acquire: free key must be granted");
    assert_eq!(lease.key, k, "exclusive_acquire: lease key");
    assert_eq!(lease.owner, "a", "exclusive_acquire: lease owner");
    assert_eq!(
        lease.expires_at.is_some(),
        s.has(&Capability::LeaseExpiry),
        "exclusive_acquire: expires_at must be set exactly when lease_expiry is advertised"
    );
    assert!(
        p.acquire(&k, "b", ttl()).await.unwrap().is_none(),
        "exclusive_acquire: held key granted to another owner"
    );
    assert!(
        p.acquire(&k, "a", ttl()).await.unwrap().is_none(),
        "exclusive_acquire: held key granted again to its owner"
    );
}

async fn release_frees_the_key(s: &Suite<'_>) {
    let p = s.provider("release_frees_the_key").await;
    let k = key("release");
    let lease = p
        .acquire(&k, "a", ttl())
        .await
        .unwrap()
        .expect("release_frees_the_key: initial grant");
    p.release(&lease)
        .await
        .expect("release_frees_the_key: release");
    p.release(&lease)
        .await
        .expect("release_frees_the_key: releasing a free key must succeed");
    let next = p.acquire(&k, "b", ttl()).await.unwrap();
    assert!(
        next.is_some(),
        "release_frees_the_key: key not free after release"
    );
}

async fn stale_leases_of_other_owners_are_rejected(s: &Suite<'_>) {
    const CASE: &str = "stale_leases_of_other_owners_are_rejected";
    let p = s.provider(CASE).await;
    let k = key("guarded");
    let first = p.acquire(&k, "a", ttl()).await.unwrap().expect(CASE);
    p.release(&first).await.expect(CASE);
    let second = p.acquire(&k, "b", ttl()).await.unwrap().expect(CASE);
    assert!(
        is_conflict(&p.release(&first).await),
        "{CASE}: a stale lease released another owner's lock"
    );
    assert!(
        is_conflict(&p.renew(&first, ttl()).await),
        "{CASE}: a stale lease was renewed"
    );
    p.release(&second)
        .await
        .unwrap_or_else(|e| panic!("{CASE}: current holder must release: {e}"));
}

/// A retried run often reuses its ID; its old lease must not control the new grant.
async fn stale_leases_of_the_same_owner_are_rejected(s: &Suite<'_>) {
    const CASE: &str = "stale_leases_of_the_same_owner_are_rejected";
    let p = s.provider(CASE).await;
    let k = key("regrant");
    let first = p.acquire(&k, "run", ttl()).await.unwrap().expect(CASE);
    p.release(&first).await.expect(CASE);
    let second = p.acquire(&k, "run", ttl()).await.unwrap().expect(CASE);
    assert_ne!(
        first.token, second.token,
        "{CASE}: two grants on a key had the same token"
    );
    assert!(
        is_conflict(&p.renew(&first, ttl()).await),
        "{CASE}: an earlier grant renewed the owner's new lease"
    );
    assert!(
        is_conflict(&p.release(&first).await),
        "{CASE}: an earlier grant released the owner's new lease"
    );
    assert!(
        p.acquire(&k, "other", ttl()).await.unwrap().is_none(),
        "{CASE}: the new lease was lost"
    );
    p.renew(&second, ttl())
        .await
        .unwrap_or_else(|e| panic!("{CASE}: the current grant must renew: {e}"));
}

async fn renew_keeps_the_token(s: &Suite<'_>) {
    let p = s.provider("renew_keeps_the_token").await;
    let k = key("renew");
    let lease = p
        .acquire(&k, "a", ttl())
        .await
        .unwrap()
        .expect("renew_keeps_the_token: grant");
    let renewed = p
        .renew(&lease, ttl())
        .await
        .expect("renew_keeps_the_token: renew");
    assert_eq!(
        renewed.token, lease.token,
        "renew_keeps_the_token: token changed"
    );
    assert_eq!(
        renewed.owner, lease.owner,
        "renew_keeps_the_token: owner changed"
    );
    assert_eq!(
        renewed.expires_at.is_some(),
        s.has(&Capability::LeaseExpiry),
        "renew_keeps_the_token: expires_at must be set exactly when lease_expiry is advertised"
    );
}

async fn keys_are_independent(s: &Suite<'_>) {
    let p = s.provider("keys_are_independent").await;
    let a = p.acquire(&key("one"), "x", ttl()).await.unwrap();
    let b = p.acquire(&key("two"), "y", ttl()).await.unwrap();
    assert!(
        a.is_some() && b.is_some(),
        "keys_are_independent: one key blocked another"
    );
}

async fn fencing_tokens_increase(s: &Suite<'_>) {
    let p = s.provider("fencing_tokens_increase").await;
    let k = key("fencing");
    let mut last = None;
    for owner in ["a", "b", "a"] {
        let lease = p
            .acquire(&k, owner, ttl())
            .await
            .unwrap()
            .expect("fencing_tokens_increase: grant");
        if let Some(previous) = last {
            assert!(
                lease.token > previous,
                "fencing_tokens_increase: token did not increase"
            );
        }
        last = Some(lease.token);
        p.release(&lease)
            .await
            .expect("fencing_tokens_increase: release");
    }
}

/// A lease must last (almost) its whole ttl, and renewal counts from the renewal.
async fn leases_hold_until_ttl_and_renewal_extends(s: &Suite<'_>) {
    const CASE: &str = "leases_hold_until_ttl_and_renewal_extends";
    let p = s.provider(CASE).await;
    let k = key("ttl");
    let lease = p.acquire(&k, "a", ttl()).await.unwrap().expect(CASE);
    s.advance(ALMOST_TTL);
    assert!(
        p.acquire(&k, "b", ttl()).await.unwrap().is_none(),
        "{CASE}: lease expired before its ttl"
    );
    let renewed = p
        .renew(&lease, ttl())
        .await
        .unwrap_or_else(|e| panic!("{CASE}: renew within ttl: {e}"));
    assert!(
        renewed.expires_at > lease.expires_at,
        "{CASE}: renewal did not extend the lease"
    );
    s.advance(ALMOST_TTL);
    assert!(
        p.acquire(&k, "b", ttl()).await.unwrap().is_none(),
        "{CASE}: renewed lease expired before its new ttl"
    );
    s.advance(2 * SECOND);
    assert!(
        p.acquire(&k, "b", ttl()).await.unwrap().is_some(),
        "{CASE}: lease outlived its ttl"
    );
}

/// An expired lease is gone even if nobody took the key: it can't be revived, and
/// releasing it is a no-op.
async fn expired_leases_cannot_be_renewed(s: &Suite<'_>) {
    const CASE: &str = "expired_leases_cannot_be_renewed";
    let p = s.provider(CASE).await;
    let k = key("lapsed");
    let lease = p.acquire(&k, "a", ttl()).await.unwrap().expect(CASE);
    s.advance(TTL_DURATION + SECOND);
    assert!(
        is_conflict(&p.renew(&lease, ttl()).await),
        "{CASE}: an expired lease was renewed"
    );
    p.release(&lease)
        .await
        .unwrap_or_else(|e| panic!("{CASE}: releasing an expired lease must succeed: {e}"));
    assert!(
        p.acquire(&k, "b", ttl()).await.unwrap().is_some(),
        "{CASE}: key not free after expiry"
    );
}

async fn expired_leases_can_be_taken_over(s: &Suite<'_>) {
    const CASE: &str = "expired_leases_can_be_taken_over";
    let p = s.provider(CASE).await;
    let k = key("expiry");
    let old = p.acquire(&k, "a", ttl()).await.unwrap().expect(CASE);
    s.advance(TTL_DURATION + SECOND);
    let new = p
        .acquire(&k, "b", ttl())
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("{CASE}: expired key was not granted"));
    assert!(
        is_conflict(&p.renew(&old, ttl()).await),
        "{CASE}: expired lease was renewed after takeover"
    );
    assert!(
        is_conflict(&p.release(&old).await),
        "{CASE}: expired lease released the new holder's lock"
    );
    if s.has(&Capability::FencingTokens) {
        assert!(
            new.token > old.token,
            "{CASE}: takeover token did not increase"
        );
    }
    p.release(&new)
        .await
        .unwrap_or_else(|e| panic!("{CASE}: new holder must release: {e}"));
}

async fn leases_without_expiry_never_expire(s: &Suite<'_>) {
    const CASE: &str = "leases_without_expiry_never_expire";
    let p = s.provider(CASE).await;
    let k = key("forever");
    let lease = p.acquire(&k, "a", ttl()).await.unwrap().expect(CASE);
    s.advance(LeaseTtl::MAX + SECOND);
    assert!(
        p.acquire(&k, "b", ttl()).await.unwrap().is_none(),
        "{CASE}: a lease expired without lease_expiry"
    );
    let renewed = p
        .renew(&lease, ttl())
        .await
        .unwrap_or_else(|e| panic!("{CASE}: renew: {e}"));
    assert_eq!(renewed.expires_at, None, "{CASE}: renewal set an expiry");
}
