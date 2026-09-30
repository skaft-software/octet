//! One-shot, loopback-only browser authorization for a ChatGPT subscription.

use std::fmt::Write as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ring::rand::{SecureRandom as _, SystemRandom};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

use super::store::CredentialStore;
use super::{login, oauth, CLIENT_ID, ORIGINATOR};

const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_REQUEST_BYTES: usize = 8 * 1024;
const MARK: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/assets/octet/marks/mark-gradient.svg"
));

/// The loopback address for one registered callback port.
pub(super) fn callback_address(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// The redirect registered for `port`. Like the current Codex CLI it names
/// `127.0.0.1`, never `localhost`, which can resolve to IPv6 first.
pub(super) fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/auth/callback")
}

pub(super) struct Authorization {
    verifier: String,
    state: String,
}

impl Authorization {
    pub(super) fn generate() -> Result<Self> {
        let random = SystemRandom::new();
        let mut verifier = [0; 32];
        let mut state = [0; 32];
        random
            .fill(&mut verifier)
            .map_err(|_| anyhow::anyhow!("generating PKCE verifier failed"))?;
        random
            .fill(&mut state)
            .map_err(|_| anyhow::anyhow!("generating OAuth state failed"))?;
        let mut state_hex = String::with_capacity(64);
        for byte in state {
            write!(state_hex, "{byte:02x}").expect("formatting a byte cannot fail");
        }
        Ok(Self {
            verifier: URL_SAFE_NO_PAD.encode(verifier),
            state: state_hex,
        })
    }

    pub(super) fn url(&self, redirect_uri: &str) -> String {
        authorize_url(redirect_uri, &self.verifier, &self.state)
    }
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn authorize_url(redirect_uri: &str, verifier: &str, state: &str) -> String {
    let mut url = Url::parse(AUTHORIZE_URL).expect("static OAuth URL is valid");
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", "openid profile email offline_access")
        .append_pair("code_challenge", &challenge(verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", ORIGINATOR);
    url.into()
}

pub(super) async fn bind(address: SocketAddr) -> std::io::Result<TcpListener> {
    // Never listen on `localhost`: that can resolve to a non-loopback interface
    // on a misconfigured host. Production pins IPv4 loopback and a registered
    // callback port.
    assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
    TcpListener::bind(address).await
}

/// How a browser sign-in that received a valid callback ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BrowserSignIn {
    /// The credential was validated and stored.
    Saved,
    /// OpenAI issued a localhost-only credential, which cannot reach the
    /// ChatGPT model pool. Nothing was stored; finish with a device code.
    LimitedCredential,
}

/// Serve until the first valid callback, cancellation, or the five-minute deadline.
/// A matching code is exchanged and stored *before* sending the success page.
pub(super) async fn serve(
    listener: TcpListener,
    store: &CredentialStore,
    http: &reqwest::Client,
    token_url: &str,
    redirect_uri: &str,
    authorization: &Authorization,
) -> Result<BrowserSignIn> {
    tokio::select! {
        result = tokio::time::timeout(CALLBACK_TIMEOUT, serve_callbacks(listener, store, http, token_url, redirect_uri, authorization)) => {
            result.context("browser sign-in timed out after 5 minutes")?
        }
        _ = crate::tui::terminal::wait_for_shutdown_signal() => {
            anyhow::bail!("browser sign-in cancelled")
        }
    }
}

async fn serve_callbacks(
    listener: TcpListener,
    store: &CredentialStore,
    http: &reqwest::Client,
    token_url: &str,
    redirect_uri: &str,
    authorization: &Authorization,
) -> Result<BrowserSignIn> {
    let port = listener.local_addr()?.port();
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .context("accepting browser callback failed")?;
        let request = read_request(&mut stream).await;
        let action = request
            .as_deref()
            .map(|line| callback(line, port, &authorization.state))
            .unwrap_or(Callback::NotFound);
        match action {
            Callback::NotFound => {
                let _ = respond(
                    &mut stream,
                    404,
                    &page("Not found", "Not found", "This is not a sign-in callback."),
                )
                .await;
            }
            Callback::StateMismatch => {
                let _ = respond(
                    &mut stream,
                    400,
                    &page(
                        "Sign-in failed",
                        "State mismatch",
                        "The sign-in state did not match. Return to your terminal and try again.",
                    ),
                )
                .await;
            }
            Callback::Denied { access_denied } => {
                let reason = if access_denied {
                    "Access denied"
                } else {
                    "Authorization failed"
                };
                let _ = respond(&mut stream, 400, &page("Sign-in failed", reason, "OpenAI did not authorize this sign-in. Return to your terminal and try again.")).await;
                anyhow::bail!("OpenAI denied browser sign-in");
            }
            Callback::Code(code) => {
                // Do not include a code, verifier, token, query string, or remote
                // response body in diagnostics or the rendered HTML.
                let result = oauth::exchange_browser_code_with_url(
                    http,
                    token_url,
                    &code,
                    &authorization.verifier,
                    redirect_uri,
                )
                .await;
                let tokens = match result {
                    Ok(tokens) => tokens,
                    Err(_) => {
                        let _ = respond(&mut stream, 400, &page("Sign-in failed", "Token exchange failed", "OpenAI did not issue a usable credential. Return to your terminal and try again.")).await;
                        anyhow::bail!("browser sign-in token exchange failed");
                    }
                };
                if oauth::is_localhost_only(&tokens.access) {
                    let _ = respond(
                        &mut stream,
                        200,
                        &page(
                            "Finish in your terminal",
                            "Finish signing in from your terminal.",
                            "OpenAI issued a limited credential for this browser sign-in, so octet will finish with a device code. Return to your terminal.",
                        ),
                    )
                    .await;
                    return Ok(BrowserSignIn::LimitedCredential);
                }
                if login::save_tokens(store, tokens).await.is_err() {
                    let _ = respond(&mut stream, 400, &page("Sign-in failed", "Sign-in could not be saved", "The credential could not be validated or saved. Return to your terminal and try again.")).await;
                    anyhow::bail!("browser sign-in credential could not be validated or saved");
                }
                let _ = respond(
                    &mut stream,
                    200,
                    &page(
                        "Signed in to octet",
                        "Signed in to octet.",
                        "You can close this page and return to your terminal.",
                    ),
                )
                .await;
                return Ok(BrowserSignIn::Saved);
            }
        }
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<String> {
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut request = Vec::new();
        let mut chunk = [0; 1024];
        loop {
            let count = stream.read(&mut chunk).await.ok()?;
            if count == 0 || request.len() + count > MAX_REQUEST_BYTES {
                return None;
            }
            request.extend_from_slice(&chunk[..count]);
            if request.ends_with(b"\r\n\r\n")
                || request.windows(4).any(|bytes| bytes == b"\r\n\r\n")
            {
                return String::from_utf8(request).ok();
            }
        }
    })
    .await
    .ok()?
}

enum Callback {
    NotFound,
    StateMismatch,
    Denied { access_denied: bool },
    Code(String),
}

fn callback(request: &str, port: u16, expected_state: &str) -> Callback {
    let mut lines = request.split("\r\n");
    let Some(request_line) = lines.next() else {
        return Callback::NotFound;
    };
    let mut parts = request_line.split(' ');
    let (Some("GET"), Some(target), Some("HTTP/1.1" | "HTTP/1.0"), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Callback::NotFound;
    };
    let mut hosts = lines.filter_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case("host").then(|| value.trim())
    });
    let host = hosts.next();
    if hosts.next().is_some()
        || !matches!(host, Some(value) if value == format!("localhost:{port}") || value == format!("127.0.0.1:{port}"))
    {
        return Callback::NotFound;
    }
    let Some(query) = target.strip_prefix("/auth/callback?") else {
        return Callback::NotFound;
    };
    if query.contains('#') {
        return Callback::NotFound;
    }
    let pairs: Vec<_> = url::form_urlencoded::parse(query.as_bytes()).collect();
    if pairs.len() != 2 && pairs.len() != 3 {
        return Callback::NotFound;
    }
    let mut state = None;
    let mut code = None;
    let mut error = None;
    let mut error_description = None;
    for (key, value) in pairs {
        let slot = match key.as_ref() {
            "state" => &mut state,
            "code" => &mut code,
            "error" => &mut error,
            "error_description" => &mut error_description,
            _ => return Callback::NotFound,
        };
        if slot.replace(value.into_owned()).is_some() {
            return Callback::NotFound;
        }
    }
    let Some(state) = state else {
        return Callback::NotFound;
    };
    if state != expected_state {
        return Callback::StateMismatch;
    }
    match (code, error, error_description) {
        (Some(code), None, None) if !code.is_empty() && !code.chars().any(char::is_control) => {
            Callback::Code(code)
        }
        (None, Some(error), _) if !error.is_empty() => Callback::Denied {
            access_denied: error == "access_denied",
        },
        _ => Callback::NotFound,
    }
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

fn page(title: &str, heading: &str, message: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title}</title><style>
:root{{color-scheme:light dark;font-family:system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;background:#f7f9ff;color:#151b30}}
body{{margin:0;min-height:100vh;display:grid;place-items:center;padding:24px;box-sizing:border-box}}
main{{width:min(100%,440px);background:#fff;border:1px solid #e1e8f3;border-radius:22px;padding:42px;box-shadow:0 20px 60px #273c6914}}
svg{{width:64px;height:32px}}h1{{font-size:1.55rem;letter-spacing:-.035em;margin:32px 0 12px}}p{{color:#53617d;line-height:1.6;margin:0}}
@media(prefers-color-scheme:dark){{:root{{background:#0e1424;color:#f6f8ff}}main{{background:#171f31;border-color:#313c53;box-shadow:0 20px 60px #0004}}p{{color:#b5bfd1}}}}
</style></head><body><main>{MARK}<h1>{heading}</h1><p>{message}</p></main></body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_appendix_b_and_random_values_have_required_lengths() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let a = Authorization::generate().unwrap();
        let b = Authorization::generate().unwrap();
        assert_eq!(a.verifier.len(), 43);
        assert!(a
            .verifier
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)));
        assert_eq!(a.state.len(), 64);
        assert!(a.state.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.state, b.state);
    }

    #[test]
    fn authorize_url_has_exact_registration_parameters_and_backend_originator() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let state = "aa".repeat(32);
        assert_eq!(authorize_url(&redirect_uri(1455), verifier, &state), format!(
            "https://auth.openai.com/oauth/authorize?response_type=code&client_id=app_EMoamEEZ73f0CkXaXp7hrann&redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback&scope=openid+profile+email+offline_access&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&state={state}&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=octet"
        ));
        let declarations: serde_json::Value =
            serde_json::from_str(include_str!("../../providers/declarations.json")).unwrap();
        let codex = declarations["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == "codex")
            .unwrap();
        assert!(codex["extra_headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|header| header[0] == "originator" && header[1] == ORIGINATOR));
    }

    #[test]
    fn callback_rejects_bad_state_path_method_and_extra_fields() {
        let request =
            |target: &str| format!("GET {target} HTTP/1.1\r\nHost: localhost:1455\r\n\r\n");
        assert!(matches!(
            callback(
                &request("/auth/callback?code=secret&state=wrong"),
                1455,
                "right"
            ),
            Callback::StateMismatch
        ));
        assert!(matches!(
            callback(&request("/else?code=secret&state=right"), 1455, "right"),
            Callback::NotFound
        ));
        assert!(matches!(
            callback(
                &request("/auth/callback?code=secret&state=right&other=x"),
                1455,
                "right"
            ),
            Callback::NotFound
        ));
        assert!(matches!(
            callback(
                &request("/auth/callback?code=secret&state=right&state=right"),
                1455,
                "right"
            ),
            Callback::NotFound
        ));
        assert!(matches!(callback("POST /auth/callback?code=secret&state=right HTTP/1.1\r\nHost: localhost:1455\r\n\r\n", 1455, "right"), Callback::NotFound));
        assert!(matches!(
            callback(
                &request("/auth/callback?error=access_denied&state=right"),
                1455,
                "right"
            ),
            Callback::Denied {
                access_denied: true
            }
        ));
        assert!(matches!(
            callback(
                &request("/auth/callback?code=secret&state=right"),
                1455,
                "right"
            ),
            Callback::Code(_)
        ));
    }

    #[tokio::test]
    async fn browser_round_trip_exchanges_and_persists_then_stops() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let token = MockServer::start().await;
        let claims = serde_json::json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acct_test", "chatgpt_plan_type": "plus" }});
        let access = format!(
            "h.{}.s",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        );
        Mock::given(method("POST")).and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"))
            .and(body_string_contains("code=browser-secret"))
            .and(body_string_contains("code_verifier=verifier"))
            .and(body_string_contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token": access, "refresh_token": "refresh-secret", "expires_in": 3600})))
            .expect(1).mount(&token).await;
        let listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let redirect_uri = redirect_uri(addr.port());
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().join("codex.json"));
        let http = super::super::http_client();
        let auth = Authorization {
            verifier: "verifier".into(),
            state: "test-state".into(),
        };
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let server = tokio::spawn({
            let store = store.clone();
            let token_url = format!("{}/token", token.uri());
            let redirect_uri = redirect_uri.clone();
            async move { serve(listener, &store, &http, &token_url, &redirect_uri, &auth).await }
        });
        let base = format!("http://{addr}");
        let wrong = client
            .get(format!("{base}/wrong?code=browser-secret&state=test-state"))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), 404);
        let wrong_state = client
            .get(format!(
                "{base}/auth/callback?code=browser-secret&state=wrong"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong_state.status(), 400);
        assert!(wrong_state.text().await.unwrap().contains("State mismatch"));
        assert!(store.load().unwrap().is_none());
        let response = client
            .get(format!(
                "{base}/auth/callback?code=browser-secret&state=test-state"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = response.text().await.unwrap();
        assert!(body.contains("Signed in to octet."));
        assert!(body.contains("You can close this page and return to your terminal."));
        assert!(body.contains("prefers-color-scheme"));
        assert!(body.contains("data-bit=\"0\""));
        assert!(!body.contains("browser-secret"));
        assert_eq!(server.await.unwrap().unwrap(), BrowserSignIn::Saved);
        let saved = store.load().unwrap().unwrap();
        assert_eq!(saved.tokens.account_id, "acct_test");
        assert_eq!(saved.tokens.refresh_token, "refresh-secret");
        token.verify().await;
        assert!(tokio::net::TcpStream::connect(addr).await.is_err());
    }

    #[tokio::test]
    async fn localhost_only_credential_is_not_stored_and_asks_for_a_device_code() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let token = MockServer::start().await;
        let claims = serde_json::json!({ "https://api.openai.com/auth": {
            "chatgpt_account_id": "acct_test", "chatgpt_plan_type": "plus", "localhost": true
        }});
        let access = format!(
            "h.{}.s",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
        );
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": access, "refresh_token": "refresh-secret", "expires_in": 3600
            })))
            .expect(1)
            .mount(&token)
            .await;
        let listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().join("codex.json"));
        let http = super::super::http_client();
        let auth = Authorization {
            verifier: "verifier".into(),
            state: "test-state".into(),
        };
        let server = tokio::spawn({
            let store = store.clone();
            let token_url = format!("{}/token", token.uri());
            let redirect_uri = redirect_uri(addr.port());
            async move { serve(listener, &store, &http, &token_url, &redirect_uri, &auth).await }
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let response = client
            .get(format!(
                "http://{addr}/auth/callback?code=browser-secret&state=test-state"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = response.text().await.unwrap();
        assert!(body.contains("Finish signing in from your terminal."));
        assert!(!body.contains("Signed in to octet."));
        assert_eq!(
            server.await.unwrap().unwrap(),
            BrowserSignIn::LimitedCredential
        );
        assert!(store.load().unwrap().is_none());
        token.verify().await;
    }

    #[tokio::test]
    async fn denied_and_exchange_errors_render_redacted_pages_and_stop() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().join("codex.json"));
        let http = super::super::http_client();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for query in [
            "error=access_denied&state=expected",
            "code=secret-code&state=expected",
        ] {
            let listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .await
                .unwrap();
            let addr = listener.local_addr().unwrap();
            let auth = Authorization {
                verifier: "secret-verifier".into(),
                state: "expected".into(),
            };
            let error_url = "http://127.0.0.1:1/token";
            let job = tokio::spawn({
                let store = store.clone();
                let http = http.clone();
                async move {
                    serve(
                        listener,
                        &store,
                        &http,
                        error_url,
                        &redirect_uri(1455),
                        &auth,
                    )
                    .await
                }
            });
            let response = client
                .get(format!("http://{addr}/auth/callback?{query}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 400);
            let page = response.text().await.unwrap();
            assert!(page.contains("Return to your terminal and try again."));
            assert!(!page.contains("secret-code"));
            assert!(!page.contains("secret-verifier"));
            assert!(job.await.unwrap().is_err());
            assert!(store.load().unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn occupied_loopback_port_is_reported_for_device_fallback() {
        let listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .unwrap();
        let err = bind(listener.local_addr().unwrap()).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    }
}
