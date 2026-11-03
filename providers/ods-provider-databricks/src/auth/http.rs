//! The one HTTP request signing in needs, behind a trait so the flows are tested
//! without a network (ADR-0021 §7).

use std::time::Duration;

use async_trait::async_trait;
use ods_sdk::contracts::secrets::SecretValue;

/// How much of an answer's body is read at most: a token answer is far smaller, and a
/// host that sends more isn't a token endpoint.
const BODY_READ: usize = 64 * 1024;

/// How much of an answer's body an error message keeps, once secrets are removed.
pub(crate) const BODY_KEPT: usize = 500;

/// An HTTP answer: its status and body (at most [`BODY_READ`] bytes). It may hold a
/// token, so it has no `Debug` and is never shown as is.
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: String,
}

/// Sends a form, with HTTP Basic credentials if given. Never logs the request.
#[async_trait]
pub(crate) trait Transport: Send + Sync {
    /// POSTs `form` (URL-encoded) to `url`.
    ///
    /// # Errors
    /// What went wrong when nothing was answered (DNS, TLS, a timeout, …). The message
    /// names the URL, which holds no secret.
    async fn post_form(
        &self,
        url: &str,
        basic: Option<(&str, &SecretValue)>,
        form: &[(&str, &str)],
    ) -> Result<Reply, String>;
}

/// The real transport: HTTPS only, TLS always verified, no redirects followed.
pub(crate) struct Https {
    client: reqwest::Client,
}

impl Https {
    /// A transport over HTTPS.
    ///
    /// # Errors
    /// The TLS stack couldn't be set up (e.g. no root certificates could be read).
    pub(crate) fn new() -> Result<Self, String> {
        Self::build(true)
    }

    /// A transport that also speaks plain HTTP, for a test endpoint on loopback.
    #[cfg(test)]
    pub(crate) fn plain_for_tests() -> Result<Self, String> {
        Self::build(false)
    }

    fn build(https_only: bool) -> Result<Self, String> {
        reqwest::Client::builder()
            .https_only(https_only)
            // A redirect could carry the credentials to another host (ADR-0021 §5).
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()
            .map(|client| Self { client })
            .map_err(|e| format!("the HTTP client couldn't be set up: {e}"))
    }
}

#[async_trait]
impl Transport for Https {
    async fn post_form(
        &self,
        url: &str,
        basic: Option<(&str, &SecretValue)>,
        form: &[(&str, &str)],
    ) -> Result<Reply, String> {
        let mut request = self.client.post(url).form(form);
        if let Some((user, password)) = basic {
            request = request.basic_auth(user, Some(password.expose()));
        }
        let mut response = request.send().await.map_err(|e| {
            if e.is_builder() {
                return "the request couldn't be built: the URL must be a valid https:// URL"
                    .to_owned();
            }
            // The cause chain names the host and what failed, never headers or a body.
            let e = e.without_url();
            let mut message = e.to_string();
            let mut source = std::error::Error::source(&e);
            while let Some(cause) = source {
                message.push_str(": ");
                message.push_str(&cause.to_string());
                source = cause.source();
            }
            message
        })?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("the answer couldn't be read: {}", e.without_url()))?
        {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > BODY_READ {
                bytes.truncate(BODY_READ);
                break;
            }
        }
        Ok(Reply {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

/// The first [`BODY_KEPT`] characters of `body`. Remove secrets first: a secret cut in
/// two can't be found whole any more.
pub(crate) fn truncated(body: &str) -> String {
    match body.char_indices().nth(BODY_KEPT) {
        Some((at, _)) => format!("{}…", &body[..at]),
        None => body.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_bodies_are_cut() {
        assert_eq!(truncated("short"), "short");
        let long = "é".repeat(BODY_KEPT + 10);
        let cut = truncated(&long);
        assert_eq!(cut.chars().count(), BODY_KEPT + 1);
        assert!(cut.ends_with('…'));
    }
}
