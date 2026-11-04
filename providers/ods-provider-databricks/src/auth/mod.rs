//! Signing in to a Databricks workspace (#297, ADR-0021). Private: tokens never leave
//! this crate; [`crate::DatabricksCredentials`] is the public side.

pub(crate) mod http;
pub(crate) mod m2m;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use ods_config::SecretRef;
use ods_sdk::contracts::credentials::CredentialError;
use ods_sdk::contracts::secrets::SecretValue;

use crate::workspace::{HostError, https_base};

/// A token is fetched again this long before it expires, so a request never goes out
/// with one about to lapse.
pub(crate) const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// Scopes asked for when `scopes` isn't set: the narrowest that serves SQL statements
/// first (ADR-0021 §2).
pub(crate) const DEFAULT_SCOPES: [&str; 2] = ["sql", "all-apis"];

/// Settings ADR-0021 names that this version can't use yet.
const NOT_YET: [&str; 3] = ["profile", "cli_path", "audience"];

/// A token, in memory only.
pub(crate) struct AccessToken {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "read by the first API call, #297's next step")
    )]
    pub(crate) value: SecretValue,
    /// When it expires, if the endpoint said.
    pub(crate) expires_at: Option<SystemTime>,
    /// The scope it was granted for.
    pub(crate) scope: String,
}

impl AccessToken {
    /// Whether it can still be used at `now`.
    pub(crate) fn fresh(&self, now: SystemTime) -> bool {
        self.expires_at.is_none_or(|at| {
            at.duration_since(now)
                .is_ok_and(|left| left > REFRESH_MARGIN)
        })
    }
}

/// The last token fetched, reused until it is about to expire.
#[derive(Default)]
pub(crate) struct TokenCache {
    slot: Mutex<Option<Arc<AccessToken>>>,
}

impl TokenCache {
    /// The cached token, if it is still fresh at `now`.
    pub(crate) fn fresh(&self, now: SystemTime) -> Option<Arc<AccessToken>> {
        let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        slot.as_ref().filter(|t| t.fresh(now)).cloned()
    }

    /// Keeps `token` for later requests.
    pub(crate) fn store(&self, token: Arc<AccessToken>) {
        *self.slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(token);
    }
}

/// How to sign in, as configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Method {
    /// `auth = "m2m"`.
    M2m {
        client_id: String,
        client_secret: SecretRef,
        scopes: Vec<String>,
    },
    /// `auth = "pat"`: a static token, only when named.
    Pat { token: SecretRef },
}

/// The workspace and how to sign in to it, read from a provider's `settings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) host: String,
    pub(crate) method: Method,
}

impl Settings {
    /// Reads `settings`. `host` is `DATABRICKS_HOST` from `env` when set, else the
    /// setting (ADR-0021 §3). Without `auth`, nothing else is read: a provider that
    /// names no method is one ODS doesn't sign in to (e.g. one set up for links), so
    /// that is what it is told, whatever else is missing.
    pub(crate) fn parse(
        settings: &toml::Table,
        env: &[(String, String)],
    ) -> Result<Self, CredentialError> {
        let Some(auth) = text(settings, "auth")? else {
            return Err(not_configured("auth"));
        };
        let from_env = env
            .iter()
            .find(|(k, v)| k == "DATABRICKS_HOST" && !v.trim().is_empty())
            .map(|(_, v)| v.as_str());
        let configured = match from_env {
            Some(_) => None,
            None => text(settings, "host")?,
        };
        let host = https_base(from_env.or(configured)).map_err(|e| match e {
            HostError::NotConfigured => not_configured("host"),
            HostError::Invalid(why) => invalid("host", why),
        })?;
        if let Some(key) = NOT_YET.iter().find(|k| settings.contains_key(**k)) {
            return Err(invalid(
                key,
                "isn't supported yet: use `auth = \"m2m\"` or `auth = \"pat\"`",
            ));
        }
        let method = match auth {
            "m2m" => Method::M2m {
                client_id: text(settings, "client_id")?
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| not_configured("client_id"))?
                    .to_owned(),
                client_secret: reference(settings, "client_secret")?,
                scopes: scopes(settings)?,
            },
            "pat" => Method::Pat {
                token: reference(settings, "token")?,
            },
            "cli" | "federated" => {
                return Err(invalid("auth", "isn't supported yet: use `m2m` or `pat`"));
            }
            _ => return Err(invalid("auth", "must be `m2m` or `pat`")),
        };
        Ok(Self { host, method })
    }
}

fn not_configured(setting: &str) -> CredentialError {
    CredentialError::NotConfigured {
        setting: setting.to_owned(),
    }
}

fn invalid(setting: &str, why: &str) -> CredentialError {
    CredentialError::InvalidSetting {
        setting: setting.to_owned(),
        why: why.to_owned(),
    }
}

fn text<'a>(settings: &'a toml::Table, key: &str) -> Result<Option<&'a str>, CredentialError> {
    match settings.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(invalid(key, "must be text")),
    }
}

/// A `{ secret = "<scheme>:<name>" }` reference. Configuration loading already refuses
/// a plaintext credential (ODS-E0103); this refuses anything else that isn't one.
fn reference(settings: &toml::Table, key: &str) -> Result<SecretRef, CredentialError> {
    let value = settings.get(key).ok_or_else(|| not_configured(key))?;
    value.clone().try_into::<SecretRef>().map_err(|_| {
        invalid(
            key,
            "must be a secret reference, e.g. { secret = \"env:VAR\" }",
        )
    })
}

fn scopes(settings: &toml::Table) -> Result<Vec<String>, CredentialError> {
    let Some(value) = settings.get("scopes") else {
        return Ok(DEFAULT_SCOPES.map(str::to_owned).to_vec());
    };
    let why = "must be a list of scope names, e.g. [\"sql\"]";
    let list = value.as_array().ok_or_else(|| invalid("scopes", why))?;
    let scopes: Vec<String> = list
        .iter()
        .map(|s| {
            s.as_str()
                .filter(|s| !s.is_empty() && !s.contains(char::is_whitespace))
                .map(str::to_owned)
                .ok_or_else(|| invalid("scopes", why))
        })
        .collect::<Result<_, _>>()?;
    if scopes.is_empty() {
        return Err(invalid("scopes", why));
    }
    Ok(scopes)
}

#[cfg(test)]
mod tests;
