//! CDN URLs for audio files: storage-resolve and the per-host expiry formats (spec section 7, step 2).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use prost::Message;

use housecat_protocol::spotify::download::proto as dl;

use crate::spclient::SpClient;
use crate::{consts, Error, FileId, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CdnUrl {
    pub url: String,
    /// When the URL stops working, minus `CDN_URL_EXPIRY_MARGIN`; `None` when it does not expire.
    pub expires_at: Option<SystemTime>,
}

#[derive(Clone, Debug)]
pub struct CdnUrls {
    pub file_id: FileId,
    pub urls: Vec<CdnUrl>,
}

impl CdnUrls {
    pub async fn resolve(spclient: &SpClient, file_id: FileId) -> Result<Self> {
        let bytes = spclient.get(&format!("{}{}", consts::STORAGE_RESOLVE_PATH, file_id.to_hex())).await?;
        Self::from_response(file_id, &bytes)
    }

    /// Decode a storage-resolve response. RESTRICTED and STORAGE results are `NotAvailable`.
    pub fn from_response(file_id: FileId, bytes: &[u8]) -> Result<Self> {
        let message = dl::StorageResolveResponse::decode(bytes)
            .map_err(|e| Error::protocol("storage-resolve", format!("bad protobuf: {e}")))?;
        match dl::storage_resolve_response::Result::try_from(message.result) {
            Ok(dl::storage_resolve_response::Result::Cdn) => {}
            Ok(dl::storage_resolve_response::Result::Restricted) => {
                return Err(Error::NotAvailable { reason: "storage-resolve answered RESTRICTED".to_string() });
            }
            Ok(dl::storage_resolve_response::Result::Storage) => {
                return Err(Error::NotAvailable { reason: "the file is not served from the CDN".to_string() });
            }
            Err(_) => {
                return Err(Error::protocol("storage-resolve", format!("unknown result {}", message.result)));
            }
        }
        if message.cdnurl.is_empty() {
            return Err(Error::protocol("storage-resolve", "no CDN urls in the response"));
        }
        let expiring = !message.fileid.is_empty();
        let urls = message
            .cdnurl
            .into_iter()
            .map(|url| {
                let expires_at = if expiring { expiry_from_url(&url) } else { None };
                CdnUrl { url, expires_at }
            })
            .collect();
        Ok(CdnUrls { file_id, urls })
    }

    /// The URLs still usable at `now`, in the server's order of preference.
    pub fn fresh(&self, now: SystemTime) -> Vec<&str> {
        self.urls
            .iter()
            .filter(|url| url.expires_at.is_none_or(|expiry| now < expiry))
            .map(|url| url.url.as_str())
            .collect()
    }
}

/// The expiry a CDN URL carries in its query, minus the margin. Four formats are known
/// (librespot's `cdn_url.rs`): `verify=<unix>-<sig>`, `__token__=exp=<unix>~hmac=...`,
/// `Expires=<unix>~FullPath~hmac=...`, and a bare `?<unix>_<sig>`. Unknown formats give `None`,
/// which means "assume it does not expire".
pub fn expiry_from_url(url: &str) -> Option<SystemTime> {
    let query = url.split_once('?')?.1;
    let pairs: Vec<(&str, &str)> = query.split('&').map(|kv| kv.split_once('=').unwrap_or((kv, ""))).collect();
    let value = |key: &str| pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    let stamp = if let Some(verify) = value("verify") {
        verify.split_once('-')?.0
    } else if let Some(token) = value("__token__") {
        token.split_once("exp=")?.1.split('~').next()?
    } else if let Some(expires) = value("Expires") {
        expires.split('~').next()?
    } else {
        query.split('_').next()?
    };
    let seconds: u64 = stamp.parse().ok()?;
    (UNIX_EPOCH + Duration::from_secs(seconds)).checked_sub(consts::CDN_URL_EXPIRY_MARGIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spclient::tests::{client, mount_auth};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const STAMP: u64 = 1_688_165_560;

    fn expected() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(STAMP) - consts::CDN_URL_EXPIRY_MARGIN
    }

    #[test]
    fn expiry_is_parsed_from_the_four_known_query_formats() {
        // The vectors are librespot's (core/src/cdn_url.rs).
        let urls = [
            format!("https://audio-cf.spotifycdn.com/audio/844ecdb2?verify={STAMP}-4R3I2w2q7OfNkR%2FGH8qH7xtIKUPlDxywBuADY%2BsvMeU%3D"),
            format!("https://audio-ak-spotify-com.akamaized.net/audio/foo?__token__=exp={STAMP}~hmac=4e661527574fab5793adb99cf04e1c2ce12294c71fe1d39ffbfabdcfe8ce3b41"),
            format!("https://audio-gm-off.spotifycdn.com/audio/foo?Expires={STAMP}~FullPath~hmac=IIZA28qptl8cuGLq15-SjHKHtLoxzpy_6r_JpAU4MfM="),
            format!("https://audio4-fa.scdn.co/audio/foo?{STAMP}_0GKSyXjLaTW1BksFOyI4J7Tf9tZDbBUNNPu9Mt4mhH4="),
        ];
        for url in &urls {
            assert_eq!(expiry_from_url(url), Some(expected()), "{url}");
        }
        assert_eq!(expiry_from_url("https://audio4-fa.scdn.co/foo?baz"), None);
        assert_eq!(expiry_from_url("https://audio4-fa.scdn.co/foo"), None);
    }

    fn response(result: dl::storage_resolve_response::Result, urls: &[&str], fileid: Vec<u8>) -> Vec<u8> {
        dl::StorageResolveResponse { result: result as i32, cdnurl: urls.iter().map(|u| u.to_string()).collect(), fileid }
            .encode_to_vec()
    }

    #[test]
    fn from_response_maps_results() {
        let file_id = FileId([1; 20]);
        let ok = CdnUrls::from_response(
            file_id,
            &response(
                dl::storage_resolve_response::Result::Cdn,
                &[&format!("https://a.example/x?verify={STAMP}-sig"), "https://b.example/x?baz"],
                vec![1],
            ),
        )
        .unwrap();
        assert_eq!(ok.file_id, file_id);
        assert_eq!(ok.urls.len(), 2);
        assert_eq!(ok.urls[0].expires_at, Some(expected()));
        assert_eq!(ok.urls[1].expires_at, None);

        let non_expiring = CdnUrls::from_response(
            file_id,
            &response(dl::storage_resolve_response::Result::Cdn, &[&format!("https://a.example/x?verify={STAMP}-sig")], vec![]),
        )
        .unwrap();
        assert_eq!(non_expiring.urls[0].expires_at, None, "an empty fileid means the urls never expire");

        let restricted = CdnUrls::from_response(file_id, &response(dl::storage_resolve_response::Result::Restricted, &[], vec![]));
        assert!(matches!(restricted, Err(Error::NotAvailable { .. })), "got {restricted:?}");
        let storage = CdnUrls::from_response(file_id, &response(dl::storage_resolve_response::Result::Storage, &[], vec![]));
        assert!(matches!(storage, Err(Error::NotAvailable { .. })), "got {storage:?}");
        let empty = CdnUrls::from_response(file_id, &response(dl::storage_resolve_response::Result::Cdn, &[], vec![]));
        assert!(matches!(empty, Err(Error::Protocol { .. })), "got {empty:?}");
        assert!(matches!(CdnUrls::from_response(file_id, b"\xff\xff"), Err(Error::Protocol { .. })));
    }

    #[test]
    fn fresh_drops_expired_urls_and_keeps_order() {
        let urls = CdnUrls {
            file_id: FileId([1; 20]),
            urls: vec![
                CdnUrl { url: "https://expired".into(), expires_at: Some(UNIX_EPOCH + Duration::from_secs(10)) },
                CdnUrl { url: "https://forever".into(), expires_at: None },
                CdnUrl { url: "https://later".into(), expires_at: Some(UNIX_EPOCH + Duration::from_secs(1000)) },
            ],
        };
        let now = UNIX_EPOCH + Duration::from_secs(500);
        assert_eq!(urls.fresh(now), vec!["https://forever", "https://later"]);
        assert_eq!(urls.fresh(UNIX_EPOCH + Duration::from_secs(5000)).len(), 1);
    }

    #[tokio::test]
    async fn resolve_gets_storage_resolve_for_the_hex_file_id() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        let file_id = FileId([0xab; 20]);
        Mock::given(method("GET"))
            .and(path(format!("/storage-resolve/files/audio/interactive/{}", "ab".repeat(20))))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(response(
                dl::storage_resolve_response::Result::Cdn,
                &["https://cdn.example/a"],
                vec![1],
            )))
            .expect(1)
            .mount(&server)
            .await;

        let urls = CdnUrls::resolve(&client(&server, vec![server.uri()]), file_id).await.unwrap();
        assert_eq!(urls.urls[0].url, "https://cdn.example/a");
    }
}
