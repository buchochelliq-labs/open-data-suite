//! Signing in to a Databricks workspace, as [`Credentials`] (#297, ADR-0021).
//!
//! Two methods so far, each used only when named with `auth`:
//! - `m2m`: a service principal's OAuth secret (`client_id`, `client_secret`),
//!   exchanged at `<host>/oidc/v1/token` for a token with the narrowest scope granted;
//! - `pat`: a static token (`token`), the last resort.
//!
//! Secrets are references, resolved only when a token is needed (ADR-0032). Tokens are
//! kept in memory, reused until a minute before they expire, and never leave this
//! crate: [`Credentials::status`] reports how it signs in, never the token.

use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use ods_core::CapabilitySet;
use ods_core::state::Timestamp;
use ods_sdk::contracts::credentials::{AuthMethod, AuthStatus, CredentialError, Credentials};
use ods_sdk::secrets::SecretResolver;
use ods_sdk::{Provider, ProviderInfo};

use crate::KIND;
use crate::auth::http::{Https, Transport};
use crate::auth::{AccessToken, Method, Settings, TokenCache, m2m};

/// Signs in to one workspace, as one `[providers.<name>]` configures it.
pub struct DatabricksCredentials {
    instance: String,
    settings: Settings,
    secrets: SecretResolver,
    transport: Arc<dyn Transport>,
    cache: TokenCache,
    clock: fn() -> SystemTime,
}

impl DatabricksCredentials {
    /// Reads `settings` (a `[providers.<name>]` table's `settings`) for instance
    /// `instance`. `env` is the environment `ods` runs in (`DATABRICKS_HOST`), and
    /// `secrets` resolves `client_secret` and `token` when a token is needed.
    ///
    /// # Errors
    /// A setting is missing or can't be used, or the HTTPS client can't be set up.
    pub fn from_settings(
        instance: impl Into<String>,
        settings: &toml::Table,
        env: &[(String, String)],
        secrets: SecretResolver,
    ) -> Result<Self, CredentialError> {
        let transport = Https::new().map_err(|reason| CredentialError::Unavailable { reason })?;
        Ok(Self::with_transport(
            instance,
            Settings::parse(settings, env)?,
            secrets,
            Arc::new(transport),
        ))
    }

    pub(crate) fn with_transport(
        instance: impl Into<String>,
        settings: Settings,
        secrets: SecretResolver,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self {
            instance: instance.into(),
            settings,
            secrets,
            transport,
            cache: TokenCache::default(),
            clock: SystemTime::now,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_clock(mut self, clock: fn() -> SystemTime) -> Self {
        self.clock = clock;
        self
    }

    /// A token for the workspace: the cached one while it is fresh, else a new one.
    pub(crate) async fn token(&self) -> Result<Arc<AccessToken>, CredentialError> {
        let now = (self.clock)();
        if let Some(token) = self.cache.fresh(now) {
            return Ok(token);
        }
        let token = match &self.settings.method {
            Method::M2m {
                client_id,
                client_secret,
                scopes,
            } => {
                let secret = self.secrets.resolve(client_secret).await?;
                m2m::grant(
                    self.transport.as_ref(),
                    &self.settings.host,
                    client_id,
                    &secret,
                    scopes,
                    now,
                )
                .await?
            }
            Method::Pat { token } => AccessToken {
                value: self.secrets.resolve(token).await?,
                expires_at: None,
                scope: String::new(),
            },
        };
        let token = Arc::new(token);
        // Only a token whose expiry is known is reused: one without could be used
        // after it lapsed (a static token is resolved again each time anyway).
        if token.expires_at.is_some() {
            self.cache.store(Arc::clone(&token));
        }
        Ok(token)
    }
}

impl fmt::Debug for DatabricksCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // References and names only; no token is ever formatted.
        f.debug_struct("DatabricksCredentials")
            .field("instance", &self.instance)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl Provider for DatabricksCredentials {
    fn info(&self) -> ProviderInfo {
        ProviderInfo::new(
            KIND,
            &self.instance,
            env!("CARGO_PKG_VERSION"),
            CapabilitySet::new(),
        )
    }
}

#[async_trait]
impl Credentials for DatabricksCredentials {
    async fn status(&self) -> Result<AuthStatus, CredentialError> {
        let token = self.token().await?;
        Ok(match &self.settings.method {
            Method::M2m { client_id, .. } => AuthStatus::new(
                AuthMethod::ServiceSecret,
                Some(client_id.clone()),
                token.scope.split_whitespace().map(str::to_owned).collect(),
                token.expires_at.map(timestamp),
                true,
            ),
            // Nothing checks a static token without calling an API with it.
            Method::Pat { .. } => {
                AuthStatus::new(AuthMethod::StaticToken, None, Vec::new(), None, false)
            }
        })
    }
}

fn timestamp(at: SystemTime) -> Timestamp {
    let seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    Timestamp::from_unix(seconds)
}
