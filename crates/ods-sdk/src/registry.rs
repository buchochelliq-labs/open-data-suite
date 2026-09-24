//! Creating providers from configuration (ADR-0006 §2).
//!
//! The CLI (the composition root) registers factories for each contract and creates
//! providers from `[providers.<name>]` configuration by `kind`. Core never looks at
//! `kind`; it only sees the resulting contract object and its capabilities.

use std::collections::BTreeMap;

use ods_config::ProviderConfig;
use ods_core::SchemaVersion;

use crate::error::ProviderError;
use crate::provider::Contract;

/// Creates providers of one kind for one contract (`P` is the contract trait object,
/// e.g. `dyn LockProvider`).
pub trait ProviderFactory<P: ?Sized>: Send + Sync {
    /// The kind this factory creates, as written in configuration.
    fn kind(&self) -> &'static str;

    /// The contract version the provider was built against.
    fn contract_version(&self) -> SchemaVersion;

    /// Validates `settings` and creates a provider named `instance`.
    ///
    /// # Errors
    /// Returns [`ProviderError::InvalidSettings`] for unknown or invalid settings (never
    /// echoing values), or another error if the provider cannot be created.
    fn create(&self, instance: &str, settings: &toml::Table) -> Result<Box<P>, ProviderError>;
}

/// Why a factory could not be registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// Another factory already provides this kind.
    #[error("a `{contract}` provider of kind `{kind}` is already registered")]
    Duplicate {
        /// Contract name.
        contract: &'static str,
        /// Kind.
        kind: &'static str,
    },
    /// The provider was built against an incompatible contract version.
    #[error(
        "provider kind `{kind}` was built against `{contract}` {built_major}.{built_minor}, \
         which this SDK ({host_major}.{host_minor}) cannot host"
    )]
    Incompatible {
        /// Contract name.
        contract: &'static str,
        /// Kind.
        kind: &'static str,
        /// Version the provider was built against.
        built_major: u32,
        /// Version the provider was built against.
        built_minor: u32,
        /// Version this SDK defines.
        host_major: u32,
        /// Version this SDK defines.
        host_minor: u32,
    },
}

/// Factories for one contract, keyed by kind.
pub struct Registry<P: ?Sized> {
    contract: Contract,
    factories: BTreeMap<&'static str, Box<dyn ProviderFactory<P>>>,
}

impl<P: ?Sized> Registry<P> {
    /// An empty registry for `contract`.
    pub fn new(contract: Contract) -> Self {
        Self {
            contract,
            factories: BTreeMap::new(),
        }
    }

    /// Adds a factory.
    ///
    /// # Errors
    /// Returns [`RegistryError`] if the kind is taken or the contract version is
    /// incompatible.
    pub fn register(&mut self, factory: Box<dyn ProviderFactory<P>>) -> Result<(), RegistryError> {
        let kind = factory.kind();
        let built = factory.contract_version();
        if !self.contract.accepts(built) {
            return Err(RegistryError::Incompatible {
                contract: self.contract.name,
                kind,
                built_major: built.major,
                built_minor: built.minor,
                host_major: self.contract.version.major,
                host_minor: self.contract.version.minor,
            });
        }
        if self.factories.contains_key(kind) {
            return Err(RegistryError::Duplicate {
                contract: self.contract.name,
                kind,
            });
        }
        self.factories.insert(kind, factory);
        Ok(())
    }

    /// Registered kinds, sorted.
    pub fn kinds(&self) -> Vec<&'static str> {
        self.factories.keys().copied().collect()
    }

    /// Creates the provider configured as `[providers.<instance>]`.
    ///
    /// # Errors
    /// Returns [`ProviderError::UnknownKind`] if no factory serves `config.kind`, or the
    /// factory's error.
    pub fn create(&self, instance: &str, config: &ProviderConfig) -> Result<Box<P>, ProviderError> {
        let factory =
            self.factories
                .get(config.kind.as_str())
                .ok_or_else(|| ProviderError::UnknownKind {
                    contract: self.contract.name,
                    kind: config.kind.clone(),
                    available: self.kinds().into_iter().map(str::to_owned).collect(),
                })?;
        factory.create(instance, &config.settings)
    }
}
