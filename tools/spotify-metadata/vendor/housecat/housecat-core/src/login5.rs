//! Bearer tokens from login5, exchanged for stored credentials (spec section 5.5).

use std::sync::Arc;
use std::time::{Duration, Instant};

use prost::Message;
use tokio::sync::Mutex;

use housecat_protocol::spotify::login5::v3 as l5;
use housecat_protocol::spotify::login5::v3::challenges as l5c;
use housecat_protocol::spotify::login5::v3::credentials as l5cred;

use crate::auth::credentials::{CredentialKind, Credentials};
use crate::client_token::ClientTokenProvider;
use crate::{consts, hashcash, Error, Result};

#[derive(Clone)]
struct BearerToken {
    token: String,
    expires_at: Instant,
}

pub struct Login5 {
    http: reqwest::Client,
    url: String,
    client_id: String,
    device_id: String,
    client_token: Arc<ClientTokenProvider>,
    credentials: Credentials,
    current: Mutex<Option<BearerToken>>,
}

impl std::fmt::Debug for Login5 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login5")
            .field("url", &self.url)
            .field("client_id", &self.client_id)
            .field("device_id", &self.device_id)
            .field("username", &self.credentials.username)
            .finish_non_exhaustive()
    }
}

impl Login5 {
    pub fn new(
        http: reqwest::Client,
        device_id: impl Into<String>,
        client_token: Arc<ClientTokenProvider>,
        credentials: Credentials,
    ) -> Result<Self> {
        if credentials.kind != CredentialKind::Stored || credentials.username.is_none() {
            return Err(Error::AuthRequired(
                "login5 needs stored credentials with a username; log in to the access point first".to_string(),
            ));
        }
        Ok(Login5 {
            http,
            url: consts::LOGIN5_URL.to_string(),
            client_id: consts::CLIENT_ID_DESKTOP.to_string(),
            device_id: device_id.into(),
            client_token,
            credentials,
            current: Mutex::new(None),
        })
    }

    /// Point at another endpoint (tests, proxies).
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// A valid bearer token, refreshed before it expires. Concurrent callers share one refresh.
    pub async fn bearer(&self) -> Result<String> {
        let mut guard = self.current.lock().await;
        if let Some(token) = guard.as_ref()
            && token.expires_at > Instant::now() + consts::TOKEN_EXPIRY_MARGIN
        {
            return Ok(token.token.clone());
        }
        let fresh = self.login().await?;
        let value = fresh.token.clone();
        *guard = Some(fresh);
        Ok(value)
    }

    async fn login(&self) -> Result<BearerToken> {
        let mut request = self.base_request();
        let mut response = self.post(&request).await?;

        for _ in 1..=consts::LOGIN5_MAX_ATTEMPTS {
            match response.response.take() {
                Some(l5::login_response::Response::Ok(ok)) => {
                    let expires_in = u64::try_from(ok.access_token_expires_in).unwrap_or(3600);
                    tracing::debug!(expires_in, "received a login5 bearer token");
                    return Ok(BearerToken {
                        token: ok.access_token,
                        expires_at: Instant::now() + Duration::from_secs(expires_in),
                    });
                }
                Some(l5::login_response::Response::Error(code)) => {
                    let error = l5::LoginError::try_from(code).unwrap_or(l5::LoginError::UnknownError);
                    match error {
                        l5::LoginError::Timeout | l5::LoginError::TryAgainLater | l5::LoginError::TooManyAttempts => {
                            tracing::warn!(?error, "login5 asked us to wait");
                            tokio::time::sleep(consts::LOGIN5_RETRY_DELAY).await;
                            response = self.post(&request).await?;
                        }
                        l5::LoginError::InvalidCredentials => {
                            return Err(Error::AuthRequired(
                                "login5 rejected the stored credentials (INVALID_CREDENTIALS); since 2026-08 only \
                                 credentials produced with the desktop client id are accepted, so log in again"
                                    .to_string(),
                            ));
                        }
                        other => return Err(Error::protocol("login5", format!("login error {other:?}"))),
                    }
                }
                Some(l5::login_response::Response::Challenges(challenges)) => {
                    tracing::debug!(count = challenges.challenges.len(), "solving login5 challenges");
                    solve_challenges(&mut request, &response.login_context, &challenges)?;
                    response = self.post(&request).await?;
                }
                None => return Err(Error::protocol("login5", "response carried neither ok, error, nor challenges")),
            }
        }
        Err(Error::protocol("login5", format!("no token after {} attempts", consts::LOGIN5_MAX_ATTEMPTS)))
    }

    fn base_request(&self) -> l5::LoginRequest {
        l5::LoginRequest {
            client_info: Some(l5::ClientInfo { client_id: self.client_id.clone(), device_id: self.device_id.clone() }),
            login_context: Vec::new(),
            challenge_solutions: None,
            login_method: Some(l5::login_request::LoginMethod::StoredCredential(l5cred::StoredCredential {
                username: self.credentials.username.clone().unwrap_or_default(),
                data: self.credentials.data.clone(),
            })),
        }
    }

    async fn post(&self, request: &l5::LoginRequest) -> Result<l5::LoginResponse> {
        let client_token = self.client_token.get().await?;
        let response = self
            .http
            .post(self.url.as_str())
            .header(reqwest::header::ACCEPT, "application/x-protobuf")
            .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
            .header("client-token", client_token)
            .body(request.encode_to_vec())
            .send()
            .await
            .map_err(|e| Error::transport(&self.url, e))?;

        let status = response.status();
        let body = response.bytes().await.map_err(|e| Error::transport(&self.url, e))?;
        if !status.is_success() {
            let snippet = String::from_utf8_lossy(&body[..body.len().min(consts::ERROR_BODY_LIMIT)]).into_owned();
            return Err(Error::protocol("login5", format!("HTTP {status}: {snippet}")));
        }
        l5::LoginResponse::decode(body).map_err(|e| Error::protocol("login5", format!("bad protobuf: {e}")))
    }
}

/// Solve every hashcash challenge and attach the solutions plus the echoed login context.
pub(crate) fn solve_challenges(
    request: &mut l5::LoginRequest,
    login_context: &[u8],
    challenges: &l5::Challenges,
) -> Result<()> {
    let mut solutions = Vec::with_capacity(challenges.challenges.len());
    for challenge in &challenges.challenges {
        match &challenge.challenge {
            Some(l5::challenge::Challenge::Hashcash(hc)) => {
                let (suffix, took) = hashcash::solve(login_context, &hc.prefix, hc.length, consts::HASHCASH_TIMEOUT)
                    .ok_or(Error::Timeout("login5 hashcash"))?;
                solutions.push(l5::ChallengeSolution {
                    solution: Some(l5::challenge_solution::Solution::Hashcash(l5c::HashcashSolution {
                        suffix: suffix.to_vec(),
                        duration: Some(prost_types::Duration {
                            seconds: took.as_secs() as i64,
                            nanos: took.subsec_nanos() as i32,
                        }),
                    })),
                });
            }
            Some(l5::challenge::Challenge::Code(_)) => {
                return Err(Error::AuthRequired(
                    "login5 asked for a code challenge, which housecat does not support".to_string(),
                ));
            }
            None => continue,
        }
    }
    request.challenge_solutions = Some(l5::ChallengeSolutions { solutions });
    request.login_context = login_context.to_vec();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wiremock::matchers::{header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ok_response(token: &str, expires_in: i32) -> Vec<u8> {
        l5::LoginResponse {
            warnings: vec![],
            login_context: vec![],
            identifier_token: String::new(),
            user_info: None,
            response: Some(l5::login_response::Response::Ok(l5::LoginOk {
                username: "alice".to_string(),
                access_token: token.to_string(),
                stored_credential: vec![],
                access_token_expires_in: expires_in,
            })),
        }
        .encode_to_vec()
    }

    fn error_response(code: l5::LoginError) -> Vec<u8> {
        l5::LoginResponse {
            warnings: vec![],
            login_context: vec![],
            identifier_token: String::new(),
            user_info: None,
            response: Some(l5::login_response::Response::Error(code as i32)),
        }
        .encode_to_vec()
    }

    fn challenge_response(prefix: &[u8], length: i32, login_context: &[u8]) -> Vec<u8> {
        l5::LoginResponse {
            warnings: vec![],
            login_context: login_context.to_vec(),
            identifier_token: String::new(),
            user_info: None,
            response: Some(l5::login_response::Response::Challenges(l5::Challenges {
                challenges: vec![l5::Challenge {
                    challenge: Some(l5::challenge::Challenge::Hashcash(l5c::HashcashChallenge {
                        prefix: prefix.to_vec(),
                        length,
                    })),
                }],
            })),
        }
        .encode_to_vec()
    }

    fn granted_client_token() -> Vec<u8> {
        use housecat_protocol::spotify::clienttoken::http::v0 as ct;
        ct::ClientTokenResponse {
            response_type: ct::ClientTokenResponseType::ResponseGrantedTokenResponse as i32,
            response: Some(ct::client_token_response::Response::GrantedToken(ct::GrantedTokenResponse {
                token: "ct-1".to_string(),
                expires_after_seconds: 1_209_600,
                refresh_after_seconds: 7200,
                domains: vec![],
            })),
        }
        .encode_to_vec()
    }

    async fn server_with_client_token() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/clienttoken"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(granted_client_token()))
            .mount(&server)
            .await;
        server
    }

    fn login5(server: &MockServer) -> Login5 {
        let http = reqwest::Client::new();
        let client_token = Arc::new(
            ClientTokenProvider::new(http.clone(), "device-1", None)
                .with_url(format!("{}/v1/clienttoken", server.uri())),
        );
        Login5::new(http, "device-1", client_token, Credentials::stored("alice", vec![1, 2, 3]))
            .unwrap()
            .with_url(format!("{}/v3/login", server.uri()))
    }

    #[test]
    fn requires_stored_credentials() {
        let http = reqwest::Client::new();
        let client_token = Arc::new(ClientTokenProvider::new(http.clone(), "d", None));
        let err = Login5::new(http, "d", client_token, Credentials::access_token("tok")).unwrap_err();
        assert!(matches!(err, Error::AuthRequired(_)));
    }

    #[tokio::test]
    async fn returns_the_bearer_and_caches_it() {
        let server = server_with_client_token().await;
        Mock::given(method("POST"))
            .and(path("/v3/login"))
            .and(header("accept", "application/x-protobuf"))
            .and(header("client-token", "ct-1"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(ok_response("bearer-1", 3600)))
            .expect(1)
            .mount(&server)
            .await;

        let login5 = login5(&server);
        assert_eq!(login5.bearer().await.unwrap(), "bearer-1");
        assert_eq!(login5.bearer().await.unwrap(), "bearer-1");

        let requests = server.received_requests().await.unwrap();
        let login = requests.iter().find(|r| r.url.path() == "/v3/login").unwrap();
        let request = l5::LoginRequest::decode(&login.body[..]).unwrap();
        let info = request.client_info.unwrap();
        assert_eq!(info.client_id, consts::CLIENT_ID_DESKTOP);
        assert_eq!(info.device_id, "device-1");
        let Some(l5::login_request::LoginMethod::StoredCredential(sc)) = request.login_method else {
            panic!("expected stored credential login");
        };
        assert_eq!(sc.username, "alice");
        assert_eq!(sc.data, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn expired_bearer_is_refreshed() {
        let server = server_with_client_token().await;
        Mock::given(method("POST"))
            .and(path("/v3/login"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(ok_response("short", 30)))
            .expect(2)
            .mount(&server)
            .await;

        let login5 = login5(&server);
        // 30 s lifetime is inside TOKEN_EXPIRY_MARGIN, so every call refreshes.
        assert_eq!(login5.bearer().await.unwrap(), "short");
        assert_eq!(login5.bearer().await.unwrap(), "short");
    }

    #[tokio::test]
    async fn hashcash_challenge_is_solved_with_the_login_context() {
        let server = server_with_client_token().await;
        Mock::given(method("POST"))
            .and(path("/v3/login"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(challenge_response(b"pfx", 8, b"ctx-7")))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v3/login"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(ok_response("bearer-2", 3600)))
            .with_priority(2)
            .mount(&server)
            .await;

        assert_eq!(login5(&server).bearer().await.unwrap(), "bearer-2");

        let requests = server.received_requests().await.unwrap();
        let logins: Vec<_> = requests.iter().filter(|r| r.url.path() == "/v3/login").collect();
        assert_eq!(logins.len(), 2);
        let second = l5::LoginRequest::decode(&logins[1].body[..]).unwrap();
        assert_eq!(second.login_context, b"ctx-7");
        let solutions = second.challenge_solutions.unwrap().solutions;
        let Some(l5::challenge_solution::Solution::Hashcash(hc)) = &solutions[0].solution else {
            panic!("expected a hashcash solution");
        };
        assert_eq!(hc.suffix.len(), 16);
        assert!(hc.duration.is_some());
        assert!(crate::hashcash::check(b"pfx", &hc.suffix, 8));
    }

    #[tokio::test]
    async fn invalid_credentials_is_auth_required() {
        let server = server_with_client_token().await;
        Mock::given(method("POST"))
            .and(path("/v3/login"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(error_response(l5::LoginError::InvalidCredentials)))
            .mount(&server)
            .await;

        let err = login5(&server).bearer().await.unwrap_err();
        assert!(matches!(err, Error::AuthRequired(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn client_token_header_is_always_present() {
        let server = server_with_client_token().await;
        Mock::given(method("POST"))
            .and(path("/v3/login"))
            .and(header_exists("client-token"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(ok_response("bearer-3", 3600)))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(login5(&server).bearer().await.unwrap(), "bearer-3");
    }
}
