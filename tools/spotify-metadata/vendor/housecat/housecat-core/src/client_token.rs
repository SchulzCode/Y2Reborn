//! Client tokens from clienttoken.spotify.com (spec section 5.5).
//!
//! Every spclient call and every login5 call carries a `client-token` header. The
//! service either grants a token or poses a hashcash challenge first. Tokens live
//! about two hours and are cached to disk.

use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use housecat_protocol::spotify::clienttoken::data::v0 as ctd;
use housecat_protocol::spotify::clienttoken::http::v0 as ct;

use crate::auth::cache::{CacheDir, CLIENT_TOKEN_FILE};
use crate::{consts, hashcash, Error, Result};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ClientToken {
    pub token: String,
    /// Unix seconds after which a new token must be fetched.
    pub refresh_at: u64,
}

impl ClientToken {
    fn is_fresh(&self, now: u64) -> bool {
        now + consts::TOKEN_EXPIRY_MARGIN.as_secs() < self.refresh_at
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub struct ClientTokenProvider {
    http: reqwest::Client,
    url: String,
    client_id: String,
    device_id: String,
    cache: Option<CacheDir>,
    current: Mutex<Option<ClientToken>>,
}

impl ClientTokenProvider {
    pub fn new(http: reqwest::Client, device_id: impl Into<String>, cache: Option<CacheDir>) -> Self {
        ClientTokenProvider {
            http,
            url: consts::CLIENT_TOKEN_URL.to_string(),
            client_id: consts::CLIENT_ID_DESKTOP.to_string(),
            device_id: device_id.into(),
            cache,
            current: Mutex::new(None),
        }
    }

    /// Point the provider at another endpoint (tests, proxies).
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// A fresh client token. Concurrent callers share one fetch.
    pub async fn get(&self) -> Result<String> {
        let mut guard = self.current.lock().await;
        let now = unix_now();

        if let Some(token) = guard.as_ref()
            && token.is_fresh(now)
        {
            return Ok(token.token.clone());
        }
        if guard.is_none()
            && let Some(token) = self.load_cached()
            && token.is_fresh(now)
        {
            let value = token.token.clone();
            *guard = Some(token);
            return Ok(value);
        }

        let fresh = self.fetch().await?;
        self.store_cached(&fresh);
        let value = fresh.token.clone();
        *guard = Some(fresh);
        Ok(value)
    }

    fn load_cached(&self) -> Option<ClientToken> {
        let bytes = self.cache.as_ref()?.read(CLIENT_TOKEN_FILE).ok().flatten()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn store_cached(&self, token: &ClientToken) {
        if let Some(cache) = &self.cache {
            let bytes = serde_json::to_vec(token).expect("client token serializes");
            if let Err(error) = cache.write(CLIENT_TOKEN_FILE, &bytes) {
                tracing::warn!(%error, "could not cache the client token");
            }
        }
    }

    async fn fetch(&self) -> Result<ClientToken> {
        let request = client_data_request(&self.client_id, &self.device_id);
        let mut response = self.post(&request).await?;

        for attempt in 1..=consts::HASHCASH_MAX_ATTEMPTS {
            match response.response {
                Some(ct::client_token_response::Response::GrantedToken(granted)) => {
                    let refresh_after = u64::try_from(granted.refresh_after_seconds).unwrap_or(7200);
                    tracing::debug!("received a client token");
                    return Ok(ClientToken { token: granted.token, refresh_at: unix_now() + refresh_after });
                }
                Some(ct::client_token_response::Response::Challenges(challenges)) => {
                    tracing::debug!(attempt, "solving a client-token hashcash challenge");
                    let answer = answer_challenges(&challenges)?;
                    response = self.post(&answer).await?;
                }
                None => {
                    return Err(Error::protocol("client token", "response carried neither a token nor challenges"));
                }
            }
        }
        Err(Error::protocol(
            "client token",
            format!("no token after {} challenge rounds", consts::HASHCASH_MAX_ATTEMPTS),
        ))
    }

    async fn post(&self, request: &ct::ClientTokenRequest) -> Result<ct::ClientTokenResponse> {
        let response = self
            .http
            .post(self.url.as_str())
            .header(reqwest::header::ACCEPT, "application/x-protobuf")
            .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
            .body(request.encode_to_vec())
            .send()
            .await
            .map_err(|e| Error::transport(&self.url, e))?;

        let status = response.status();
        let body = response.bytes().await.map_err(|e| Error::transport(&self.url, e))?;
        if !status.is_success() {
            let snippet = String::from_utf8_lossy(&body[..body.len().min(consts::ERROR_BODY_LIMIT)]).into_owned();
            return Err(Error::protocol("client token", format!("HTTP {status}: {snippet}")));
        }
        ct::ClientTokenResponse::decode(body).map_err(|e| Error::protocol("client token", format!("bad protobuf: {e}")))
    }
}

pub(crate) fn client_data_request(client_id: &str, device_id: &str) -> ct::ClientTokenRequest {
    ct::ClientTokenRequest {
        request_type: ct::ClientTokenRequestType::RequestClientDataRequest as i32,
        request: Some(ct::client_token_request::Request::ClientData(ct::ClientDataRequest {
            client_version: consts::SPOTIFY_SEMANTIC_VERSION.to_string(),
            client_id: client_id.to_string(),
            data: Some(ct::client_data_request::Data::ConnectivitySdkData(ctd::ConnectivitySdkData {
                platform_specific_data: Some(ctd::PlatformSpecificData { data: Some(consts::platform_specific_data()) }),
                device_id: device_id.to_string(),
            })),
        })),
    }
}

/// Solve every hashcash challenge in the response and build the answer request.
pub(crate) fn answer_challenges(challenges: &ct::ChallengesResponse) -> Result<ct::ClientTokenRequest> {
    let mut answers = Vec::with_capacity(challenges.challenges.len());
    for challenge in &challenges.challenges {
        match &challenge.parameters {
            Some(ct::challenge::Parameters::EvaluateHashcashParameters(params)) => {
                let prefix = hex::decode(&params.prefix)
                    .map_err(|e| Error::protocol("client token", format!("hashcash prefix is not hex: {e}")))?;
                let (suffix, _) = hashcash::solve(&[], &prefix, params.length, consts::HASHCASH_TIMEOUT)
                    .ok_or(Error::Timeout("client token hashcash"))?;
                answers.push(ct::ChallengeAnswer {
                    challenge_type: ct::ChallengeType::ChallengeHashCash as i32,
                    answer: Some(ct::challenge_answer::Answer::HashCash(ct::HashCashAnswer {
                        suffix: hex::encode_upper(suffix),
                    })),
                });
            }
            other => {
                return Err(Error::protocol(
                    "client token",
                    format!("unsupported challenge type {}: {other:?}", challenge.r#type),
                ));
            }
        }
    }
    Ok(ct::ClientTokenRequest {
        request_type: ct::ClientTokenRequestType::RequestChallengeAnswersRequest as i32,
        request: Some(ct::client_token_request::Request::ChallengeAnswers(ct::ChallengeAnswersRequest {
            state: challenges.state.clone(),
            answers,
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn granted(token: &str) -> Vec<u8> {
        ct::ClientTokenResponse {
            response_type: ct::ClientTokenResponseType::ResponseGrantedTokenResponse as i32,
            response: Some(ct::client_token_response::Response::GrantedToken(ct::GrantedTokenResponse {
                token: token.to_string(),
                expires_after_seconds: 1_209_600,
                refresh_after_seconds: 7200,
                domains: vec![],
            })),
        }
        .encode_to_vec()
    }

    fn challenge(prefix_hex: &str, length: i32) -> Vec<u8> {
        ct::ClientTokenResponse {
            response_type: ct::ClientTokenResponseType::ResponseChallengesResponse as i32,
            response: Some(ct::client_token_response::Response::Challenges(ct::ChallengesResponse {
                state: "state-1".to_string(),
                challenges: vec![ct::Challenge {
                    r#type: ct::ChallengeType::ChallengeHashCash as i32,
                    parameters: Some(ct::challenge::Parameters::EvaluateHashcashParameters(ct::HashCashParameters {
                        length,
                        prefix: prefix_hex.to_string(),
                    })),
                }],
            })),
        }
        .encode_to_vec()
    }

    fn provider(server: &MockServer, cache: Option<CacheDir>) -> ClientTokenProvider {
        ClientTokenProvider::new(reqwest::Client::new(), "device-1", cache)
            .with_url(format!("{}/v1/clienttoken", server.uri()))
    }

    #[test]
    fn request_carries_version_client_id_and_device() {
        let request = client_data_request("cid", "dev");
        assert_eq!(request.request_type, ct::ClientTokenRequestType::RequestClientDataRequest as i32);
        let Some(ct::client_token_request::Request::ClientData(data)) = request.request else {
            panic!("expected client data");
        };
        assert_eq!(data.client_version, consts::SPOTIFY_SEMANTIC_VERSION);
        assert_eq!(data.client_id, "cid");
        let Some(ct::client_data_request::Data::ConnectivitySdkData(sdk)) = data.data else {
            panic!("expected connectivity data");
        };
        assert_eq!(sdk.device_id, "dev");
        assert!(sdk.platform_specific_data.unwrap().data.is_some());
    }

    #[tokio::test]
    async fn granted_token_is_returned_and_then_cached_in_memory() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/clienttoken"))
            .and(header("accept", "application/x-protobuf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(granted("tok-1")))
            .expect(1)
            .mount(&server)
            .await;

        let provider = provider(&server, None);
        assert_eq!(provider.get().await.unwrap(), "tok-1");
        assert_eq!(provider.get().await.unwrap(), "tok-1");
    }

    #[tokio::test]
    async fn hashcash_challenge_is_solved_and_answered() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/clienttoken"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(challenge("a1b2c3d4", 8)))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/clienttoken"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(granted("tok-2")))
            .with_priority(2)
            .mount(&server)
            .await;

        assert_eq!(provider(&server, None).get().await.unwrap(), "tok-2");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let answer = ct::ClientTokenRequest::decode(&requests[1].body[..]).unwrap();
        assert_eq!(answer.request_type, ct::ClientTokenRequestType::RequestChallengeAnswersRequest as i32);
        let Some(ct::client_token_request::Request::ChallengeAnswers(answers)) = answer.request else {
            panic!("expected challenge answers");
        };
        assert_eq!(answers.state, "state-1");
        let Some(ct::challenge_answer::Answer::HashCash(hc)) = &answers.answers[0].answer else {
            panic!("expected a hashcash answer");
        };
        assert_eq!(hc.suffix, hc.suffix.to_uppercase(), "suffix must be uppercase hex");
        let suffix = hex::decode(&hc.suffix).unwrap();
        assert!(crate::hashcash::check(&hex::decode("a1b2c3d4").unwrap(), &suffix, 8));
    }

    #[tokio::test]
    async fn token_is_persisted_to_and_read_from_the_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(dir.path());
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(granted("tok-3")))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(provider(&server, Some(cache.clone())).get().await.unwrap(), "tok-3");
        assert!(dir.path().join(CLIENT_TOKEN_FILE).exists());

        // A fresh provider over the same cache directory must not hit the server again.
        assert_eq!(provider(&server, Some(cache)).get().await.unwrap(), "tok-3");
    }

    #[tokio::test]
    async fn stale_cached_token_is_refetched() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CacheDir::new(dir.path());
        let stale = ClientToken { token: "old".to_string(), refresh_at: 0 };
        cache.write(CLIENT_TOKEN_FILE, &serde_json::to_vec(&stale).unwrap()).unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(granted("new")))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(provider(&server, Some(cache)).get().await.unwrap(), "new");
    }

    #[tokio::test]
    async fn http_error_is_a_protocol_error_with_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request body"))
            .mount(&server)
            .await;

        let err = provider(&server, None).get().await.unwrap_err();
        assert!(matches!(&err, Error::Protocol { what: "client token", detail } if detail.contains("400") && detail.contains("bad request body")), "got {err:?}");
    }
}
