//! Resolving secret references through the secret providers a host has (#126,
//! ADR-0032), and the built-in `env:` provider.
//!
//! A [`SecretResolver`] maps each scheme to the one [`SecretProvider`] serving it and
//! sends every reference to its scheme's provider. It holds no values: each
//! [`resolve`](SecretResolver::resolve) asks the provider again, so a rotated secret is
//! picked up and nothing resolved outlives its caller.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use ods_config::SecretRef;
use ods_core::CapabilitySet;

use crate::contracts::secrets::{SecretError, SecretProvider, SecretValue};
use crate::provider::{Provider, ProviderInfo};

/// Why a secret provider couldn't be added to a [`SecretResolver`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ResolverError {
    /// Another provider already serves the scheme.
    #[error("a secret provider for `{0}:` references is already registered")]
    DuplicateScheme(String),
    /// The scheme isn't one a reference can name.
    #[error(
        "`{0}` is not a valid secret scheme: use lowercase letters, digits or `-` (e.g. `keychain`)"
    )]
    InvalidScheme(String),
}

/// The secret providers a host has, by scheme.
#[derive(Clone, Default)]
pub struct SecretResolver {
    providers: BTreeMap<String, Arc<dyn SecretProvider>>,
}

impl SecretResolver {
    /// A resolver with no providers: every reference is [`SecretError::UnknownScheme`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `provider` for its scheme.
    ///
    /// # Errors
    /// Returns [`ResolverError`] if its scheme is invalid or already served: a second
    /// provider never silently takes over a scheme.
    pub fn register(&mut self, provider: Arc<dyn SecretProvider>) -> Result<(), ResolverError> {
        let scheme = provider.scheme().to_owned();
        if !SecretRef::is_valid_scheme(&scheme) {
            return Err(ResolverError::InvalidScheme(scheme));
        }
        if self.providers.contains_key(&scheme) {
            return Err(ResolverError::DuplicateScheme(scheme));
        }
        self.providers.insert(scheme, provider);
        Ok(())
    }

    /// The schemes served, sorted.
    pub fn schemes(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    /// The provider serving `scheme`, if any.
    pub fn provider(&self, scheme: &str) -> Option<&Arc<dyn SecretProvider>> {
        self.providers.get(scheme)
    }

    /// The value `reference` names, from its scheme's provider.
    ///
    /// # Errors
    /// Returns [`SecretError::UnknownScheme`] if no provider serves its scheme,
    /// [`SecretError::Empty`] if the provider returned an empty value (whatever the
    /// provider, an empty credential is never handed out), and otherwise what the
    /// provider returns.
    pub async fn resolve(&self, reference: &SecretRef) -> Result<SecretValue, SecretError> {
        let Some(provider) = self.providers.get(reference.scheme()) else {
            return Err(SecretError::UnknownScheme {
                scheme: reference.scheme().to_owned(),
                available: self.schemes(),
            });
        };
        let value = provider.resolve(reference).await?;
        if value.expose().is_empty() {
            return Err(SecretError::empty(reference));
        }
        Ok(value)
    }
}

impl fmt::Debug for SecretResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretResolver")
            .field("schemes", &self.schemes())
            .finish()
    }
}

/// The built-in provider for `env:` references: environment variables, which is also
/// how CI systems inject secrets.
///
/// It reads from the variables it was given, not the process's, so hosts pass their own
/// environment and tests stay hermetic.
pub struct EnvSecrets {
    vars: BTreeMap<String, String>,
}

impl EnvSecrets {
    /// The scheme it serves.
    pub const SCHEME: &'static str = "env";

    /// A provider over `vars`, e.g. the process's environment.
    pub fn from_vars<K, V>(vars: impl IntoIterator<Item = (K, V)>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            vars: vars
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }
}

impl fmt::Debug for EnvSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnvSecrets")
    }
}

impl Provider for EnvSecrets {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            Self::SCHEME,
            Self::SCHEME,
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}

#[async_trait]
impl SecretProvider for EnvSecrets {
    fn scheme(&self) -> &str {
        Self::SCHEME
    }

    async fn resolve(&self, reference: &SecretRef) -> Result<SecretValue, SecretError> {
        if reference.scheme() != Self::SCHEME {
            return Err(SecretError::UnknownScheme {
                scheme: reference.scheme().to_owned(),
                available: vec![Self::SCHEME.to_owned()],
            });
        }
        match self.vars.get(reference.name()) {
            None => Err(SecretError::not_found(reference)),
            Some(value) if value.is_empty() => Err(SecretError::empty(reference)),
            Some(value) => Ok(SecretValue::new(value.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(text: &str) -> SecretRef {
        SecretRef::parse(text).expect("valid reference")
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        // The env provider never awaits anything, so one poll finishes it.
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(out) => out,
            std::task::Poll::Pending => panic!("the env provider never waits"),
        }
    }

    fn env() -> Arc<EnvSecrets> {
        Arc::new(EnvSecrets::from_vars([("TOKEN", "s3cr3t"), ("BLANK", "")]))
    }

    #[test]
    fn env_resolves_set_variables_and_refuses_missing_or_empty_ones() {
        let env = env();
        let value = block_on(env.resolve(&reference("env:TOKEN"))).expect("set");
        assert_eq!(value.expose(), "s3cr3t");
        assert_eq!(
            block_on(env.resolve(&reference("env:MISSING"))).map(|_| ()),
            Err(SecretError::not_found(&reference("env:MISSING")))
        );
        assert_eq!(
            block_on(env.resolve(&reference("env:BLANK"))).map(|_| ()),
            Err(SecretError::empty(&reference("env:BLANK")))
        );
        assert!(matches!(
            block_on(env.resolve(&reference("vault:TOKEN"))),
            Err(SecretError::UnknownScheme { .. })
        ));
        assert_eq!(format!("{env:?}"), "EnvSecrets");
    }

    #[test]
    fn the_resolver_dispatches_by_scheme_and_lists_what_it_serves() {
        let mut resolver = SecretResolver::new();
        resolver.register(env()).expect("first env");
        assert_eq!(
            resolver.register(env()),
            Err(ResolverError::DuplicateScheme("env".to_owned()))
        );
        assert_eq!(resolver.schemes(), ["env"]);
        let value = block_on(resolver.resolve(&reference("env:TOKEN"))).expect("set");
        assert_eq!(value.expose(), "s3cr3t");
        let unknown = block_on(resolver.resolve(&reference("keychain:db")))
            .map(|_| ())
            .expect_err("no keychain provider");
        assert_eq!(
            unknown.to_string(),
            "no secret provider serves `keychain:` references (served: env)"
        );
        assert_eq!(
            format!("{resolver:?}"),
            r#"SecretResolver { schemes: ["env"] }"#
        );
    }

    #[test]
    fn an_empty_value_is_never_handed_out_whatever_the_provider() {
        struct Careless;
        impl Provider for Careless {
            fn info(&self) -> ProviderInfo {
                ProviderInfo::new("x", "x", "0", CapabilitySet::new())
            }
        }
        #[async_trait]
        impl SecretProvider for Careless {
            fn scheme(&self) -> &'static str {
                "careless"
            }
            async fn resolve(&self, _: &SecretRef) -> Result<SecretValue, SecretError> {
                Ok(SecretValue::new(String::new()))
            }
        }
        let mut resolver = SecretResolver::new();
        resolver.register(Arc::new(Careless)).expect("valid scheme");
        assert_eq!(
            block_on(resolver.resolve(&reference("careless:x"))).map(|_| ()),
            Err(SecretError::empty(&reference("careless:x")))
        );
    }

    #[test]
    fn invalid_schemes_are_refused() {
        struct Upper;
        impl Provider for Upper {
            fn info(&self) -> ProviderInfo {
                ProviderInfo::new("x", "x", "0", CapabilitySet::new())
            }
        }
        #[async_trait]
        impl SecretProvider for Upper {
            fn scheme(&self) -> &'static str {
                "Vault"
            }
            async fn resolve(&self, r: &SecretRef) -> Result<SecretValue, SecretError> {
                Err(SecretError::not_found(r))
            }
        }
        assert_eq!(
            SecretResolver::new().register(Arc::new(Upper)),
            Err(ResolverError::InvalidScheme("Vault".to_owned()))
        );
    }
}
