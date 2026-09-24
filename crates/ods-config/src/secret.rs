//! Secret references (ADR-0005 §4).
//!
//! Configuration never holds a secret's value, only a reference to where it lives
//! (`{ secret = "env:DATABRICKS_TOKEN" }`). Resolving references is the job of
//! `SecretProvider`s (#126); this crate only parses them and rejects plaintext.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A reference to a secret held outside ODS, written `{ secret = "<scheme>:<name>" }`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "RawSecretRef", into = "RawSecretRef")]
pub struct SecretRef {
    scheme: String,
    name: String,
}

impl SecretRef {
    /// Parses `<scheme>:<name>`, e.g. `env:DATABRICKS_TOKEN`.
    ///
    /// # Errors
    /// Returns a message if either part is missing or the scheme is not lowercase
    /// ASCII letters, digits or `-`.
    pub fn parse(reference: &str) -> Result<Self, String> {
        let (scheme, name) = reference
            .split_once(':')
            .ok_or_else(|| format!("secret reference `{reference}` must be `<scheme>:<name>`"))?;
        let scheme_ok = !scheme.is_empty()
            && scheme
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !scheme_ok {
            return Err(format!(
                "secret reference `{reference}` has an invalid scheme"
            ));
        }
        if name.is_empty() {
            return Err(format!("secret reference `{reference}` has an empty name"));
        }
        Ok(Self {
            scheme: scheme.to_owned(),
            name: name.to_owned(),
        })
    }

    /// Where the secret lives, e.g. `env` or `keychain`.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// The secret's name within its scheme.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for SecretRef {
    /// Shows the reference, never a value: `secret(env:DATABRICKS_TOKEN)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "secret({}:{})", self.scheme, self.name)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSecretRef {
    secret: String,
}

impl TryFrom<RawSecretRef> for SecretRef {
    type Error = String;

    fn try_from(raw: RawSecretRef) -> Result<Self, Self::Error> {
        SecretRef::parse(&raw.secret)
    }
}

impl From<SecretRef> for RawSecretRef {
    fn from(secret: SecretRef) -> Self {
        RawSecretRef {
            secret: format!("{}:{}", secret.scheme, secret.name),
        }
    }
}

/// Key names that hold credentials, matched exactly or as a `_`-separated suffix
/// (`token`, `access_token`, …). Values under such keys must be [`SecretRef`]s.
const SECRET_KEY_WORDS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "private_key",
    "credentials",
    "client_secret",
];

/// Whether a config key name denotes a credential.
pub fn is_secret_key(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SECRET_KEY_WORDS
        .iter()
        .any(|word| name == *word || name.ends_with(&format!("_{word}")))
}

/// Whether a raw TOML value is a secret reference table (`{ secret = "…" }`).
pub(crate) fn is_secret_ref_value(value: &toml::Value) -> bool {
    matches!(value, toml::Value::Table(t) if t.len() == 1 && t.contains_key("secret"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_without_values() {
        let secret = SecretRef::parse("env:DATABRICKS_TOKEN").unwrap();
        assert_eq!(secret.scheme(), "env");
        assert_eq!(secret.name(), "DATABRICKS_TOKEN");
        assert_eq!(secret.to_string(), "secret(env:DATABRICKS_TOKEN)");
    }

    #[test]
    fn rejects_malformed_references() {
        for bad in ["DATABRICKS_TOKEN", ":x", "env:", "Env:x", "e v:x"] {
            assert!(SecretRef::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn round_trips_through_toml() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Holder {
            token: SecretRef,
        }
        let holder: Holder = toml::from_str(r#"token = { secret = "env:T" }"#).unwrap();
        assert_eq!(holder.token, SecretRef::parse("env:T").unwrap());
        let json = serde_json::to_string(&holder).unwrap();
        assert_eq!(json, r#"{"token":{"secret":"env:T"}}"#);
    }

    #[test]
    fn secret_keys_are_detected_by_word_boundaries() {
        for key in [
            "token",
            "access_token",
            "PASSWORD",
            "client_secret",
            "api_key",
            "private_key",
        ] {
            assert!(is_secret_key(key), "{key}");
        }
        for key in [
            "token_url",
            "tokens_per_minute",
            "host",
            "secretary",
            "keyspace",
        ] {
            assert!(!is_secret_key(key), "{key}");
        }
    }
}
