//! Conformance suite for [`LockProvider`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ods_core::Capability;

use super::Report;
use crate::contracts::lock::{LockKey, LockProvider};
use crate::error::ProviderError;

/// What the suite needs from a provider under test.
#[async_trait]
pub trait LockHarness: Send + Sync {
    /// A fresh provider with no locks held. Called once per case.
    async fn provider(&self) -> Arc<dyn LockProvider>;

    /// Moves the provider's clock forward by `by`. Returns `false` if the provider's
    /// time cannot be controlled, which skips the expiry cases.
    fn advance_time(&self, by: Duration) -> bool;
}

const TTL: Duration = Duration::from_secs(30);

fn key(name: &str) -> LockKey {
    LockKey::new(name).expect("suite keys are valid")
}

/// Runs every case against providers from `harness`.
///
/// # Panics
/// Panics with the case name when a provider violates the contract.
pub async fn run(harness: &dyn LockHarness) -> Report {
    let mut report = Report::default();
    let capabilities = harness.provider().await.info().capabilities;

    exclusive_acquire(harness).await;
    report.passed.push("exclusive_acquire");
    release_frees_the_key(harness).await;
    report.passed.push("release_frees_the_key");
    release_is_idempotent_and_guarded(harness).await;
    report.passed.push("release_is_idempotent_and_guarded");
    renew_keeps_the_token(harness).await;
    report.passed.push("renew_keeps_the_token");
    keys_are_independent(harness).await;
    report.passed.push("keys_are_independent");

    if capabilities.contains(&Capability::FencingTokens) {
        fencing_tokens_increase(harness).await;
        report.passed.push("fencing_tokens_increase");
    } else {
        report.skipped.push((
            "fencing_tokens_increase",
            "provider lacks fencing_tokens".into(),
        ));
    }

    if !capabilities.contains(&Capability::LeaseExpiry) {
        report.skipped.push((
            "expired_leases_can_be_taken_over",
            "provider lacks lease_expiry".into(),
        ));
    } else if expired_leases_can_be_taken_over(harness).await {
        report.passed.push("expired_leases_can_be_taken_over");
    } else {
        report.skipped.push((
            "expired_leases_can_be_taken_over",
            "harness cannot advance time".into(),
        ));
    }
    report
}

async fn exclusive_acquire(h: &dyn LockHarness) {
    let p = h.provider().await;
    let k = key("exclusive");
    let lease = p
        .acquire(&k, "a", TTL)
        .await
        .unwrap()
        .expect("exclusive_acquire: free key must be granted");
    assert_eq!(lease.key, k, "exclusive_acquire: lease key");
    assert_eq!(lease.owner, "a", "exclusive_acquire: lease owner");
    assert!(
        p.acquire(&k, "b", TTL).await.unwrap().is_none(),
        "exclusive_acquire: held key granted to another owner"
    );
    assert!(
        p.acquire(&k, "a", TTL).await.unwrap().is_none(),
        "exclusive_acquire: held key granted again to its owner"
    );
}

async fn release_frees_the_key(h: &dyn LockHarness) {
    let p = h.provider().await;
    let k = key("release");
    let lease = p
        .acquire(&k, "a", TTL)
        .await
        .unwrap()
        .expect("release_frees_the_key: initial grant");
    p.release(&lease)
        .await
        .expect("release_frees_the_key: release");
    let next = p.acquire(&k, "b", TTL).await.unwrap();
    assert!(
        next.is_some(),
        "release_frees_the_key: key not free after release"
    );
}

async fn release_is_idempotent_and_guarded(h: &dyn LockHarness) {
    let p = h.provider().await;
    let k = key("guarded");
    let first = p
        .acquire(&k, "a", TTL)
        .await
        .unwrap()
        .expect("release_is_idempotent_and_guarded: grant");
    p.release(&first)
        .await
        .expect("release_is_idempotent_and_guarded: release");
    p.release(&first)
        .await
        .expect("release_is_idempotent_and_guarded: releasing a free key must succeed");
    let second = p
        .acquire(&k, "b", TTL)
        .await
        .unwrap()
        .expect("release_is_idempotent_and_guarded: regrant");
    assert!(
        matches!(p.release(&first).await, Err(ProviderError::Conflict(_))),
        "release_is_idempotent_and_guarded: a stale lease released another owner's lock"
    );
    assert!(
        matches!(p.renew(&first, TTL).await, Err(ProviderError::Conflict(_))),
        "release_is_idempotent_and_guarded: a stale lease was renewed"
    );
    p.release(&second)
        .await
        .expect("release_is_idempotent_and_guarded: current holder releases");
}

async fn renew_keeps_the_token(h: &dyn LockHarness) {
    let p = h.provider().await;
    let k = key("renew");
    let lease = p
        .acquire(&k, "a", TTL)
        .await
        .unwrap()
        .expect("renew_keeps_the_token: grant");
    let renewed = p
        .renew(&lease, TTL * 2)
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
    if let (Some(before), Some(after)) = (lease.expires_at, renewed.expires_at) {
        assert!(
            after >= before,
            "renew_keeps_the_token: renewal shortened the lease"
        );
    }
}

async fn keys_are_independent(h: &dyn LockHarness) {
    let p = h.provider().await;
    let a = p.acquire(&key("one"), "x", TTL).await.unwrap();
    let b = p.acquire(&key("two"), "y", TTL).await.unwrap();
    assert!(
        a.is_some() && b.is_some(),
        "keys_are_independent: one key blocked another"
    );
}

async fn fencing_tokens_increase(h: &dyn LockHarness) {
    let p = h.provider().await;
    let k = key("fencing");
    let mut last = None;
    for owner in ["a", "b", "c"] {
        let lease = p
            .acquire(&k, owner, TTL)
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

/// Returns `false` if the harness cannot advance time.
async fn expired_leases_can_be_taken_over(h: &dyn LockHarness) -> bool {
    let p = h.provider().await;
    let k = key("expiry");
    let old = p
        .acquire(&k, "a", TTL)
        .await
        .unwrap()
        .expect("expired_leases_can_be_taken_over: grant");
    assert!(
        old.expires_at.is_some(),
        "expired_leases_can_be_taken_over: lease_expiry provider gave no expiry"
    );
    if !h.advance_time(TTL + Duration::from_secs(1)) {
        return false;
    }
    let new = p
        .acquire(&k, "b", TTL)
        .await
        .unwrap()
        .expect("expired_leases_can_be_taken_over: expired key was not granted");
    assert!(
        matches!(p.renew(&old, TTL).await, Err(ProviderError::Conflict(_))),
        "expired_leases_can_be_taken_over: expired lease was renewed after takeover"
    );
    if p.info().capabilities.contains(&Capability::FencingTokens) {
        assert!(
            new.token > old.token,
            "expired_leases_can_be_taken_over: takeover token did not increase"
        );
    }
    true
}
