//! Resolve access-point and spclient hosts (spec section 5.1, step 2).
//!
//! One GET to apresolve returns `host:port` lists. Any failure falls back to the
//! hardcoded hosts in `consts`, so this function never blocks a connect.

use serde::Deserialize;

use crate::consts;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHosts {
    /// `host:port` entries for the access-point TCP connection, in preference order.
    pub accesspoints: Vec<String>,
    /// `host:port` entries for HTTPS spclient calls, in preference order.
    pub spclients: Vec<String>,
}

impl ResolvedHosts {
    pub fn fallback() -> Self {
        ResolvedHosts {
            accesspoints: vec![consts::AP_FALLBACK.to_string()],
            spclients: vec![consts::SPCLIENT_FALLBACK.to_string()],
        }
    }
}

#[derive(Deserialize)]
struct ApResolveResponse {
    #[serde(default)]
    accesspoint: Vec<String>,
    #[serde(default)]
    spclient: Vec<String>,
}

pub async fn resolve(http: &reqwest::Client, url: &str) -> ResolvedHosts {
    match fetch(http, url).await {
        Ok(hosts) => hosts,
        Err(error) => {
            tracing::warn!(%error, "apresolve failed, using fallback hosts");
            ResolvedHosts::fallback()
        }
    }
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<ResolvedHosts> {
    let response = http
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::transport(url, e))?;
    let body: ApResolveResponse = response.json().await.map_err(|e| Error::transport(url, e))?;

    let mut hosts = ResolvedHosts { accesspoints: body.accesspoint, spclients: body.spclient };
    if hosts.accesspoints.is_empty() {
        hosts.accesspoints.push(consts::AP_FALLBACK.to_string());
    }
    if hosts.spclients.is_empty() {
        hosts.spclients.push(consts::SPCLIENT_FALLBACK.to_string());
    }
    Ok(hosts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[tokio::test]
    async fn parses_hosts_from_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .and(query_param("type", "accesspoint"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accesspoint": ["ap-gew4.spotify.com:4070", "ap-gew4.spotify.com:443"],
                "spclient": ["gew4-spclient.spotify.com:443"]
            })))
            .mount(&server)
            .await;

        let url = format!("{}/?type=accesspoint&type=spclient", server.uri());
        let hosts = resolve(&client(), &url).await;
        assert_eq!(hosts.accesspoints, vec!["ap-gew4.spotify.com:4070", "ap-gew4.spotify.com:443"]);
        assert_eq!(hosts.spclients, vec!["gew4-spclient.spotify.com:443"]);
    }

    #[tokio::test]
    async fn empty_lists_get_fallbacks() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let hosts = resolve(&client(), &format!("{}/", server.uri())).await;
        assert_eq!(hosts.accesspoints, vec![consts::AP_FALLBACK]);
        assert_eq!(hosts.spclients, vec![consts::SPCLIENT_FALLBACK]);
    }

    #[tokio::test]
    async fn server_error_gives_fallbacks() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let hosts = resolve(&client(), &format!("{}/", server.uri())).await;
        assert_eq!(hosts, ResolvedHosts::fallback());
    }

    #[tokio::test]
    async fn unreachable_server_gives_fallbacks() {
        // Port 9 (discard) is almost never listening; connection refused is the expected path.
        let hosts = resolve(&client(), "http://127.0.0.1:9/").await;
        assert_eq!(hosts, ResolvedHosts::fallback());
    }
}
