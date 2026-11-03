//! M2M: a service principal's OAuth secret, exchanged for a token with the
//! `client_credentials` grant at `<host>/oidc/v1/token` (ADR-0021 §1, §2).

use std::time::{Duration, SystemTime};

use ods_sdk::contracts::credentials::CredentialError;
use ods_sdk::contracts::secrets::SecretValue;
use serde::Deserialize;

use super::AccessToken;
use super::http::{Reply, Transport, truncated};

/// What Databricks says when a scope isn't granted to the service principal (#295;
/// `.github/databricks/ci.py` matches the same words).
const NOT_ASSIGNED: &str = "are not assigned";

#[derive(Deserialize)]
struct Granted {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

/// Asks for a token for each of `scopes` in turn, narrowest first, while Databricks
/// answers that the scope isn't assigned; any other refusal stops at once, so a wrong
/// secret is never retried.
pub(crate) async fn grant(
    transport: &dyn Transport,
    host: &str,
    client_id: &str,
    secret: &SecretValue,
    scopes: &[String],
    now: SystemTime,
) -> Result<AccessToken, CredentialError> {
    let url = format!("{host}/oidc/v1/token");
    let mut unassigned = Vec::new();
    for scope in scopes {
        let form = [
            ("grant_type", "client_credentials"),
            ("scope", scope.as_str()),
        ];
        let reply = transport
            .post_form(&url, Some((client_id, secret)), &form)
            .await
            .map_err(|reason| CredentialError::Unavailable {
                reason: secret.redact_in(&reason).into_owned(),
            })?;
        match reply.status {
            // Any success holds the token: its body is never shown.
            200..=299 => return token(&reply, scope, now),
            // A timeout, a rate limit or a server error: worth trying again later.
            408 | 429 | 500.. => {
                return Err(CredentialError::Unavailable {
                    reason: said(&reply, secret),
                });
            }
            _ if reply.body.contains(NOT_ASSIGNED) => unassigned.push(scope.as_str()),
            _ => {
                return Err(CredentialError::Refused {
                    reason: said(&reply, secret),
                });
            }
        }
    }
    Err(CredentialError::Refused {
        reason: format!(
            "none of the scopes {} is assigned to the service principal",
            unassigned.join(", ")
        ),
    })
}

/// The token in a `200` answer.
fn token(reply: &Reply, scope: &str, now: SystemTime) -> Result<AccessToken, CredentialError> {
    // The body holds the token: it is never put in an error.
    let granted: Granted =
        serde_json::from_str(&reply.body).map_err(|_| CredentialError::Refused {
            reason: "the token endpoint's answer isn't a token".to_owned(),
        })?;
    if granted.access_token.is_empty() {
        return Err(CredentialError::Refused {
            reason: "the token endpoint granted an empty token".to_owned(),
        });
    }
    Ok(AccessToken {
        value: SecretValue::new(granted.access_token),
        expires_at: granted
            .expires_in
            .map(|seconds| now + Duration::from_secs(seconds)),
        scope: granted.scope.unwrap_or_else(|| scope.to_owned()),
    })
}

/// What a refusal said: its status and the start of its body, with the secret removed
/// before it is cut.
fn said(reply: &Reply, secret: &SecretValue) -> String {
    let body = truncated(&secret.redact_in(reply.body.trim()));
    if body.is_empty() {
        format!("HTTP {}", reply.status)
    } else {
        format!("HTTP {}: {body}", reply.status)
    }
}
