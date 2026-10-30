//! The `Metadata` handle (spec section 4.1): cheap to clone, one method per lookup.

use std::sync::Arc;

use prost::Message;

use housecat_core::spclient::SpClient;
use housecat_core::{Error, IdKind, Result, Session, SpotifyId};
use housecat_protocol::spotify::extendedmetadata::ExtensionKind;
use housecat_protocol::spotify::metadata as md;
use housecat_protocol::spotify::playlist4::proto as pl;

use crate::audio_info::{self, TrackAudio};
use crate::model::{Album, Artist, Episode, Lyrics, Playlist, RootlistEntry, Show, Track};
use crate::playlist::{self, ROOTLIST_DECORATE};
use crate::{convert, entities, lyrics};

/// Rootlist items requested per page.
const ROOTLIST_PAGE: usize = 200;

#[derive(Clone)]
enum Backend {
    Session(Session),
    Direct { spclient: Arc<SpClient>, country: String, username: String },
}

/// Typed catalog access over one session. Clone it freely; clones share the session.
#[derive(Clone)]
pub struct Metadata {
    backend: Backend,
}

impl std::fmt::Debug for Metadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metadata").field("country", &self.country()).finish_non_exhaustive()
    }
}

impl Metadata {
    pub fn new(session: &Session) -> Self {
        Metadata { backend: Backend::Session(session.clone()) }
    }

    /// A handle over a bare spclient, for tests and tools that have no session.
    #[doc(hidden)]
    pub fn with_client(spclient: Arc<SpClient>, country: impl Into<String>, username: impl Into<String>) -> Self {
        Metadata { backend: Backend::Direct { spclient, country: country.into(), username: username.into() } }
    }

    pub(crate) fn spclient(&self) -> &SpClient {
        match &self.backend {
            Backend::Session(session) => session.spclient(),
            Backend::Direct { spclient, .. } => spclient,
        }
    }

    fn country(&self) -> &str {
        match &self.backend {
            Backend::Session(session) => session.country(),
            Backend::Direct { country, .. } => country,
        }
    }

    pub(crate) fn username(&self) -> &str {
        match &self.backend {
            Backend::Session(session) => session.username(),
            Backend::Direct { username, .. } => username,
        }
    }

    pub async fn track(&self, id: SpotifyId) -> Result<Track> {
        one(self.tracks(&[id]).await?, id)
    }

    /// One slot per id, in order; `None` where Spotify has no such track.
    pub async fn tracks(&self, ids: &[SpotifyId]) -> Result<Vec<Option<Track>>> {
        expect_kind(ids, IdKind::Track)?;
        let payloads = entities::fetch(self.spclient(), ids, ExtensionKind::TrackV4).await?;
        map_all(payloads, "track", |proto: &md::Track| convert::track(proto, self.country()))
    }

    pub async fn album(&self, id: SpotifyId) -> Result<Album> {
        one(self.albums(&[id]).await?, id)
    }

    pub async fn albums(&self, ids: &[SpotifyId]) -> Result<Vec<Option<Album>>> {
        expect_kind(ids, IdKind::Album)?;
        let payloads = entities::fetch(self.spclient(), ids, ExtensionKind::AlbumV4).await?;
        map_all(payloads, "album", |proto: &md::Album| convert::album(proto))
    }

    pub async fn artist(&self, id: SpotifyId) -> Result<Artist> {
        one(self.artists(&[id]).await?, id)
    }

    pub async fn artists(&self, ids: &[SpotifyId]) -> Result<Vec<Option<Artist>>> {
        expect_kind(ids, IdKind::Artist)?;
        let payloads = entities::fetch(self.spclient(), ids, ExtensionKind::ArtistV4).await?;
        map_all(payloads, "artist", |proto: &md::Artist| convert::artist(proto, self.country()))
    }

    pub async fn show(&self, id: SpotifyId) -> Result<Show> {
        one(self.shows(&[id]).await?, id)
    }

    pub async fn shows(&self, ids: &[SpotifyId]) -> Result<Vec<Option<Show>>> {
        expect_kind(ids, IdKind::Show)?;
        let payloads = entities::fetch(self.spclient(), ids, ExtensionKind::ShowV4).await?;
        map_all(payloads, "show", |proto: &md::Show| convert::show(proto))
    }

    pub async fn episode(&self, id: SpotifyId) -> Result<Episode> {
        one(self.episodes(&[id]).await?, id)
    }

    pub async fn episodes(&self, ids: &[SpotifyId]) -> Result<Vec<Option<Episode>>> {
        expect_kind(ids, IdKind::Episode)?;
        let payloads = entities::fetch(self.spclient(), ids, ExtensionKind::EpisodeV4).await?;
        map_all(payloads, "episode", |proto: &md::Episode| convert::episode(proto))
    }

    /// The audio files of a track, for the audio crate.
    pub async fn track_audio(&self, id: SpotifyId) -> Result<TrackAudio> {
        audio_info::track_audio_with(self.spclient(), id).await
    }

    pub async fn episode_audio(&self, id: SpotifyId) -> Result<TrackAudio> {
        audio_info::episode_audio_with(self.spclient(), id).await
    }

    pub async fn playlist(&self, id: SpotifyId) -> Result<Playlist> {
        expect_kind(&[id], IdKind::Playlist)?;
        let bytes = self.spclient().get(&format!("/playlist/v2/playlist/{}", id.to_base62())).await?;
        let content = pl::SelectedListContent::decode(bytes)
            .map_err(|e| Error::protocol("playlist", format!("bad protobuf: {e}")))?;
        Ok(playlist::from_proto(id, &content))
    }

    /// The user's playlists and folders, flattened, in the order the Spotify app shows them.
    pub async fn rootlist(&self) -> Result<Vec<RootlistEntry>> {
        self.rootlist_paged(ROOTLIST_PAGE).await
    }

    pub(crate) async fn rootlist_paged(&self, page: usize) -> Result<Vec<RootlistEntry>> {
        let mut items = Vec::new();
        let mut metas = Vec::new();
        let mut from = 0usize;
        loop {
            let path = format!(
                "/playlist/v2/user/{}/rootlist?decorate={ROOTLIST_DECORATE}&from={from}&length={page}",
                playlist::percent_encode(self.username()),
            );
            let bytes = self.spclient().get(&path).await?;
            let content = pl::SelectedListContent::decode(bytes)
                .map_err(|e| Error::protocol("rootlist", format!("bad protobuf: {e}")))?;
            let Some(contents) = content.contents else { break };
            let received = contents.items.len();
            let truncated = contents.truncated;
            items.extend(contents.items);
            metas.extend(contents.meta_items);
            from += received;
            if !truncated || received == 0 {
                break;
            }
        }
        Ok(playlist::flatten_rootlist(&items, &metas))
    }

    /// The track's lyrics, `None` when Spotify has none.
    pub async fn lyrics(&self, track: SpotifyId) -> Result<Option<Lyrics>> {
        expect_kind(&[track], IdKind::Track)?;
        let path = format!(
            "/color-lyrics/v2/track/{}?format=json&vocalRemoval=false&market=from_token",
            track.to_base62()
        );
        match self.spclient().get_json(&path).await? {
            Some(bytes) => lyrics::parse(&bytes).map(Some),
            None => Ok(None),
        }
    }
}

fn expect_kind(ids: &[SpotifyId], kind: IdKind) -> Result<()> {
    match ids.iter().find(|id| id.kind != kind) {
        Some(wrong) => Err(Error::protocol("metadata", format!("{wrong} is not a {}", kind.as_str()))),
        None => Ok(()),
    }
}

fn map_all<P, T>(
    payloads: Vec<Option<Vec<u8>>>,
    what: &'static str,
    map: impl Fn(&P) -> Option<T>,
) -> Result<Vec<Option<T>>>
where
    P: Message + Default,
{
    payloads
        .into_iter()
        .map(|payload| match payload {
            None => Ok(None),
            Some(bytes) => {
                let proto = P::decode(&bytes[..]).map_err(|e| Error::protocol(what, format!("bad protobuf: {e}")))?;
                map(&proto).map(Some).ok_or_else(|| Error::protocol(what, "entity without a usable id"))
            }
        })
        .collect()
}

fn one<T>(mut items: Vec<Option<T>>, id: SpotifyId) -> Result<T> {
    items.pop().flatten().ok_or_else(|| Error::NotFound(id.to_uri()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::Availability;
    use housecat_core::auth::credentials::Credentials;
    use housecat_core::client_token::ClientTokenProvider;
    use housecat_core::login5::Login5;
    use housecat_protocol::spotify::extendedmetadata as ext;
    use prost::Message;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture file")
    }

    /// Client-token and login5 doubles, shared with the entities and catalog tests.
    pub(crate) async fn server_with_auth() -> MockServer {
        use housecat_protocol::spotify::clienttoken::http::v0 as ct;
        use housecat_protocol::spotify::login5::v3 as l5;

        let server = MockServer::start().await;
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
            .mount(&server)
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
            .mount(&server)
            .await;
        server
    }

    /// An spclient whose auth endpoints and host are the mock server.
    pub(crate) fn spclient(server: &MockServer) -> SpClient {
        let http = reqwest::Client::new();
        let client_token = Arc::new(
            ClientTokenProvider::new(http.clone(), "device-1", None)
                .with_url(format!("{}/v1/clienttoken", server.uri())),
        );
        let login5 = Arc::new(
            Login5::new(http.clone(), "device-1", client_token.clone(), Credentials::stored("alice", vec![1]))
                .unwrap()
                .with_url(format!("{}/v3/login", server.uri())),
        );
        SpClient::new(http, vec![server.uri()], "US", login5, client_token)
    }

    pub(crate) fn metadata(server: &MockServer) -> Metadata {
        Metadata::with_client(Arc::new(spclient(server)), "US", "alice")
    }

    /// One extended-metadata response with the given entries, mounted for every request.
    async fn mount_entities(server: &MockServer, entries: Vec<(&str, i32, Option<Vec<u8>>)>) {
        let body = ext::BatchedExtensionResponse {
            header: None,
            extended_metadata: vec![ext::EntityExtensionDataArray {
                header: None,
                extension_kind: ext::ExtensionKind::TrackV4 as i32,
                extension_data: entries
                    .into_iter()
                    .map(|(uri, status, payload)| ext::EntityExtensionData {
                        header: Some(ext::EntityExtensionDataHeader {
                            status_code: status,
                            etag: String::new(),
                            locale: String::new(),
                            cache_ttl_in_seconds: 0,
                            offline_ttl_in_seconds: 0,
                        }),
                        entity_uri: uri.to_string(),
                        extension_data: payload.map(|value| prost_types::Any { type_url: String::new(), value }),
                    })
                    .collect(),
            }],
        }
        .encode_to_vec();
        Mock::given(method("POST"))
            .and(path("/extended-metadata/v0/extended-metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(server)
            .await;
    }

    const TRACK: &str = "spotify:track:4PTG3Z6ehGkBFwjybzWkR8";

    #[tokio::test]
    async fn track_is_fetched_and_mapped() {
        let server = server_with_auth().await;
        mount_entities(&server, vec![(TRACK, 200, Some(fixture("track.pb")))]).await;
        let track = metadata(&server).track(SpotifyId::from_uri(TRACK).unwrap()).await.unwrap();
        assert_eq!(track.name, "Never Gonna Give You Up");
        assert_eq!(track.availability, Availability::Available);
    }

    #[tokio::test]
    async fn missing_track_is_not_found() {
        let server = server_with_auth().await;
        mount_entities(&server, vec![(TRACK, 404, None)]).await;
        let err = metadata(&server).track(SpotifyId::from_uri(TRACK).unwrap()).await.unwrap_err();
        assert!(matches!(&err, Error::NotFound(uri) if uri == TRACK), "got {err:?}");
    }

    #[tokio::test]
    async fn batch_returns_one_slot_per_id_and_sends_every_uri() {
        let server = server_with_auth().await;
        let other = "spotify:track:0000000000000000000001";
        mount_entities(&server, vec![(TRACK, 200, Some(fixture("track.pb"))), (other, 404, None)]).await;
        let ids = [SpotifyId::from_uri(TRACK).unwrap(), SpotifyId::from_uri(other).unwrap()];
        let tracks = metadata(&server).tracks(&ids).await.unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].as_ref().map(|t| t.name.as_str()), Some("Never Gonna Give You Up"));
        assert!(tracks[1].is_none());

        let requests = server.received_requests().await.unwrap();
        let post = requests.iter().find(|r| r.url.path().starts_with("/extended-metadata")).unwrap();
        let request = ext::BatchedEntityRequest::decode(&post.body[..]).unwrap();
        let uris: Vec<&str> = request.entity_request.iter().map(|e| e.entity_uri.as_str()).collect();
        assert_eq!(uris, vec![TRACK, other]);
    }

    #[tokio::test]
    async fn wrong_kind_fails_before_any_request() {
        let server = server_with_auth().await;
        let album = SpotifyId::from_uri("spotify:album:6eUW0wxWtzkFdaEFsTJto6").unwrap();
        let err = metadata(&server).track(album).await.unwrap_err();
        assert!(matches!(err, Error::Protocol { what: "metadata", .. }), "got {err:?}");
        assert!(server.received_requests().await.unwrap().iter().all(|r| !r.url.path().starts_with("/extended")));
    }

    #[tokio::test]
    async fn every_entity_kind_round_trips_through_its_fixture() {
        let server = server_with_auth().await;
        mount_entities(
            &server,
            vec![
                ("spotify:album:6eUW0wxWtzkFdaEFsTJto6", 200, Some(fixture("album.pb"))),
                ("spotify:artist:0gxyHStUsqpMadRV0Di1Qt", 200, Some(fixture("artist.pb"))),
                ("spotify:show:4rOoJ6Egrf8K2IrywzwOMk", 200, Some(fixture("show.pb"))),
                ("spotify:episode:1arAXEEsM5yVp3cFrDRuZ6", 200, Some(fixture("episode.pb"))),
            ],
        )
        .await;
        let m = metadata(&server);
        let album = m.album(SpotifyId::from_uri("spotify:album:6eUW0wxWtzkFdaEFsTJto6").unwrap()).await.unwrap();
        assert_eq!(album.name, "Whenever You Need Somebody");
        let artist = m.artist(SpotifyId::from_uri("spotify:artist:0gxyHStUsqpMadRV0Di1Qt").unwrap()).await.unwrap();
        assert_eq!(artist.name, "Rick Astley");
        let show = m.show(SpotifyId::from_uri("spotify:show:4rOoJ6Egrf8K2IrywzwOMk").unwrap()).await.unwrap();
        assert_eq!(show.publisher, "Joe Rogan");
        let episode = m.episode(SpotifyId::from_uri("spotify:episode:1arAXEEsM5yVp3cFrDRuZ6").unwrap()).await.unwrap();
        assert_eq!(episode.duration, Duration::from_millis(8_510_079));
    }

    fn rootlist_page(items: &[&str], truncated: bool) -> Vec<u8> {
        pl::SelectedListContent {
            contents: Some(pl::ListItems {
                pos: 0,
                truncated,
                items: items.iter().map(|uri| pl::Item { uri: uri.to_string(), attributes: None }).collect(),
                meta_items: items
                    .iter()
                    .map(|uri| pl::MetaItem {
                        attributes: Some(pl::ListAttributes { name: Some(format!("name of {uri}")), ..Default::default() }),
                        ..Default::default()
                    })
                    .collect(),
                available_signals: vec![],
                continuation_token: None,
            }),
            ..Default::default()
        }
        .encode_to_vec()
    }

    #[tokio::test]
    async fn playlist_is_fetched_by_base62_id() {
        use wiremock::matchers::query_param;
        let server = server_with_auth().await;
        Mock::given(method("GET"))
            .and(path("/playlist/v2/playlist/37i9dQZF1DXcBWIGoYBM5M"))
            .and(query_param("product", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(fixture("playlist.pb")))
            .expect(1)
            .mount(&server)
            .await;
        let playlist = metadata(&server)
            .playlist(SpotifyId::from_uri("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M").unwrap())
            .await
            .unwrap();
        assert_eq!(playlist.items.len(), 50);
        assert_eq!(playlist.owner, "spotify");
    }

    #[tokio::test]
    async fn rootlist_pages_until_a_page_is_not_truncated() {
        use wiremock::matchers::query_param;
        let server = server_with_auth().await;
        Mock::given(method("GET"))
            .and(path("/playlist/v2/user/alice/rootlist"))
            .and(query_param("from", "0"))
            .and(query_param("length", "2"))
            .and(query_param("decorate", "revision,attributes,length,owner,capabilities,status_code"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(rootlist_page(
                &["spotify:start-group:g1:Folder", "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"],
                true,
            )))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/playlist/v2/user/alice/rootlist"))
            .and(query_param("from", "2"))
            .and(query_param("length", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(rootlist_page(
                &["spotify:end-group:g1", "spotify:playlist:6eUW0wxWtzkFdaEFsTJto6"],
                false,
            )))
            .expect(1)
            .mount(&server)
            .await;

        let entries = metadata(&server).rootlist_paged(2).await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].folder_path, vec!["Folder".to_string()]);
        assert_eq!(entries[0].playlist.name, "name of spotify:playlist:37i9dQZF1DXcBWIGoYBM5M");
        assert!(entries[1].folder_path.is_empty(), "the folder closed on the second page");
    }

    #[tokio::test]
    async fn lyrics_are_fetched_as_json_and_missing_means_none() {
        use wiremock::matchers::{header, query_param};
        let server = server_with_auth().await;
        Mock::given(method("GET"))
            .and(path("/color-lyrics/v2/track/4PTG3Z6ehGkBFwjybzWkR8"))
            .and(query_param("format", "json"))
            .and(query_param("market", "from_token"))
            .and(header("accept", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(crate::lyrics::tests::SYNCED))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/color-lyrics/v2/track/1arAXEEsM5yVp3cFrDRuZ6"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let m = metadata(&server);
        let lyrics = m.lyrics(SpotifyId::from_uri(TRACK).unwrap()).await.unwrap().expect("lyrics");
        assert_eq!(lyrics.lines.len(), 2);
        assert!(m.lyrics(SpotifyId::from_uri("spotify:track:1arAXEEsM5yVp3cFrDRuZ6").unwrap()).await.unwrap().is_none());
    }
}
