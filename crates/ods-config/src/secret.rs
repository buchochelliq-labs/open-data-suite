//! Secret references (ADR-0005 §4).
//!
//! Configuration never holds a secret's value, only a reference to where it lives
//! (`{ secret = "env:DATABRICKS_TOKEN" }`). Resolving references is the job of
//! `SecretProvider`s (#126); this crate parses them and rejects plaintext.

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
    /// Returns a reason if either part is missing or the scheme is not lowercase ASCII
    /// letters, digits or `-`. The reason never repeats the input, because a mistyped
    /// reference may be a pasted secret.
    pub fn parse(reference: &str) -> Result<Self, String> {
        let (scheme, name) = reference
            .split_once(':')
            .ok_or_else(|| "a secret reference must be `<scheme>:<name>`".to_owned())?;
        let scheme_ok = !scheme.is_empty()
            && scheme
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !scheme_ok {
            return Err(
                "a secret reference's scheme must be lowercase letters, digits or `-`".to_owned(),
            );
        }
        if name.is_empty() {
            return Err("a secret reference's name must not be empty".to_owned());
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
/// (`token`, `access_token`, …). Values at or under such keys must be [`SecretRef`]s.
const SECRET_KEY_WORDS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "api_key",
    "apikey",
    "private_key",
    "credentials",
    "client_secret",
    "connection_string",
    "dsn",
];

/// Whether a config key name denotes a credential.
pub fn is_secret_key(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SECRET_KEY_WORDS
        .iter()
        .any(|word| name == *word || name.ends_with(&format!("_{word}")))
}

/// Whether a raw TOML value has the shape of a secret reference (`{ secret = … }`).
pub(crate) fn is_secret_ref_value(value: &toml::Value) -> bool {
    matches!(value, toml::Value::Table(t) if t.len() == 1 && t.contains_key("secret"))
}

/// Why a value breaks the secret rules; never contains the value itself.
pub(crate) enum SecretViolation {
    /// A credential-named key (or something under one) holds a plaintext value.
    Plaintext {
        /// Path to the offending value, relative to the checked value.
        path: Vec<String>,
    },
    /// A `{ secret = … }` reference is malformed.
    InvalidReference {
        /// Path to the reference.
        path: Vec<String>,
        /// Why, without the reference text.
        reason: String,
    },
}

/// Checks a value set at `key` against the secret rules (ADR-0005 §4):
/// every reference must parse, and every value at or under a credential-named key must
/// be a reference. Walks nested tables and arrays (array elements are `[n]`).
pub(crate) fn check_secrets(key: &[String], value: &toml::Value) -> Result<(), SecretViolation> {
    fn walk(path: &mut Vec<String>, value: &toml::Value) -> Result<(), SecretViolation> {
        if is_secret_ref_value(value) {
            let reference = value.get("secret").and_then(toml::Value::as_str);
            return match reference.map(SecretRef::parse) {
                Some(Ok(_)) => Ok(()),
                Some(Err(reason)) => Err(SecretViolation::InvalidReference {
                    path: path.clone(),
                    reason,
                }),
                None => Err(SecretViolation::InvalidReference {
                    path: path.clone(),
                    reason: "`secret` must be a string `<scheme>:<name>`".to_owned(),
                }),
            };
        }
        match value {
            toml::Value::Table(table) => {
                for (key, inner) in table {
                    path.push(key.clone());
                    walk(path, inner)?;
                    path.pop();
                }
                Ok(())
            }
            toml::Value::Array(items) => {
                for (index, inner) in items.iter().enumerate() {
                    path.push(format!("[{index}]"));
                    walk(path, inner)?;
                    path.pop();
                }
                Ok(())
            }
            _ if path.iter().any(|segment| is_secret_key(segment)) => {
                Err(SecretViolation::Plaintext { path: path.clone() })
            }
            _ => Ok(()),
        }
    }
    walk(&mut key.to_vec(), value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(key: &str, toml_value: &str) -> Result<(), SecretViolation> {
        let table: toml::Table = format!("v = {toml_value}").parse().unwrap();
        let key: Vec<String> = key.split('.').map(str::to_owned).collect();
        check_secrets(&key, &table["v"])
    }

    #[test]
    fn parses_and_displays_without_values() {
        let secret = SecretRef::parse("env:DATABRICKS_TOKEN").unwrap();
        assert_eq!(secret.scheme(), "env");
        assert_eq!(secret.name(), "DATABRICKS_TOKEN");
        assert_eq!(secret.to_string(), "secret(env:DATABRICKS_TOKEN)");
    }

    #[test]
    fn rejects_malformed_references_without_echoing_them() {
        for bad in [
            "DATABRICKS_TOKEN",
            ":x",
            "env:",
            "Env:x",
            "e v:x",
            "hunter2",
        ] {
            let reason = SecretRef::parse(bad).unwrap_err();
            assert!(!reason.contains(bad), "{reason}");
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
            "passphrase",
            "dsn",
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

    #[test]
    fn plaintext_is_found_at_any_depth() {
        assert!(check("providers.w.settings.token", r#"{ secret = "env:T" }"#).is_ok());
        assert!(check("providers.w.settings.host", r#""h""#).is_ok());
        for (key, value) in [
            ("providers.w.settings.token", r#""plain""#),
            ("providers.w.settings.token", r#"{ value = "plain" }"#),
            ("providers.w.settings.conn", r#"[{ password = "plain" }]"#),
            ("policy.rules.api_key", r#""plain""#),
        ] {
            assert!(
                matches!(check(key, value), Err(SecretViolation::Plaintext { .. })),
                "{key} = {value}"
            );
        }
    }

    #[test]
    fn every_reference_must_parse() {
        for value in [
            r#"{ secret = "hunter4" }"#,
            r"{ secret = 7 }",
            r#"[{ secret = "x" }]"#,
        ] {
            assert!(
                matches!(
                    check("providers.w.settings.anything", value),
                    Err(SecretViolation::InvalidReference { .. })
                ),
                "{value}"
            );
        }
    }
}
