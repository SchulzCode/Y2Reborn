//! OAuth authorization-code flow with PKCE against the desktop client ID (spec section 5.4).
//!
//! The app opens `authorize_url()` in a browser; Spotify redirects to the fixed loopback
//! address, which `wait_for_callback()` serves once; the code is exchanged for an access
//! token that becomes `Credentials::access_token`. No refresh token is kept: the access
//! point turns the token into reusable stored credentials.

use std::net::SocketAddr;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

use crate::auth::credentials::Credentials;
use crate::{consts, http, random, Error, Result};

const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub struct PkceFlow {
    client_id: String,
    redirect_uri: String,
    authorize_url: String,
    token_url: String,
    verifier: String,
    state: String,
    http: reqwest::Client,
}

/// A flow for Spotify's desktop client ID with the redirect registered for it.
pub fn pkce() -> PkceFlow {
    PkceFlow::with_endpoints(
        consts::CLIENT_ID_DESKTOP,
        consts::OAUTH_REDIRECT_URI,
        consts::OAUTH_AUTHORIZE_URL,
        consts::OAUTH_TOKEN_URL,
        consts::OAUTH_SCOPES,
    )
}

/// `base64url(SHA256(verifier))` without padding, per RFC 7636.
pub(crate) fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

impl PkceFlow {
    pub(crate) fn with_endpoints(
        client_id: &str,
        redirect_uri: &str,
        authorize_endpoint: &str,
        token_url: &str,
        scopes: &[&str],
    ) -> Self {
        let verifier = URL_SAFE_NO_PAD.encode(random::random_bytes::<32>());
        let state = URL_SAFE_NO_PAD.encode(random::random_bytes::<16>());

        let mut url = Url::parse(authorize_endpoint).expect("authorize endpoint is a valid url");
        url.query_pairs_mut()
            .append_pair("client_id", client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &scopes.join(" "))
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge_for(&verifier))
            .append_pair("code_challenge_method", "S256");

        PkceFlow {
            client_id: client_id.to_string(),
            redirect_uri: redirect_uri.to_string(),
            authorize_url: url.into(),
            token_url: token_url.to_string(),
            verifier,
            state,
            http: http::client(),
        }
    }

    /// The URL to open in the user's browser.
    pub fn authorize_url(&self) -> &str {
        &self.authorize_url
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// The CSRF `state` parameter carried in the authorize URL.
    pub fn state(&self) -> &str {
        &self.state
    }

    /// Bind the redirect's loopback address, serve the browser's callback once, and
    /// exchange the code. Fails with `AuthRequired` if the user denied access.
    pub async fn wait_for_callback(self) -> Result<Credentials> {
        let redirect = Url::parse(&self.redirect_uri)
            .map_err(|e| Error::protocol("oauth", format!("redirect uri: {e}")))?;
        let addr: SocketAddr = format!(
            "{}:{}",
            redirect.host_str().unwrap_or("127.0.0.1"),
            redirect.port().unwrap_or(consts::OAUTH_REDIRECT_PORT)
        )
        .parse()
        .map_err(|e| Error::protocol("oauth", format!("redirect address: {e}")))?;
        let expected_path = redirect.path().to_string();

        let listener = TcpListener::bind(addr).await.map_err(|e| {
            std::io::Error::new(e.kind(), format!("cannot listen on {addr} for the OAuth redirect: {e}"))
        })?;
        tracing::info!(%addr, "waiting for the OAuth redirect");

        let code = loop {
            let (mut stream, _) = listener.accept().await?;
            let mut buf = vec![0u8; 8192];
            let n = tokio::time::timeout(CALLBACK_READ_TIMEOUT, stream.read(&mut buf))
                .await
                .map_err(|_| Error::Timeout("oauth callback request"))??;
            let request = String::from_utf8_lossy(&buf[..n]);

            match parse_callback(&request, &expected_path, &self.state) {
                Ok(Some(code)) => {
                    respond(&mut stream, 200, "Signed in. You can close this tab and return to the application.").await?;
                    break code;
                }
                Ok(None) => respond(&mut stream, 404, "Not the OAuth redirect.").await?,
                Err(e) => {
                    respond(&mut stream, 400, &format!("Sign-in failed: {e}")).await?;
                    return Err(e);
                }
            }
        };

        self.exchange_code(&code).await
    }

    /// Exchange an authorization code for an access token.
    pub async fn exchange_code(&self, code: &str) -> Result<Credentials> {
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("client_id", self.client_id.as_str()),
            ("code_verifier", self.verifier.as_str()),
        ];
        let response = self
            .http
            .post(self.token_url.as_str())
            .form(&form)
            .send()
            .await
            .map_err(|e| Error::transport(&self.token_url, e))?;

        let status = response.status();
        let body = response.text().await.map_err(|e| Error::transport(&self.token_url, e))?;
        if !status.is_success() {
            return Err(Error::AuthRequired(format!("token exchange failed with HTTP {status}: {body}")));
        }

        let token: TokenResponse = serde_json::from_str(&body)
            .map_err(|e| Error::protocol("oauth token response", format!("{e}: {body}")))?;
        Ok(Credentials::access_token(token.access_token))
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

/// Parse the request line of the browser's callback.
///
/// Returns `Ok(Some(code))` for the redirect, `Ok(None)` for any other path (browsers
/// also ask for `/favicon.ico`), and `Err(AuthRequired)` when the user denied access or
/// the state does not match.
pub(crate) fn parse_callback(request: &str, expected_path: &str, expected_state: &str) -> Result<Option<String>> {
    let line = request.lines().next().unwrap_or("");
    let target = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| Error::protocol("oauth callback", "malformed request line"))?;
    let url = Url::parse(&format!("http://localhost{target}"))
        .map_err(|_| Error::protocol("oauth callback", "unparseable request target"))?;
    if url.path() != expected_path {
        return Ok(None);
    }

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        match &*key {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = error {
        return Err(Error::AuthRequired(format!("authorization was denied: {error}")));
    }
    if state.as_deref() != Some(expected_state) {
        return Err(Error::AuthRequired("oauth state mismatch on the redirect".to_string()));
    }
    code.map(Some).ok_or_else(|| Error::AuthRequired("the redirect carried no code".to_string()))
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str) -> Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn challenge_matches_rfc_7636_appendix_b() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorize_url_carries_every_required_parameter() {
        let flow = pkce();
        let url = url::Url::parse(flow.authorize_url()).unwrap();
        assert_eq!(url.origin().ascii_serialization(), "https://accounts.spotify.com");
        assert_eq!(url.path(), "/authorize");
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], consts::CLIENT_ID_DESKTOP);
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["redirect_uri"], consts::OAUTH_REDIRECT_URI);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], challenge_for(&flow.verifier));
        assert_eq!(q["state"], flow.state());
        assert!(q["scope"].split(' ').any(|s| s == "streaming"));
        assert_eq!(flow.verifier.len(), 43);
    }

    #[test]
    fn parse_callback_extracts_the_code_and_checks_state() {
        let ok = "GET /login?code=abc123&state=st HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        assert_eq!(parse_callback(ok, "/login", "st").unwrap(), Some("abc123".to_string()));

        let other = "GET /favicon.ico HTTP/1.1\r\n\r\n";
        assert_eq!(parse_callback(other, "/login", "st").unwrap(), None);

        let denied = "GET /login?error=access_denied&state=st HTTP/1.1\r\n\r\n";
        assert!(matches!(parse_callback(denied, "/login", "st"), Err(Error::AuthRequired(_))));

        let wrong_state = "GET /login?code=abc&state=nope HTTP/1.1\r\n\r\n";
        assert!(matches!(parse_callback(wrong_state, "/login", "st"), Err(Error::AuthRequired(_))));

        let garbage = "\r\n";
        assert!(matches!(parse_callback(garbage, "/login", "st"), Err(Error::Protocol { .. })));
    }

    #[tokio::test]
    async fn exchange_code_posts_the_verifier_and_returns_token_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=the-code"))
            .and(body_string_contains("code_verifier="))
            .and(body_string_contains(format!("client_id={}", consts::CLIENT_ID_DESKTOP)))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-123", "token_type": "Bearer", "expires_in": 3600,
                "refresh_token": "rt", "scope": "streaming"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let flow = PkceFlow::with_endpoints(
            consts::CLIENT_ID_DESKTOP,
            consts::OAUTH_REDIRECT_URI,
            consts::OAUTH_AUTHORIZE_URL,
            &format!("{}/api/token", server.uri()),
            consts::OAUTH_SCOPES,
        );
        let creds = flow.exchange_code("the-code").await.unwrap();
        assert_eq!(creds, Credentials::access_token("at-123"));
    }

    #[tokio::test]
    async fn exchange_code_failure_is_auth_required() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant", "error_description": "Invalid authorization code"
            })))
            .mount(&server)
            .await;

        let flow = PkceFlow::with_endpoints(
            "cid",
            consts::OAUTH_REDIRECT_URI,
            consts::OAUTH_AUTHORIZE_URL,
            &format!("{}/api/token", server.uri()),
            &["streaming"],
        );
        let err = flow.exchange_code("bad").await.unwrap_err();
        assert!(matches!(err, Error::AuthRequired(ref msg) if msg.contains("invalid_grant")), "got {err:?}");
    }

    #[tokio::test]
    async fn wait_for_callback_serves_the_redirect_and_exchanges_the_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-456", "token_type": "Bearer", "expires_in": 3600
            })))
            .mount(&server)
            .await;

        let flow = PkceFlow::with_endpoints(
            "cid",
            "http://127.0.0.1:18998/login",
            consts::OAUTH_AUTHORIZE_URL,
            &format!("{}/api/token", server.uri()),
            &["streaming"],
        );
        let state = flow.state().to_string();
        let waiting = tokio::spawn(flow.wait_for_callback());

        // Give the listener a moment to bind, then act as the browser.
        let mut browser = None;
        for _ in 0..50 {
            if let Ok(s) = tokio::net::TcpStream::connect("127.0.0.1:18998").await {
                browser = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let mut browser = browser.expect("callback listener did not bind");
        browser
            .write_all(format!("GET /login?code=xyz&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        browser.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");

        let creds = waiting.await.unwrap().unwrap();
        assert_eq!(creds, Credentials::access_token("at-456"));
    }
}
