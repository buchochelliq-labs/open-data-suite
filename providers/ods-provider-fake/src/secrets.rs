//! In-memory [`SecretProvider`] (#126, ADR-0032).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use ods_config::SecretRef;
use ods_core::CapabilitySet;
use ods_sdk::contracts::secrets::{SecretError, SecretProvider, SecretValue};
use ods_sdk::{Provider, ProviderInfo};

use crate::KIND;

/// How the fake store fails, as set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecretFailure {
    /// Every read is refused, as a store denying this login would.
    Denied,
    /// The store can't be reached, as a locked keychain or a cloud outage would.
    Unavailable,
}

/// Secrets held in memory under one scheme, as set up. It counts the reads it is asked
/// for, so tests can see that a host asks again each time rather than keeping values.
pub struct FakeSecrets {
    scheme: String,
    secrets: BTreeMap<String, String>,
    failure: Option<SecretFailure>,
    reads: AtomicUsize,
}

impl FakeSecrets {
    /// An empty store serving `scheme`.
    pub fn new(scheme: impl Into<String>) -> Self {
        Self {
            scheme: scheme.into(),
            secrets: BTreeMap::new(),
            failure: None,
            reads: AtomicUsize::new(0),
        }
    }

    /// The store holds `value` under `name`.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.secrets.insert(name.into(), value.into());
        self
    }

    /// Every read fails with `failure`.
    #[must_use]
    pub fn failing(mut self, failure: SecretFailure) -> Self {
        self.failure = Some(failure);
        self
    }

    /// How many references it was asked to resolve.
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl fmt::Debug for FakeSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeSecrets")
            .field("scheme", &self.scheme)
            .field("names", &self.secrets.keys().collect::<Vec<_>>())
            .field("failure", &self.failure)
            .finish_non_exhaustive()
    }
}

impl Provider for FakeSecrets {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            &self.scheme,
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}

#[async_trait]
impl SecretProvider for FakeSecrets {
    fn scheme(&self) -> &str {
        &self.scheme
    }

    async fn resolve(&self, reference: &SecretRef) -> Result<SecretValue, SecretError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if reference.scheme() != self.scheme {
            return Err(SecretError::UnknownScheme {
                scheme: reference.scheme().to_owned(),
                available: vec![self.scheme.clone()],
            });
        }
        match self.failure {
            Some(SecretFailure::Denied) => return Err(SecretError::denied(reference)),
            Some(SecretFailure::Unavailable) => {
                return Err(SecretError::unavailable(
                    reference,
                    "the fake store is offline",
                ));
            }
            None => {}
        }
        match self.secrets.get(reference.name()) {
            None => Err(SecretError::not_found(reference)),
            Some(value) if value.is_empty() => Err(SecretError::empty(reference)),
            Some(value) => Ok(SecretValue::new(value.clone())),
        }
    }
}
