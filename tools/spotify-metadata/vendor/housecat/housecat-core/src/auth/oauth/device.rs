//! OAuth device authorization flow against the desktop client ID (spec section 5.4).
//!
//! No loopback listener: the app shows a short code, the user enters it at
//! spotify.com/pair, and the flow polls the token endpoint until Spotify hands over an
//! access token, which becomes `Credentials::access_token`.

use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::auth::credentials::Credentials;
use crate::{consts, http, Error, Result};

const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

pub struct DeviceFlow {
    client_id: String,
    token_url: String,
    http: reqwest::Client,
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    interval: Duration,
    expires_at: Instant,
}

impl std::fmt::Debug for DeviceFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceFlow")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default = "default_expires_in")]
    expires_in: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_expires_in() -> u64 {
    600
}

fn default_interval() -> u64 {
    5
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct TokenError {
    error: String,
    #[serde(default)]
    error_description: String,
}

/// Start a device flow for Spotify's desktop client ID with the scopes from `consts`.
pub async fn device() -> Result<DeviceFlow> {
    DeviceFlow::start_with_endpoints(
        consts::CLIENT_ID_DESKTOP,
        consts::OAUTH_DEVICE_AUTHORIZE_URL,
        consts::OAUTH_TOKEN_URL,
        consts::OAUTH_SCOPES,
    )
    .await
}

impl DeviceFlow {
    pub(crate) async fn start_with_endpoints(
        client_id: &str,
        device_authorize_url: &str,
        token_url: &str,
        scopes: &[&str],
    ) -> Result<Self> {
        let http = http::client();
        let scope = scopes.join(" ");
        let form = [("client_id", client_id), ("scope", scope.as_str())];
        let response = http
            .post(device_authorize_url)
            .form(&form)
            .send()
            .await
            .map_err(|e| Error::transport(device_authorize_url, e))?;

        let status = response.status();
        let body = response.text().await.map_err(|e| Error::transport(device_authorize_url, e))?;
        if !status.is_success() {
            return Err(Error::AuthRequired(format!("device authorization failed with HTTP {status}: {body}")));
        }
        let auth: DeviceAuthorization = serde_json::from_str(&body)
            .map_err(|e| Error::protocol("device authorization response", format!("{e}: {body}")))?;

        Ok(DeviceFlow {
            client_id: client_id.to_string(),
            token_url: token_url.to_string(),
            http,
            device_code: auth.device_code,
            user_code: auth.user_code,
            verification_uri: auth.verification_uri,
            verification_uri_complete: auth.verification_uri_complete,
            interval: Duration::from_secs(auth.interval),
            expires_at: Instant::now() + Duration::from_secs(auth.expires_in),
        })
    }

    /// The short code the user types in at `verification_uri`.
    pub fn user_code(&self) -> &str {
        &self.user_code
    }

    pub fn verification_uri(&self) -> &str {
        &self.verification_uri
    }

    /// A URL that carries the code already, when Spotify provides one.
    pub fn verification_uri_complete(&self) -> Option<&str> {
        self.verification_uri_complete.as_deref()
    }

    /// Poll until the user approves, denies, or the code expires.
    pub async fn wait(self) -> Result<Credentials> {
        let mut interval = self.interval;
        loop {
            if Instant::now() >= self.expires_at {
                return Err(Error::AuthRequired("the device code expired before it was approved".to_string()));
            }

            let form = [
                ("client_id", self.client_id.as_str()),
                ("grant_type", DEVICE_CODE_GRANT),
                ("device_code", self.device_code.as_str()),
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

            if status.is_success() {
                let token: TokenResponse = serde_json::from_str(&body)
                    .map_err(|e| Error::protocol("device token response", format!("{e}: {body}")))?;
                return Ok(Credentials::access_token(token.access_token));
            }

            let error: TokenError = serde_json::from_str(&body).unwrap_or(TokenError {
                error: format!("http {status}"),
                error_description: body.clone(),
            });
            match error.error.as_str() {
                "authorization_pending" => {}
                "slow_down" => interval += SLOW_DOWN_STEP,
                other => {
                    return Err(Error::AuthRequired(format!(
                        "device authorization ended with {other}: {}",
                        error.error_description
                    )));
                }
            }
            tokio::time::sleep(interval).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn start(server: &MockServer, interval: u64, expires_in: u64) -> DeviceFlow {
        Mock::given(method("POST"))
            .and(path("/oauth2/device/authorize"))
            .and(body_string_contains("client_id=cid"))
            .and(body_string_contains("scope=streaming"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-code", "user_code": "ABCDEF",
                "verification_uri": "https://spotify.com/pair",
                "verification_uri_complete": "https://spotify.com/pair?code=ABCDEF",
                "expires_in": expires_in, "interval": interval
            })))
            .mount(server)
            .await;
        DeviceFlow::start_with_endpoints(
            "cid",
            &format!("{}/oauth2/device/authorize", server.uri()),
            &format!("{}/api/token", server.uri()),
            &["streaming"],
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn start_exposes_the_user_code_and_uris() {
        let server = MockServer::start().await;
        let flow = start(&server, 5, 600).await;
        assert_eq!(flow.user_code(), "ABCDEF");
        assert_eq!(flow.verification_uri(), "https://spotify.com/pair");
        assert_eq!(flow.verification_uri_complete(), Some("https://spotify.com/pair?code=ABCDEF"));
    }

    #[tokio::test]
    async fn start_failure_is_auth_required() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({ "error": "invalid_scope" })))
            .mount(&server)
            .await;
        let err = DeviceFlow::start_with_endpoints(
            "cid",
            &format!("{}/oauth2/device/authorize", server.uri()),
            &format!("{}/api/token", server.uri()),
            &["bogus"],
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::AuthRequired(ref msg) if msg.contains("invalid_scope")), "got {err:?}");
    }

    #[tokio::test]
    async fn wait_polls_until_approved() {
        let server = MockServer::start().await;
        let flow = start(&server, 0, 600).await;
        Mock::given(method("POST"))
            .and(path("/api/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({ "error": "authorization_pending" })))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/token"))
            .and(body_string_contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"))
            .and(body_string_contains("device_code=dev-code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-789", "token_type": "Bearer", "expires_in": 3600, "refresh_token": "rt"
            })))
            .with_priority(2)
            .mount(&server)
            .await;

        let creds = flow.wait().await.unwrap();
        assert_eq!(creds, Credentials::access_token("at-789"));
        let polls = server.received_requests().await.unwrap().iter().filter(|r| r.url.path() == "/api/token").count();
        assert_eq!(polls, 3);
    }

    #[tokio::test]
    async fn denial_is_auth_required() {
        let server = MockServer::start().await;
        let flow = start(&server, 0, 600).await;
        Mock::given(method("POST"))
            .and(path("/api/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "access_denied", "error_description": "User denied"
            })))
            .mount(&server)
            .await;
        let err = flow.wait().await.unwrap_err();
        assert!(matches!(err, Error::AuthRequired(ref msg) if msg.contains("access_denied")), "got {err:?}");
    }

    #[tokio::test]
    async fn expiry_stops_the_polling() {
        let server = MockServer::start().await;
        let flow = start(&server, 0, 0).await;
        let err = flow.wait().await.unwrap_err();
        assert!(matches!(err, Error::AuthRequired(ref msg) if msg.contains("expired")), "got {err:?}");
    }
}
