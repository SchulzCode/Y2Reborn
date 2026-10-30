//! Authenticated HTTPS calls to spclient (spec section 5.6).
//!
//! One policy covers every call: a per-host rate limiter, up to `SPCLIENT_RETRY_ATTEMPTS`
//! attempts with jittered backoff on transport errors and 5xx, Retry-After honoured on a
//! 429 up to `RATE_LIMIT_MAX_WAIT`, and host rotation with re-resolution after
//! `UNAVAILABLE_BEFORE_REROUTE` consecutive failures. JSON lookups (`get_json`) treat a
//! 404 as "nothing there" instead of as an endpoint yank.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use reqwest::{Method, StatusCode};

use crate::apresolve;
use crate::client_token::ClientTokenProvider;
use crate::login5::Login5;
use crate::ratelimit::RateLimiter;
use crate::{consts, random, Error, Result};

pub struct SpClient {
    http: reqwest::Client,
    /// Base URLs such as `https://gew4-spclient.spotify.com:443`; the front one is current.
    hosts: Mutex<VecDeque<String>>,
    scheme: String,
    apresolve_url: String,
    country: String,
    login5: Arc<Login5>,
    client_token: Arc<ClientTokenProvider>,
    limiter: RateLimiter,
    unavailable: AtomicU32,
}

impl std::fmt::Debug for SpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpClient")
            .field("base_url", &self.base_url())
            .field("country", &self.country)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    /// spclient, protobuf, with the metrics query.
    SpClient,
    /// spclient with `Accept: application/json`; a 404 means "no such resource", not a yank.
    SpClientJson,
    /// An absolute URL on another Spotify service (pathfinder), JSON, no metrics query.
    Absolute,
}

enum Attempt {
    Done(Bytes),
    Missing,
    Retry { error: Error, retry_after: Option<Duration> },
    Fatal(Error),
}

#[derive(Clone)]
enum Payload {
    Protobuf(Vec<u8>),
    Json(String),
}

impl Payload {
    fn content_type(&self) -> &'static str {
        match self {
            Payload::Protobuf(_) => "application/x-protobuf",
            Payload::Json(_) => "application/json",
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        match self {
            Payload::Protobuf(bytes) => bytes,
            Payload::Json(text) => text.into_bytes(),
        }
    }
}

impl SpClient {
    /// `base_urls` are spclient bases in preference order, `country` comes from the session.
    pub fn new(
        http: reqwest::Client,
        base_urls: Vec<String>,
        country: impl Into<String>,
        login5: Arc<Login5>,
        client_token: Arc<ClientTokenProvider>,
    ) -> Self {
        SpClient {
            http,
            hosts: Mutex::new(base_urls.into_iter().collect()),
            scheme: "https".to_string(),
            apresolve_url: consts::APRESOLVE_URL.to_string(),
            country: country.into(),
            login5,
            client_token,
            limiter: RateLimiter::new(consts::RATE_LIMIT_CALLS, consts::RATE_LIMIT_WINDOW),
            unavailable: AtomicU32::new(0),
        }
    }

    /// Where to re-resolve hosts from (tests point this at a mock).
    pub fn with_apresolve_url(mut self, url: impl Into<String>) -> Self {
        self.apresolve_url = url.into();
        self
    }

    /// Scheme for re-resolved hosts, `https` unless a test says otherwise.
    pub fn with_scheme(mut self, scheme: impl Into<String>) -> Self {
        self.scheme = scheme.into();
        self
    }

    /// The current spclient base URL, or empty when the host list is exhausted.
    pub fn base_url(&self) -> String {
        self.hosts.lock().expect("hosts lock").front().cloned().unwrap_or_default()
    }

    pub async fn get(&self, path: &str) -> Result<Bytes> {
        self.request(Target::SpClient, Method::GET, path, None).await
    }

    pub async fn post_protobuf(&self, path: &str, body: Vec<u8>) -> Result<Bytes> {
        self.request(Target::SpClient, Method::POST, path, Some(Payload::Protobuf(body))).await
    }

    /// GET a JSON endpoint on spclient. `Ok(None)` when the server answers 404, which for
    /// endpoints such as lyrics means the entity has none.
    pub async fn get_json(&self, path: &str) -> Result<Option<Bytes>> {
        self.request_inner(Target::SpClientJson, Method::GET, path, None).await
    }

    /// POST JSON to spclient, with the metrics query, and read JSON back.
    pub async fn post_json(&self, path: &str, body: String) -> Result<Bytes> {
        self.request(Target::SpClientJson, Method::POST, path, Some(Payload::Json(body))).await
    }

    /// POST JSON to an absolute URL on another Spotify service with the same bearer and
    /// client token. No metrics query, no host rotation; 4xx answers are protocol errors
    /// carrying the body.
    pub async fn post_json_to(&self, url: &str, body: String) -> Result<Bytes> {
        self.request(Target::Absolute, Method::POST, url, Some(Payload::Json(body))).await
    }

    async fn request(&self, target: Target, method: Method, path: &str, payload: Option<Payload>) -> Result<Bytes> {
        self.request_inner(target, method, path, payload)
            .await?
            .ok_or_else(|| Error::protocol("spclient", format!("unexpected empty result for {path}")))
    }

    async fn request_inner(
        &self,
        target: Target,
        method: Method,
        path: &str,
        payload: Option<Payload>,
    ) -> Result<Option<Bytes>> {
        let mut last_error = None;
        for attempt in 1..=consts::SPCLIENT_RETRY_ATTEMPTS {
            self.limiter.acquire().await;
            let url = match target {
                Target::Absolute => path.to_string(),
                Target::SpClient | Target::SpClientJson => self.spclient_url(path).await,
            };

            match self.send_once(target, method.clone(), &url, path, payload.clone()).await {
                Attempt::Done(bytes) => {
                    self.unavailable.store(0, Ordering::Relaxed);
                    return Ok(Some(bytes));
                }
                Attempt::Missing => {
                    self.unavailable.store(0, Ordering::Relaxed);
                    return Ok(None);
                }
                Attempt::Fatal(error) => return Err(error),
                Attempt::Retry { error, retry_after } => {
                    if target != Target::Absolute {
                        self.note_unavailable().await;
                    }
                    let delay = retry_after.unwrap_or_else(|| backoff(attempt));
                    tracing::warn!(%error, attempt, ?delay, "request failed; retrying");
                    last_error = Some(error);
                    if attempt < consts::SPCLIENT_RETRY_ATTEMPTS {
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| Error::protocol("spclient", "no request attempts were made")))
    }

    async fn send_once(&self, target: Target, method: Method, url: &str, path: &str, payload: Option<Payload>) -> Attempt {
        let bearer = match self.login5.bearer().await {
            Ok(bearer) => bearer,
            Err(error) => return Attempt::Fatal(error),
        };

        let mut request = self.http.request(method, url).header(AUTHORIZATION, format!("Bearer {bearer}"));
        match self.client_token.get().await {
            Ok(token) => request = request.header("client-token", token),
            Err(error) => tracing::warn!(%error, "no client token; sending the request without one"),
        }
        if target != Target::SpClient {
            request = request.header(ACCEPT, "application/json");
        }
        if let Some(payload) = payload {
            request = request.header(CONTENT_TYPE, payload.content_type()).body(payload.into_bytes());
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => return Attempt::Retry { error: Error::transport(url, error), retry_after: None },
        };
        let status = response.status();
        let retry_after = parse_retry_after(response.headers().get(RETRY_AFTER));
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => return Attempt::Retry { error: Error::transport(url, error), retry_after: None },
        };
        if status.is_success() {
            return Attempt::Done(bytes);
        }

        let snippet = String::from_utf8_lossy(&bytes[..bytes.len().min(consts::ERROR_BODY_LIMIT)]).into_owned();
        let detail = format!("HTTP {status} from {path}: {snippet}");
        match status {
            StatusCode::TOO_MANY_REQUESTS => {
                let wait = retry_after.unwrap_or(consts::SPCLIENT_RETRY_MAX_DELAY);
                tracing::warn!(path, ?wait, body = %snippet, "rate limited by Spotify");
                if wait > consts::RATE_LIMIT_MAX_WAIT {
                    Attempt::Fatal(Error::RateLimited { retry_after: wait })
                } else {
                    Attempt::Retry { error: Error::RateLimited { retry_after: wait }, retry_after: Some(wait) }
                }
            }
            StatusCode::UNAUTHORIZED => Attempt::Fatal(Error::AuthRequired(detail)),
            StatusCode::NOT_FOUND if target == Target::SpClientJson => Attempt::Missing,
            StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND | StatusCode::GONE => {
                tracing::error!(%status, path, body = %snippet, "known endpoint answered like a yank");
                Attempt::Fatal(Error::protocol("spclient", detail))
            }
            status if status.is_server_error() => Attempt::Retry { error: Error::protocol("spclient", detail), retry_after: None },
            _ => Attempt::Fatal(Error::protocol("spclient", detail)),
        }
    }

    /// `base + path` plus the metrics and cache-busting query Spotify's client adds.
    async fn spclient_url(&self, path: &str) -> String {
        let base = self.current_base().await;
        let separator = if path.contains('?') { '&' } else { '?' };
        format!("{base}{path}{separator}product=0&country={}&salt={}", self.country, random::random_u32())
    }

    async fn current_base(&self) -> String {
        if let Some(front) = self.hosts.lock().expect("hosts lock").front().cloned() {
            return front;
        }
        self.re_resolve().await;
        self.hosts
            .lock()
            .expect("hosts lock")
            .front()
            .cloned()
            .unwrap_or_else(|| format!("{}://{}", self.scheme, consts::SPCLIENT_FALLBACK))
    }

    async fn note_unavailable(&self) {
        let failures = self.unavailable.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= consts::UNAVAILABLE_BEFORE_REROUTE {
            self.unavailable.store(0, Ordering::Relaxed);
            self.rotate_host().await;
        }
    }

    async fn rotate_host(&self) {
        let (dropped, empty) = {
            let mut hosts = self.hosts.lock().expect("hosts lock");
            let dropped = hosts.pop_front();
            (dropped, hosts.is_empty())
        };
        tracing::warn!(?dropped, "rotating spclient host after repeated failures");
        if empty {
            self.re_resolve().await;
        }
    }

    async fn re_resolve(&self) {
        let resolved = apresolve::resolve(&self.http, &self.apresolve_url).await;
        let mut hosts = self.hosts.lock().expect("hosts lock");
        hosts.clear();
        hosts.extend(resolved.spclients.into_iter().map(|host| format!("{}://{host}", self.scheme)));
        tracing::info!(count = hosts.len(), "spclient hosts re-resolved");
    }
}

/// 200 ms, 400 ms, 800 ms, ... capped at `SPCLIENT_RETRY_MAX_DELAY`, plus up to 100 ms of jitter.
fn backoff(attempt: u32) -> Duration {
    let factor = 1u32 << attempt.saturating_sub(1).min(8);
    let exponential = consts::SPCLIENT_RETRY_BASE_DELAY.saturating_mul(factor).min(consts::SPCLIENT_RETRY_MAX_DELAY);
    exponential + Duration::from_millis(u64::from(random::random_u32() % 100))
}

/// Only the delay-seconds form of Retry-After; HTTP dates are treated as absent.
fn parse_retry_after(value: Option<&HeaderValue>) -> Option<Duration> {
    value?.to_str().ok()?.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::Instant as StdInstant;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Client-token and login5 doubles, shared with other modules' tests.
    pub(crate) async fn mount_auth(server: &MockServer) {
        use housecat_protocol::spotify::clienttoken::http::v0 as ct;
        use housecat_protocol::spotify::login5::v3 as l5;
        use prost::Message;

        let ct_body = ct::ClientTokenResponse {
            response_type: ct::ClientTokenResponseType::ResponseGrantedTokenResponse as i32,
            response: Some(ct::client_token_response::Response::GrantedToken(ct::GrantedTokenResponse {
                token: "ct-1".to_string(),
                expires_after_seconds: 1_209_600,
                refresh_after_seconds: 7200,
                domains: vec![],
            })),
        }
        .encode_to_vec();
        Mock::given(method("POST"))
            .and(path("/v1/clienttoken"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(ct_body))
            .mount(server)
            .await;

        let l5_body = l5::LoginResponse {
            warnings: vec![],
            login_context: vec![],
            identifier_token: String::new(),
            user_info: None,
            response: Some(l5::login_response::Response::Ok(l5::LoginOk {
                username: "alice".to_string(),
                access_token: "bearer-1".to_string(),
                stored_credential: vec![],
                access_token_expires_in: 3600,
            })),
        }
        .encode_to_vec();
        Mock::given(method("POST"))
            .and(path("/v3/login"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(l5_body))
            .mount(server)
            .await;
    }

    /// An apresolve double on `auth` that names `spclient` as the only spclient host.
    async fn mount_apresolve(auth: &MockServer, spclient: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/apresolve"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accesspoint": [],
                "spclient": [spclient.address().to_string()]
            })))
            .mount(auth)
            .await;
    }

    /// A client whose auth endpoints live on `auth` and whose spclient hosts are `hosts`.
    pub(crate) fn client(auth: &MockServer, hosts: Vec<String>) -> SpClient {
        use crate::auth::credentials::Credentials;

        let http = reqwest::Client::new();
        let client_token = Arc::new(
            ClientTokenProvider::new(http.clone(), "device-1", None)
                .with_url(format!("{}/v1/clienttoken", auth.uri())),
        );
        let login5 = Arc::new(
            Login5::new(http.clone(), "device-1", client_token.clone(), Credentials::stored("alice", vec![1]))
                .unwrap()
                .with_url(format!("{}/v3/login", auth.uri())),
        );
        SpClient::new(http, hosts, "SE", login5, client_token)
            .with_apresolve_url(format!("{}/apresolve", auth.uri()))
            .with_scheme("http")
    }

    async fn count(server: &MockServer, endpoint: &str) -> usize {
        server.received_requests().await.unwrap().iter().filter(|r| r.url.path() == endpoint).count()
    }

    #[tokio::test]
    async fn get_sends_auth_headers_and_metrics_query() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        Mock::given(method("GET"))
            .and(path("/some/endpoint"))
            .and(header("authorization", "Bearer bearer-1"))
            .and(header("client-token", "ct-1"))
            .and(query_param("product", "0"))
            .and(query_param("country", "SE"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"payload".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let body = client(&server, vec![server.uri()]).get("/some/endpoint").await.unwrap();
        assert_eq!(&body[..], b"payload");

        let requests = server.received_requests().await.unwrap();
        let get = requests.iter().find(|r| r.url.path() == "/some/endpoint").unwrap();
        assert!(get.url.query_pairs().any(|(k, _)| k == "salt"), "salt query param missing");
    }

    #[tokio::test]
    async fn post_protobuf_sets_the_content_type_and_keeps_existing_query() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        Mock::given(method("POST"))
            .and(path("/extended-metadata/v0/extended-metadata"))
            .and(header("content-type", "application/x-protobuf"))
            .and(query_param("x", "1"))
            .and(query_param("product", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1, 2, 3]))
            .expect(1)
            .mount(&server)
            .await;

        let body = client(&server, vec![server.uri()])
            .post_protobuf("/extended-metadata/v0/extended-metadata?x=1", vec![9, 9])
            .await
            .unwrap();
        assert_eq!(&body[..], &[1, 2, 3]);
        let requests = server.received_requests().await.unwrap();
        let post = requests.iter().find(|r| r.url.path().starts_with("/extended-metadata")).unwrap();
        assert_eq!(post.body, vec![9, 9]);
    }

    #[tokio::test]
    async fn gone_is_a_protocol_error_without_retry() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        Mock::given(method("GET"))
            .and(path("/gone"))
            .respond_with(ResponseTemplate::new(410).set_body_string("endpoint retired"))
            .mount(&server)
            .await;

        let err = client(&server, vec![server.uri()]).get("/gone").await.unwrap_err();
        assert!(matches!(&err, Error::Protocol { what: "spclient", detail } if detail.contains("410") && detail.contains("endpoint retired")), "got {err:?}");
        assert_eq!(count(&server, "/gone").await, 1);
    }

    #[tokio::test]
    async fn unauthorized_is_auth_required_without_retry() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        Mock::given(method("GET"))
            .and(path("/private"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = client(&server, vec![server.uri()]).get("/private").await.unwrap_err();
        assert!(matches!(err, Error::AuthRequired(_)), "got {err:?}");
        assert_eq!(count(&server, "/private").await, 1);
    }

    #[tokio::test]
    async fn retries_5xx_and_then_succeeds() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        mount_apresolve(&server, &server).await;
        Mock::given(method("GET"))
            .and(path("/flaky"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/flaky"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok".to_vec()))
            .with_priority(2)
            .mount(&server)
            .await;

        let body = client(&server, vec![server.uri()]).get("/flaky").await.unwrap();
        assert_eq!(&body[..], b"ok");
        assert_eq!(count(&server, "/flaky").await, 3);
    }

    #[tokio::test]
    async fn gives_up_after_the_configured_attempts() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        mount_apresolve(&server, &server).await;
        Mock::given(method("GET"))
            .and(path("/down"))
            .respond_with(ResponseTemplate::new(502).set_body_string("bad gateway"))
            .mount(&server)
            .await;

        let err = client(&server, vec![server.uri()]).get("/down").await.unwrap_err();
        assert!(matches!(&err, Error::Protocol { detail, .. } if detail.contains("502")), "got {err:?}");
        assert_eq!(count(&server, "/down").await, consts::SPCLIENT_RETRY_ATTEMPTS as usize);
    }

    #[tokio::test]
    async fn honours_a_short_retry_after_on_429() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        mount_apresolve(&server, &server).await;
        Mock::given(method("GET"))
            .and(path("/busy"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/busy"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"later".to_vec()))
            .with_priority(2)
            .mount(&server)
            .await;

        let started = StdInstant::now();
        let body = client(&server, vec![server.uri()]).get("/busy").await.unwrap();
        assert_eq!(&body[..], b"later");
        assert!(started.elapsed() >= Duration::from_secs(1), "must wait for Retry-After");
        assert_eq!(count(&server, "/busy").await, 2);
    }

    #[tokio::test]
    async fn a_long_retry_after_is_surfaced_as_rate_limited() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        Mock::given(method("GET"))
            .and(path("/busy"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
            .mount(&server)
            .await;

        let err = client(&server, vec![server.uri()]).get("/busy").await.unwrap_err();
        assert!(matches!(err, Error::RateLimited { retry_after } if retry_after == Duration::from_secs(60)), "got {err:?}");
        assert_eq!(count(&server, "/busy").await, 1);
    }

    #[tokio::test]
    async fn rotates_to_the_next_host_after_repeated_failures() {
        let auth = MockServer::start().await;
        mount_auth(&auth).await;
        let dead = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(503)).mount(&dead).await;
        let alive = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/thing"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"alive".to_vec()))
            .mount(&alive)
            .await;

        let client = client(&auth, vec![dead.uri(), alive.uri()]);
        assert!(client.get("/thing").await.is_err(), "the first host fails every attempt");
        assert_eq!(count(&dead, "/thing").await, 3);

        let body = client.get("/thing").await.unwrap();
        assert_eq!(&body[..], b"alive");
        assert_eq!(count(&alive, "/thing").await, 1);
        assert_eq!(client.base_url(), alive.uri());
    }

    #[tokio::test]
    async fn re_resolves_hosts_when_the_list_runs_out() {
        let auth = MockServer::start().await;
        mount_auth(&auth).await;
        let dead = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(503)).mount(&dead).await;
        let alive = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/thing"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"resolved".to_vec()))
            .mount(&alive)
            .await;
        mount_apresolve(&auth, &alive).await;

        let client = client(&auth, vec![dead.uri()]);
        assert!(client.get("/thing").await.is_err());
        let body = client.get("/thing").await.unwrap();
        assert_eq!(&body[..], b"resolved");
        assert_eq!(count(&auth, "/apresolve").await, 1);
        assert_eq!(client.base_url(), format!("http://{}", alive.address()));
    }

    #[tokio::test]
    async fn get_json_sends_accept_json_and_returns_the_body() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        Mock::given(method("GET"))
            .and(path("/color-lyrics/v2/track/abc"))
            .and(header("accept", "application/json"))
            .and(header("authorization", "Bearer bearer-1"))
            .and(header("client-token", "ct-1"))
            .and(query_param("product", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "lyrics": {} })))
            .expect(1)
            .mount(&server)
            .await;

        let body = client(&server, vec![server.uri()]).get_json("/color-lyrics/v2/track/abc").await.unwrap();
        assert_eq!(body.as_deref(), Some(&br#"{"lyrics":{}}"#[..]));
    }

    #[tokio::test]
    async fn get_json_treats_404_as_none_without_retrying() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        Mock::given(method("GET"))
            .and(path("/color-lyrics/v2/track/none"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let body = client(&server, vec![server.uri()]).get_json("/color-lyrics/v2/track/none").await.unwrap();
        assert!(body.is_none());
        assert_eq!(count(&server, "/color-lyrics/v2/track/none").await, 1);
    }

    #[tokio::test]
    async fn post_json_sends_json_with_the_metrics_query() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        Mock::given(method("POST"))
            .and(path("/collection/v2/paging"))
            .and(header("content-type", "application/json"))
            .and(header("accept", "application/json"))
            .and(header("client-token", "ct-1"))
            .and(query_param("product", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "items": [] })))
            .expect(1)
            .mount(&server)
            .await;

        let body = client(&server, vec![server.uri()])
            .post_json("/collection/v2/paging", r#"{"set":"collection"}"#.to_string())
            .await
            .unwrap();
        assert_eq!(&body[..], br#"{"items":[]}"#);
        let requests = server.received_requests().await.unwrap();
        let post = requests.iter().find(|r| r.url.path() == "/collection/v2/paging").unwrap();
        assert_eq!(post.body, br#"{"set":"collection"}"#);
    }

    #[tokio::test]
    async fn post_json_to_uses_the_absolute_url_without_metrics() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        Mock::given(method("POST"))
            .and(path("/pathfinder/v2/query"))
            .and(header("authorization", "Bearer bearer-1"))
            .and(header("client-token", "ct-1"))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "data": {} })))
            .expect(1)
            .mount(&server)
            .await;

        let url = format!("{}/pathfinder/v2/query", server.uri());
        let body = client(&server, vec![server.uri()]).post_json_to(&url, "{}".to_string()).await.unwrap();
        assert_eq!(&body[..], br#"{"data":{}}"#);
        let requests = server.received_requests().await.unwrap();
        let post = requests.iter().find(|r| r.url.path() == "/pathfinder/v2/query").unwrap();
        assert!(post.url.query().is_none(), "no metrics or salt on absolute urls");
    }

    #[tokio::test]
    async fn post_json_to_surfaces_412_as_a_protocol_error_with_the_body() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        Mock::given(method("POST"))
            .and(path("/pathfinder/v2/query"))
            .respond_with(ResponseTemplate::new(412).set_body_string("\"Invalid query hash\""))
            .mount(&server)
            .await;

        let url = format!("{}/pathfinder/v2/query", server.uri());
        let err = client(&server, vec![server.uri()]).post_json_to(&url, "{}".to_string()).await.unwrap_err();
        assert!(
            matches!(&err, Error::Protocol { detail, .. } if detail.contains("412") && detail.contains("Invalid query hash")),
            "got {err:?}"
        );
        assert_eq!(count(&server, "/pathfinder/v2/query").await, 1);
    }
}
