//! An authenticated session (spec section 5): connect, log in, keep alive, hand out
//! the pieces the metadata and audio crates need.

pub mod dispatcher;
pub mod link;
pub(crate) mod supervisor;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::watch;

use crate::ap;
use crate::apresolve;
use crate::auth::cache::CacheDir;
use crate::auth::credentials::{CredentialKind, Credentials};
use crate::client_token::ClientTokenProvider;
use crate::login5::Login5;
use crate::spclient::SpClient;
use crate::{consts, http, random, Error, FileId, Result, SpotifyId};
pub use link::{ApLink, SessionState};

pub struct SessionBuilder {
    cache_dir: Option<PathBuf>,
    device_name: String,
}

impl Default for SessionBuilder {
    fn default() -> Self {
        SessionBuilder { cache_dir: None, device_name: "housecat".to_string() }
    }
}

impl SessionBuilder {
    /// Where credentials, the device id, and the client token are kept between runs.
    pub fn cache_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_dir = Some(path.into());
        self
    }

    pub fn device_name(mut self, name: impl Into<String>) -> Self {
        self.device_name = name.into();
        self
    }

    /// The connect sequence of spec section 5.1.
    pub async fn connect(self, credentials: Credentials) -> Result<Session> {
        let cache = self.cache_dir.map(CacheDir::new);
        let device_id = match &cache {
            Some(cache) => cache.device_id()?,
            None => hex::encode(random::random_bytes::<20>()),
        };
        let http = http::client();

        let hosts = apresolve::resolve(&http, consts::APRESOLVE_URL).await;
        let (transport, stored) = connect_and_login(&hosts.accesspoints, &credentials, &device_id).await?;
        if let Some(cache) = &cache {
            cache.save_credentials(&stored)?;
        }
        let username = stored.username.clone().unwrap_or_default();
        tracing::info!(%username, "logged in to the access point");

        let (closing_tx, closing_rx) = watch::channel(false);
        let link = Arc::new(ApLink::new(closing_tx));
        let (country_tx, mut country_rx) = watch::channel(None);
        let (product_tx, mut product_rx) = watch::channel(None);

        let connector = reconnector(http.clone(), consts::APRESOLVE_URL.to_string(), device_id.clone(), stored.clone());
        let on_credentials: Box<dyn Fn(&Credentials) + Send> = Box::new({
            let cache = cache.clone();
            move |credentials| {
                if let Some(cache) = &cache
                    && let Err(error) = cache.save_credentials(credentials)
                {
                    tracing::warn!(%error, "could not save refreshed credentials");
                }
            }
        });
        tokio::spawn(supervisor::run(
            transport,
            dispatcher::Welcome { country: country_tx, product: product_tx },
            connector,
            link.clone(),
            closing_rx,
            on_credentials,
        ));

        let country = await_value(&mut country_rx, "country code").await?;
        let attributes = await_value(&mut product_rx, "product info").await?;
        match attributes.get("type") {
            Some(kind) if kind != "premium" => {
                tracing::error!(account_type = %kind, "account is not premium");
                link.close();
                return Err(Error::PremiumRequired);
            }
            Some(_) => {}
            None => tracing::warn!("product info carried no account type"),
        }

        let client_token = Arc::new(ClientTokenProvider::new(http.clone(), device_id.clone(), cache.clone()));
        let login5 = Arc::new(Login5::new(http.clone(), device_id.clone(), client_token.clone(), stored)?);
        let spclient = SpClient::new(
            http.clone(),
            hosts.spclients.iter().map(|host| format!("https://{host}")).collect(),
            country.clone(),
            login5.clone(),
            client_token.clone(),
        );
        if let Err(error) = client_token.get().await {
            tracing::warn!(%error, "client token unavailable at connect; it will be retried on demand");
        }

        Ok(Session(Arc::new(Inner {
            username,
            country,
            device_id,
            device_name: self.device_name,
            attributes,
            http,
            client_token,
            login5,
            spclient,
            link,
        })))
    }
}

/// Try access points in order. `TryAnotherAccessPoint` and transport failures move on;
/// credential and premium failures stop immediately. After a token login, reconnect with
/// the stored credentials the way librespot does, so the live session is a stored-credential one.
async fn connect_and_login(
    accesspoints: &[String],
    credentials: &Credentials,
    device_id: &str,
) -> Result<(ap::Transport, Credentials)> {
    let mut last_error = None;
    for addr in accesspoints.iter().take(consts::MAX_ACCESS_POINT_ATTEMPTS) {
        tracing::info!(%addr, "connecting to access point");
        let mut transport = match ap::connect(addr).await {
            Ok(transport) => transport,
            Err(error) => {
                tracing::warn!(%addr, %error, "access point connect failed");
                last_error = Some(error);
                continue;
            }
        };
        match ap::login(&mut transport, credentials, device_id).await {
            Ok(mut stored) => {
                if credentials.kind == CredentialKind::AccessToken {
                    tracing::debug!("reconnecting with the stored credentials");
                    transport = ap::connect(addr).await?;
                    stored = ap::login(&mut transport, &stored, device_id).await?;
                }
                return Ok((transport, stored));
            }
            Err(Error::TryAnotherAccessPoint) => {
                tracing::warn!(%addr, "told to try another access point");
                last_error = Some(Error::TryAnotherAccessPoint);
            }
            Err(error @ (Error::AuthRequired(_) | Error::PremiumRequired)) => return Err(error),
            Err(error) => {
                tracing::warn!(%addr, %error, "login failed, trying another access point");
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| Error::protocol("connect", "no access points to try")))
}

/// The connector the supervisor calls after every loss: re-resolve the access points,
/// log in with the newest stored credentials, and remember the ones the access point
/// hands back so the next reconnect uses them.
fn reconnector(
    http: reqwest::Client,
    apresolve_url: String,
    device_id: String,
    initial: Credentials,
) -> supervisor::Connector<ap::Transport> {
    let current = Arc::new(std::sync::Mutex::new(initial));
    Box::new(move || {
        let http = http.clone();
        let apresolve_url = apresolve_url.clone();
        let device_id = device_id.clone();
        let current = current.clone();
        Box::pin(async move {
            let hosts = apresolve::resolve(&http, &apresolve_url).await;
            let credentials = current.lock().expect("credentials lock").clone();
            let (transport, stored) = connect_and_login(&hosts.accesspoints, &credentials, &device_id).await?;
            *current.lock().expect("credentials lock") = stored.clone();
            Ok(supervisor::Connection { transport, credentials: stored })
        })
    })
}

async fn await_value<T: Clone>(rx: &mut watch::Receiver<Option<T>>, what: &'static str) -> Result<T> {
    let wait = async {
        loop {
            if let Some(value) = rx.borrow_and_update().clone() {
                return Ok(value);
            }
            rx.changed().await.map_err(|_| Error::Disconnected)?;
        }
    };
    tokio::time::timeout(consts::PRODUCT_INFO_TIMEOUT, wait)
        .await
        .map_err(|_| Error::Timeout(what))?
}

struct Inner {
    username: String,
    country: String,
    device_id: String,
    device_name: String,
    attributes: HashMap<String, String>,
    http: reqwest::Client,
    client_token: Arc<ClientTokenProvider>,
    login5: Arc<Login5>,
    spclient: SpClient,
    link: Arc<ApLink>,
}

impl Drop for Inner {
    /// The last `Session` handle is gone: stop the socket task and close the socket.
    fn drop(&mut self) {
        self.link.close();
    }
}

/// A cheap, cloneable handle over one connected session.
#[derive(Clone)]
pub struct Session(Arc<Inner>);

impl Session {
    pub fn builder() -> SessionBuilder {
        SessionBuilder::default()
    }

    pub fn username(&self) -> &str {
        &self.0.username
    }

    pub fn country(&self) -> &str {
        &self.0.country
    }

    pub fn device_id(&self) -> &str {
        &self.0.device_id
    }

    pub fn device_name(&self) -> &str {
        &self.0.device_name
    }

    /// The product-info attributes the access point sent, for example `type`.
    pub fn attributes(&self) -> &HashMap<String, String> {
        &self.0.attributes
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.0.http
    }

    pub fn spclient(&self) -> &SpClient {
        &self.0.spclient
    }

    pub fn client_token(&self) -> &Arc<ClientTokenProvider> {
        &self.0.client_token
    }

    pub fn login5(&self) -> &Arc<Login5> {
        &self.0.login5
    }

    /// The login5 bearer token, for apps that call the Web API themselves.
    pub async fn access_token(&self) -> Result<String> {
        self.0.login5.bearer().await
    }

    pub async fn audio_key(&self, track: SpotifyId, file: FileId) -> Result<[u8; 16]> {
        self.0.link.audio_key(track, file).await
    }

    pub fn state(&self) -> watch::Receiver<SessionState> {
        self.0.link.state()
    }

    pub fn current_state(&self) -> SessionState {
        self.0.link.current_state()
    }

    pub fn close(&self) {
        self.0.link.close()
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("username", &self.0.username)
            .field("country", &self.0.country)
            .field("device_id", &self.0.device_id)
            .field("state", &*self.0.link.state().borrow())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults() {
        let builder = SessionBuilder::default();
        assert_eq!(builder.device_name, "housecat");
        assert!(builder.cache_dir.is_none());
        let builder = Session::builder().cache_dir("x").device_name("y");
        assert_eq!(builder.cache_dir.as_deref(), Some(std::path::Path::new("x")));
        assert_eq!(builder.device_name, "y");
    }

    #[tokio::test]
    async fn connect_with_no_reachable_access_point_fails_fast() {
        let err = connect_and_login(&["127.0.0.1:9".to_string()], &Credentials::access_token("t"), "dev")
            .await
            .unwrap_err();
        assert!(!matches!(err, Error::AuthRequired(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn await_value_times_out_when_nothing_arrives() {
        tokio::time::pause();
        let (_tx, mut rx) = watch::channel::<Option<String>>(None);
        let err = await_value(&mut rx, "country code").await.unwrap_err();
        assert!(matches!(err, Error::Timeout("country code")));
    }

    #[tokio::test]
    async fn reconnector_reports_unreachable_access_points_as_transport_errors() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // apresolve names an access point nobody listens on, so the connector must fail
        // with something the supervisor retries, never with AuthRequired.
        let apresolve = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accesspoint": ["127.0.0.1:9"], "spclient": []
            })))
            .mount(&apresolve)
            .await;

        let mut connect = reconnector(
            reqwest::Client::new(),
            format!("{}/", apresolve.uri()),
            "dev".to_string(),
            Credentials::stored("alice", vec![1]),
        );
        let err = connect().await.unwrap_err();
        assert!(!matches!(err, Error::AuthRequired(_) | Error::PremiumRequired), "got {err:?}");
    }
}
