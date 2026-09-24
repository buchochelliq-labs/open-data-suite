//! In-memory [`LockProvider`].

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use ods_core::{Capability, CapabilitySet, SchemaVersion};
use ods_sdk::contracts::lock::{
    LOCK_CAPABILITIES, LOCK_PROVIDER, Lease, LeaseTtl, LockKey, LockProvider,
};
use ods_sdk::{Provider, ProviderError, ProviderFactory, ProviderInfo};

use crate::KIND;
use crate::clock::FakeClock;

#[derive(Debug, Default)]
struct State {
    held: BTreeMap<LockKey, Lease>,
    /// Last token granted per key; never reset, so every grant gets a new, larger token
    /// (unique per grant as the contract requires, increasing with fencing tokens).
    last_token: BTreeMap<LockKey, u64>,
}

/// An in-memory lock provider with a controllable clock and switchable capabilities.
#[derive(Debug)]
pub struct FakeLockProvider {
    instance: String,
    clock: FakeClock,
    capabilities: CapabilitySet,
    state: Mutex<State>,
}

impl FakeLockProvider {
    /// A provider with every lock capability.
    pub fn new(clock: FakeClock) -> Self {
        Self {
            instance: "fake".to_owned(),
            clock,
            capabilities: CapabilitySet::from(LOCK_CAPABILITIES),
            state: Mutex::new(State::default()),
        }
    }

    /// Removes a capability, to exercise fallbacks and capability-gated behaviour.
    #[must_use]
    pub fn without(mut self, capability: &Capability) -> Self {
        self.capabilities.remove(capability);
        self
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn expires_at(&self, ttl: LeaseTtl) -> Option<std::time::SystemTime> {
        self.capabilities
            .contains(&Capability::LeaseExpiry)
            .then(|| self.clock.now() + ttl.get())
    }

    /// The lease currently holding `key`, dropping it if it has expired (expiry is
    /// inclusive: at `expires_at` the key is free).
    fn current(&self, state: &mut State, key: &LockKey) -> Option<Lease> {
        let now = self.clock.now();
        let expired = state
            .held
            .get(key)
            .and_then(|lease| lease.expires_at)
            .is_some_and(|expires| expires <= now);
        if expired {
            state.held.remove(key);
        }
        state.held.get(key).cloned()
    }
}

impl Provider for FakeLockProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            self.instance.clone(),
            env!("CARGO_PKG_VERSION"),
            self.capabilities.clone(),
        )
    }
}

#[async_trait]
impl LockProvider for FakeLockProvider {
    async fn acquire(
        &self,
        key: &LockKey,
        owner: &str,
        ttl: LeaseTtl,
    ) -> Result<Option<Lease>, ProviderError> {
        let mut state = self.state();
        if self.current(&mut state, key).is_some() {
            return Ok(None);
        }
        let token = state.last_token.get(key).copied().unwrap_or(0) + 1;
        state.last_token.insert(key.clone(), token);
        let lease = Lease::new(key.clone(), owner, token, self.expires_at(ttl));
        state.held.insert(key.clone(), lease.clone());
        Ok(Some(lease))
    }

    async fn renew(&self, lease: &Lease, ttl: LeaseTtl) -> Result<Lease, ProviderError> {
        let mut state = self.state();
        match self.current(&mut state, &lease.key) {
            Some(mut renewed) if renewed.same_grant(lease) => {
                renewed.expires_at = self.expires_at(ttl);
                state.held.insert(lease.key.clone(), renewed.clone());
                Ok(renewed)
            }
            _ => Err(ProviderError::Conflict(format!(
                "lease on `{}` is no longer held",
                lease.key
            ))),
        }
    }

    async fn release(&self, lease: &Lease) -> Result<(), ProviderError> {
        let mut state = self.state();
        match self.current(&mut state, &lease.key) {
            None => Ok(()),
            Some(current) if current.same_grant(lease) => {
                state.held.remove(&lease.key);
                Ok(())
            }
            Some(_) => Err(ProviderError::Conflict(format!(
                "lock `{}` is held by a different lease",
                lease.key
            ))),
        }
    }
}

/// Creates [`FakeLockProvider`]s from `[providers.<name>] kind = "fake"`.
///
/// Settings: `lease_expiry` and `fencing_tokens` (booleans, default `true`) switch the
/// corresponding capability off when `false`. Any other key is an error.
#[derive(Debug, Clone, Default)]
pub struct FakeLockFactory {
    clock: FakeClock,
}

impl FakeLockFactory {
    /// A factory whose providers share `clock`.
    pub fn new(clock: FakeClock) -> Self {
        Self { clock }
    }
}

impl ProviderFactory<dyn LockProvider> for FakeLockFactory {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn contract_version(&self) -> SchemaVersion {
        LOCK_PROVIDER.version
    }

    fn create(
        &self,
        instance: &str,
        settings: &toml::Table,
    ) -> Result<Box<dyn LockProvider>, ProviderError> {
        let invalid = |key: &str, message: &str| ProviderError::InvalidSettings {
            instance: instance.to_owned(),
            kind: KIND.to_owned(),
            key: key.to_owned(),
            message: message.to_owned(),
        };
        let mut provider = FakeLockProvider::new(self.clock.clone());
        instance.clone_into(&mut provider.instance);
        for (key, value) in settings {
            let capability = match key.as_str() {
                "lease_expiry" => Capability::LeaseExpiry,
                "fencing_tokens" => Capability::FencingTokens,
                _ => {
                    return Err(invalid(
                        key,
                        "unknown setting; expected lease_expiry or fencing_tokens",
                    ));
                }
            };
            match value.as_bool() {
                Some(true) => {}
                Some(false) => provider = provider.without(&capability),
                None => return Err(invalid(key, "must be a boolean")),
            }
        }
        Ok(Box::new(provider))
    }
}
