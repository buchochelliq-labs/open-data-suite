//! Signing in, against a fake token endpoint: no network (ADR-0021 §7).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use ods_sdk::contracts::credentials::{AuthMethod, CredentialError, Credentials};
use ods_sdk::contracts::secrets::{SecretError, SecretValue};
use ods_sdk::secrets::{EnvSecrets, SecretResolver};

use super::http::{Https, Reply, Transport};
use super::{Method, Settings};
use crate::DatabricksCredentials;

const SECRET: &str = "ods-test-secret-9d2f";
const TOKEN: &str = "ods-test-token-41ab";
const HOST: &str = "https://dbc-1.cloud.databricks.com";

/// One request the fake endpoint saw.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    url: String,
    user: Option<String>,
    password_was_secret: bool,
    form: Vec<(String, String)>,
}

/// Answers each request with the next reply it was given, and records what it saw.
#[derive(Default)]
struct FakeEndpoint {
    replies: Mutex<VecDeque<Result<Reply, String>>>,
    seen: Mutex<Vec<Seen>>,
}

impl FakeEndpoint {
    fn answering(replies: impl IntoIterator<Item = (u16, String)>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|(status, body)| Ok(Reply { status, body }))
                    .collect(),
            ),
            seen: Mutex::default(),
        })
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl Transport for FakeEndpoint {
    async fn post_form(
        &self,
        url: &str,
        basic: Option<(&str, &SecretValue)>,
        form: &[(&str, &str)],
    ) -> Result<Reply, String> {
        self.seen.lock().unwrap().push(Seen {
            url: url.to_owned(),
            user: basic.map(|(u, _)| u.to_owned()),
            password_was_secret: basic.is_some_and(|(_, p)| p.expose() == SECRET),
            form: form
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        });
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err("no more replies".to_owned()))
    }
}

fn granted(scope: &str, expires_in: u64) -> (u16, String) {
    (
        200,
        format!(
            r#"{{"access_token":"{TOKEN}","token_type":"Bearer","expires_in":{expires_in},"scope":"{scope}"}}"#
        ),
    )
}

fn not_assigned(scope: &str) -> (u16, String) {
    (
        400,
        format!(
            r#"{{"error":"invalid_request","error_description":"The following scopes are not assigned: {scope}"}}"#
        ),
    )
}

fn secrets() -> SecretResolver {
    let mut resolver = SecretResolver::new();
    resolver
        .register(Arc::new(EnvSecrets::from_vars([
            ("CLIENT_SECRET", SECRET),
            ("PAT", TOKEN),
        ])))
        .unwrap();
    resolver
}

fn table(text: &str) -> toml::Table {
    text.parse().unwrap()
}

const M2M: &str = r#"
host = "dbc-1.cloud.databricks.com"
auth = "m2m"
client_id = "app-1"
client_secret = { secret = "env:CLIENT_SECRET" }
"#;

fn m2m(endpoint: &Arc<FakeEndpoint>) -> DatabricksCredentials {
    DatabricksCredentials::with_transport(
        "uc",
        Settings::parse(&table(M2M), &[]).unwrap(),
        secrets(),
        endpoint.clone(),
    )
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn epoch() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

#[test]
fn m2m_posts_the_client_credentials_grant_to_the_workspace_token_endpoint() {
    let endpoint = FakeEndpoint::answering([granted("sql", 3600)]);
    let credentials = m2m(&endpoint).with_clock(epoch);
    let status = block_on(credentials.status()).unwrap();
    assert_eq!(
        endpoint.seen(),
        [Seen {
            url: format!("{HOST}/oidc/v1/token"),
            user: Some("app-1".to_owned()),
            password_was_secret: true,
            form: vec![
                ("grant_type".to_owned(), "client_credentials".to_owned()),
                ("scope".to_owned(), "sql".to_owned()),
            ],
        }]
    );
    assert_eq!(status.method, AuthMethod::ServiceSecret);
    assert_eq!(status.principal.as_deref(), Some("app-1"));
    assert_eq!(status.scopes, ["sql"]);
    assert!(status.verified);
    assert_eq!(
        status.expires_at.map(ods_core::state::Timestamp::unix),
        Some(1_800_000_000 + 3600)
    );
}

#[test]
fn a_scope_not_assigned_falls_back_to_the_next_and_nothing_else_does() {
    let endpoint = FakeEndpoint::answering([not_assigned("sql"), granted("all-apis", 3600)]);
    let status = block_on(m2m(&endpoint).status()).unwrap();
    assert_eq!(status.scopes, ["all-apis"]);
    let asked: Vec<_> = endpoint
        .seen()
        .iter()
        .map(|s| s.form[1].1.clone())
        .collect();
    assert_eq!(asked, ["sql", "all-apis"]);

    // A wrong secret is refused at once, never retried with another scope.
    let endpoint = FakeEndpoint::answering([(
        401,
        r#"{"error":"invalid_client","error_description":"Client authentication failed"}"#
            .to_owned(),
    )]);
    let error = block_on(m2m(&endpoint).status()).unwrap_err();
    assert_eq!(endpoint.seen().len(), 1);
    assert!(
        matches!(&error, CredentialError::Refused { reason } if reason.starts_with("HTTP 401: ") && reason.contains("invalid_client")),
        "{error}"
    );

    let endpoint = FakeEndpoint::answering([not_assigned("sql"), not_assigned("all-apis")]);
    let error = block_on(m2m(&endpoint).status()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "sign-in was refused: none of the scopes sql, all-apis is assigned to the service principal"
    );
}

#[test]
fn the_secret_never_appears_in_what_a_refusal_says() {
    // An endpoint that quotes what it was sent.
    let endpoint = FakeEndpoint::answering([(400, format!("bad client secret {SECRET}"))]);
    let error = block_on(m2m(&endpoint).status()).unwrap_err();
    for shown in [error.to_string(), format!("{error:?}")] {
        assert!(!shown.contains(SECRET), "{shown}");
        assert!(shown.contains("[secret removed]"), "{shown}");
    }
    // A server error and an unreachable endpoint are worth retrying; nothing is shown
    // of a granted answer that isn't a token.
    let endpoint = FakeEndpoint::answering([(503, String::new())]);
    let error = block_on(m2m(&endpoint).status()).unwrap_err();
    assert!(error.is_retryable());
    assert_eq!(error.to_string(), "sign-in failed: HTTP 503");
    let endpoint = FakeEndpoint::answering([(200, format!("<html>{TOKEN}</html>"))]);
    let error = block_on(m2m(&endpoint).status()).unwrap_err();
    assert!(!error.to_string().contains(TOKEN), "{error}");
}

#[test]
fn a_secret_cut_by_the_message_limit_is_still_removed() {
    // The secret straddles the 500th character of what the endpoint said.
    let body = format!(
        "{}{SECRET}{}",
        "x".repeat(super::http::BODY_KEPT - 5),
        "y".repeat(50)
    );
    let endpoint = FakeEndpoint::answering([(400, body)]);
    let error = block_on(m2m(&endpoint).status()).unwrap_err();
    let shown = error.to_string();
    assert!(!shown.contains(&SECRET[..5]), "{shown}");
    assert!(shown.ends_with('…'), "cut after removing: {shown}");
}

#[test]
fn any_success_is_read_as_a_token_and_never_shown() {
    let endpoint = FakeEndpoint::answering([(201, format!("not json {TOKEN}"))]);
    let error = block_on(m2m(&endpoint).status()).unwrap_err();
    assert!(!error.to_string().contains(TOKEN), "{error}");
    let endpoint = FakeEndpoint::answering([granted("sql", 3600)]);
    let mut accepted = endpoint
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap()
        .unwrap();
    accepted.status = 202;
    endpoint.replies.lock().unwrap().push_back(Ok(accepted));
    assert!(block_on(m2m(&endpoint).status()).is_ok());
}

#[test]
fn a_rate_limit_or_timeout_is_worth_retrying() {
    for status in [408, 429, 502] {
        let endpoint = FakeEndpoint::answering([(status, "slow down".to_owned())]);
        let error = block_on(m2m(&endpoint).status()).unwrap_err();
        assert!(error.is_retryable(), "{status}: {error}");
    }
}

#[test]
fn a_token_without_an_expiry_is_never_reused() {
    let endpoint = FakeEndpoint::answering([
        (200, format!(r#"{{"access_token":"{TOKEN}"}}"#)),
        (200, format!(r#"{{"access_token":"{TOKEN}"}}"#)),
    ]);
    let credentials = m2m(&endpoint);
    block_on(credentials.token()).unwrap();
    block_on(credentials.token()).unwrap();
    assert_eq!(endpoint.seen().len(), 2, "fetched each time");
}

#[test]
fn a_token_is_reused_until_a_minute_before_it_expires() {
    fn early() -> SystemTime {
        epoch() + Duration::from_secs(3600 - 61)
    }
    fn late() -> SystemTime {
        epoch() + Duration::from_secs(3600 - 59)
    }
    let endpoint = FakeEndpoint::answering([granted("sql", 3600), granted("sql", 3600)]);
    let credentials = m2m(&endpoint).with_clock(epoch);
    block_on(credentials.token()).unwrap();
    let credentials = credentials.with_clock(early);
    block_on(credentials.token()).unwrap();
    assert_eq!(endpoint.seen().len(), 1, "fresh: reused");
    let credentials = credentials.with_clock(late);
    let token = block_on(credentials.token()).unwrap();
    assert_eq!(endpoint.seen().len(), 2, "about to expire: fetched again");
    assert_eq!(token.value.expose(), TOKEN);
}

#[test]
fn a_static_token_is_used_only_when_named_and_is_never_called_verified() {
    let settings = Settings::parse(
        &table(
            "host = \"https://dbc-1.cloud.databricks.com\"\nauth = \"pat\"\ntoken = { secret = \"env:PAT\" }\n",
        ),
        &[],
    )
    .unwrap();
    assert!(matches!(settings.method, Method::Pat { .. }));
    let endpoint = FakeEndpoint::answering([]);
    let credentials =
        DatabricksCredentials::with_transport("uc", settings, secrets(), endpoint.clone());
    let status = block_on(credentials.status()).unwrap();
    assert_eq!(status.method, AuthMethod::StaticToken);
    assert!(!status.verified);
    assert!(
        endpoint.seen().is_empty(),
        "nothing is sent for a static token"
    );
    // Without `auth`, nothing is chosen.
    assert_eq!(
        Settings::parse(
            &table("host = \"h.example\"\ntoken = { secret = \"env:PAT\" }\n"),
            &[]
        )
        .unwrap_err(),
        CredentialError::NotConfigured {
            setting: "auth".to_owned()
        }
    );
}

#[test]
fn a_missing_secret_names_only_its_reference() {
    let endpoint = FakeEndpoint::answering([]);
    let credentials = DatabricksCredentials::with_transport(
        "uc",
        Settings::parse(&table(M2M), &[]).unwrap(),
        SecretResolver::new(),
        endpoint.clone(),
    );
    let error = block_on(credentials.status()).unwrap_err();
    assert!(matches!(
        error,
        CredentialError::Secret(SecretError::UnknownScheme { .. })
    ));
    let mut env = SecretResolver::new();
    env.register(Arc::new(EnvSecrets::from_vars(
        Vec::<(String, String)>::new(),
    )))
    .unwrap();
    let credentials = DatabricksCredentials::with_transport(
        "uc",
        Settings::parse(&table(M2M), &[]).unwrap(),
        env,
        endpoint.clone(),
    );
    assert_eq!(
        block_on(credentials.status()).unwrap_err().to_string(),
        "secret(env:CLIENT_SECRET) is not set"
    );
    assert!(
        endpoint.seen().is_empty(),
        "nothing is sent without the secret"
    );
}

#[test]
fn settings_are_checked_before_anything_is_sent() {
    let parse = |text: &str| Settings::parse(&table(text), &[]).map(|_| ()).unwrap_err();
    let with = |extra: &str| format!("{M2M}{extra}\n");
    assert_eq!(
        parse("auth = \"m2m\""),
        CredentialError::NotConfigured {
            setting: "host".to_owned()
        }
    );
    assert!(matches!(
        parse("host = \"http://dbc-1.cloud.databricks.com\"\nauth = \"pat\""),
        CredentialError::InvalidSetting { setting, .. } if setting == "host"
    ));
    assert!(matches!(
        parse(&M2M.replace("{ secret = \"env:CLIENT_SECRET\" }", "\"pasted\"")),
        CredentialError::InvalidSetting { setting, why } if setting == "client_secret" && !why.contains("pasted")
    ));
    assert!(matches!(
        parse(&M2M.replace("auth = \"m2m\"", "auth = \"cli\"")),
        CredentialError::InvalidSetting { setting, why } if setting == "auth" && why.contains("isn't supported yet")
    ));
    assert!(matches!(
        parse(&with("profile = \"free\"")),
        CredentialError::InvalidSetting { setting, .. } if setting == "profile"
    ));
    assert!(matches!(
        parse(&with("scopes = []")),
        CredentialError::InvalidSetting { setting, .. } if setting == "scopes"
    ));
    assert!(matches!(
        parse(&M2M.replace("client_id = \"app-1\"", "")),
        CredentialError::NotConfigured { setting } if setting == "client_id"
    ));
    // DATABRICKS_HOST comes before the setting, as for links.
    let env = [(
        "DATABRICKS_HOST".to_owned(),
        "https://adb-2.3.azuredatabricks.net/".to_owned(),
    )];
    let settings = Settings::parse(&table(M2M), &env).unwrap();
    assert_eq!(settings.host, "https://adb-2.3.azuredatabricks.net");
    let scoped = Settings::parse(&table(&with("scopes = [\"all-apis\"]")), &[]).unwrap();
    assert!(matches!(scoped.method, Method::M2m { scopes, .. } if scopes == ["all-apis"]));
}

#[test]
fn debug_shows_references_never_values() {
    let endpoint = FakeEndpoint::answering([granted("sql", 3600)]);
    let credentials = m2m(&endpoint);
    block_on(credentials.token()).unwrap();
    let shown = format!("{credentials:?}");
    assert!(
        shown.contains("env:CLIENT_SECRET") || shown.contains("CLIENT_SECRET"),
        "{shown}"
    );
    assert!(!shown.contains(SECRET) && !shown.contains(TOKEN), "{shown}");
}

/// The SDK's conformance suite, over the fake endpoint.
mod conformance {
    use ods_sdk::conformance::credentials::{CredentialsHarness, run};

    use super::*;

    struct Harness;

    #[async_trait]
    impl CredentialsHarness for Harness {
        async fn provider(&self) -> Arc<dyn Credentials> {
            Arc::new(m2m(&FakeEndpoint::answering([granted("sql", 3600)])))
        }

        async fn failing(&self) -> Option<Arc<dyn Credentials>> {
            Some(Arc::new(m2m(&FakeEndpoint::answering([(
                400,
                format!("invalid client secret {SECRET}"),
            )]))))
        }

        fn markers(&self) -> Vec<String> {
            vec![SECRET.to_owned(), TOKEN.to_owned()]
        }
    }

    #[test]
    fn the_databricks_credentials_conform() {
        let report = block_on(run(&Harness));
        assert!(report.skipped.is_empty(), "{report:?}");
        assert_eq!(report.passed.len(), 3, "{report:?}");
    }
}

/// The real HTTP client, against a one-request HTTP endpoint on loopback.
mod over_http {
    use base64::Engine as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// What the endpoint received: the request head and body.
    type Received = tokio::sync::oneshot::Receiver<(String, String)>;

    /// Serves one request on loopback, answering with `answer` (a whole HTTP response).
    async fn serve_once(answer: String) -> (String, Received) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (tell, received) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            let (head, body) = loop {
                let n = socket.read(&mut buffer).await.unwrap();
                bytes.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .and_then(|n| n.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= length || n == 0 {
                        break (head.to_owned(), body.to_owned());
                    }
                }
            };
            socket.write_all(answer.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
            let _ = tell.send((head, body));
        });
        (format!("http://{address}"), received)
    }

    fn response(status: &str, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn the_client_sends_basic_auth_and_a_form_and_reads_the_token() {
        block_on(async {
            let body = format!(r#"{{"access_token":"{TOKEN}","expires_in":3600,"scope":"sql"}}"#);
            let (base, received) = serve_once(response("200 OK", "", &body)).await;
            let settings = Settings {
                host: base,
                method: Method::M2m {
                    client_id: "app-1".to_owned(),
                    client_secret: ods_config::SecretRef::parse("env:CLIENT_SECRET").unwrap(),
                    scopes: vec!["sql".to_owned()],
                },
            };
            let transport = Arc::new(Https::plain_for_tests().unwrap());
            let credentials =
                DatabricksCredentials::with_transport("uc", settings, secrets(), transport);
            let token = credentials.token().await.unwrap();
            assert_eq!(token.value.expose(), TOKEN);
            assert_eq!(token.scope, "sql");
            let (head, body) = received.await.unwrap();
            assert!(head.starts_with("POST /oidc/v1/token HTTP/1.1"), "{head}");
            let basic = base64::engine::general_purpose::STANDARD.encode(format!("app-1:{SECRET}"));
            assert!(
                head.lines()
                    .any(|l| l.eq_ignore_ascii_case(&format!("authorization: Basic {basic}"))),
                "no Basic authorization header"
            );
            assert_eq!(body, "grant_type=client_credentials&scope=sql");
        });
    }

    #[test]
    fn redirects_are_not_followed() {
        block_on(async {
            let (base, _) = serve_once(response(
                "307 Temporary Redirect",
                "location: http://127.0.0.1:9/oidc/v1/token\r\n",
                "",
            ))
            .await;
            let secret = SecretValue::new(SECRET.to_owned());
            let reply = Https::plain_for_tests()
                .unwrap()
                .post_form(
                    &format!("{base}/oidc/v1/token"),
                    Some(("app-1", &secret)),
                    &[("grant_type", "client_credentials")],
                )
                .await
                .unwrap();
            assert_eq!(reply.status, 307, "answered, not followed");
        });
    }

    #[test]
    fn the_real_client_refuses_plain_http() {
        block_on(async {
            let Err(error) = Https::new()
                .unwrap()
                .post_form("http://127.0.0.1:9/oidc/v1/token", None, &[])
                .await
            else {
                panic!("plain HTTP was sent");
            };
            assert!(error.contains("https://"), "{error}");
        });
    }
}
