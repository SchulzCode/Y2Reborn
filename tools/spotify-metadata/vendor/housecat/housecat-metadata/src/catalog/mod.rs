//! Search and the user's library over private endpoints (spec section 6.2).

pub(crate) mod library;
pub(crate) mod search;

use std::sync::Arc;

use housecat_core::{consts, Error, IdKind, Result, Session, SpotifyId};

use crate::client::Metadata;
use crate::model::{Album, Artist, Episode, Page, Paging, Saved, SearchKinds, SearchResults, Show, Track};
use library::{LibrarySet, Snapshots};

/// Search and library. Cheap to clone; clones share the session and the library snapshots.
#[derive(Clone)]
pub struct Catalog {
    metadata: Metadata,
    snapshots: Arc<Snapshots>,
    search_url: String,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog").finish_non_exhaustive()
    }
}

impl Catalog {
    pub fn new(session: &Session) -> Self {
        Self::with_metadata(Metadata::new(session))
    }

    /// A catalog over an existing metadata handle, for tests and tools.
    #[doc(hidden)]
    pub fn with_metadata(metadata: Metadata) -> Self {
        Catalog {
            metadata,
            snapshots: Arc::new(Snapshots::new(consts::LIBRARY_SNAPSHOT_TTL)),
            search_url: consts::PATHFINDER_URL.to_string(),
        }
    }

    /// Where search requests go; tests point this at a mock.
    #[doc(hidden)]
    pub fn with_search_url(mut self, url: impl Into<String>) -> Self {
        self.search_url = url.into();
        self
    }

    /// Search the catalog. `limit` applies per kind; only `kinds` are mapped.
    pub async fn search(&self, query: &str, kinds: SearchKinds, limit: u32) -> Result<SearchResults> {
        let body = search::request_body(query, limit, 0);
        let bytes = match self.metadata.spclient().post_json_to(&self.search_url, body).await {
            Ok(bytes) => bytes,
            Err(Error::Protocol { detail, .. }) if detail.contains("HTTP 412") => {
                tracing::error!(%detail, "Spotify rejected the search query hash; SEARCH_DESKTOP_QUERY_HASH needs refreshing");
                return Err(Error::protocol(
                    "search",
                    format!(
                        "Spotify rejected the persisted query hash ({detail}); refresh \
                         housecat_core::consts::SEARCH_DESKTOP_QUERY_HASH from the installed desktop client \
                         (see its doc comment)"
                    ),
                ));
            }
            Err(error) => return Err(error),
        };
        search::parse(&bytes, kinds)
    }

    /// Forget the library snapshots, so the next page request fetches the sets again.
    pub fn invalidate(&self) {
        self.snapshots.invalidate();
    }

    async fn set(&self, set: LibrarySet) -> Result<Arc<Vec<Saved<SpotifyId>>>> {
        let spclient = self.metadata.spclient();
        let username = self.metadata.username();
        self.snapshots.get(set, library::fetch_set(spclient, username, set)).await
    }

    async fn ids_of_kind(&self, set: LibrarySet, kind: IdKind) -> Result<Vec<Saved<SpotifyId>>> {
        Ok(self.set(set).await?.iter().filter(|saved| saved.item.kind == kind).cloned().collect())
    }

    /// Every saved track, newest first.
    pub async fn saved_track_ids(&self) -> Result<Vec<Saved<SpotifyId>>> {
        self.ids_of_kind(LibrarySet::Collection, IdKind::Track).await
    }

    pub async fn saved_album_ids(&self) -> Result<Vec<Saved<SpotifyId>>> {
        self.ids_of_kind(LibrarySet::Collection, IdKind::Album).await
    }

    pub async fn followed_artist_ids(&self) -> Result<Vec<Saved<SpotifyId>>> {
        self.ids_of_kind(LibrarySet::Artists, IdKind::Artist).await
    }

    pub async fn saved_show_ids(&self) -> Result<Vec<Saved<SpotifyId>>> {
        self.ids_of_kind(LibrarySet::Shows, IdKind::Show).await
    }

    pub async fn saved_episode_ids(&self) -> Result<Vec<Saved<SpotifyId>>> {
        self.ids_of_kind(LibrarySet::Episodes, IdKind::Episode).await
    }

    /// One page of saved tracks, newest first, with full metadata. Tracks Spotify no longer
    /// knows are dropped from the page, so a page can be shorter than `paging.limit`.
    pub async fn saved_tracks(&self, paging: Paging) -> Result<Page<Saved<Track>>> {
        let ids = self.saved_track_ids().await?;
        let page = library::page_of(&ids, paging);
        let items = self.metadata.tracks(&page.items.iter().map(|s| s.item).collect::<Vec<_>>()).await?;
        Ok(hydrated(page, items))
    }

    pub async fn saved_albums(&self, paging: Paging) -> Result<Page<Saved<Album>>> {
        let ids = self.saved_album_ids().await?;
        let page = library::page_of(&ids, paging);
        let items = self.metadata.albums(&page.items.iter().map(|s| s.item).collect::<Vec<_>>()).await?;
        Ok(hydrated(page, items))
    }

    pub async fn followed_artists(&self, paging: Paging) -> Result<Page<Saved<Artist>>> {
        let ids = self.followed_artist_ids().await?;
        let page = library::page_of(&ids, paging);
        let items = self.metadata.artists(&page.items.iter().map(|s| s.item).collect::<Vec<_>>()).await?;
        Ok(hydrated(page, items))
    }

    pub async fn saved_shows(&self, paging: Paging) -> Result<Page<Saved<Show>>> {
        let ids = self.saved_show_ids().await?;
        let page = library::page_of(&ids, paging);
        let items = self.metadata.shows(&page.items.iter().map(|s| s.item).collect::<Vec<_>>()).await?;
        Ok(hydrated(page, items))
    }

    pub async fn saved_episodes(&self, paging: Paging) -> Result<Page<Saved<Episode>>> {
        let ids = self.saved_episode_ids().await?;
        let page = library::page_of(&ids, paging);
        let items = self.metadata.episodes(&page.items.iter().map(|s| s.item).collect::<Vec<_>>()).await?;
        Ok(hydrated(page, items))
    }
}

/// Pair each id of the page with its entity, dropping the ones Spotify did not return.
fn hydrated<T>(page: Page<Saved<SpotifyId>>, entities: Vec<Option<T>>) -> Page<Saved<T>> {
    let items = page
        .items
        .into_iter()
        .zip(entities)
        .filter_map(|(saved, entity)| match entity {
            Some(item) => Some(Saved { item, added_at: saved.added_at }),
            None => {
                tracing::warn!(id = %saved.item, "saved item is gone from the catalog; dropped from the page");
                None
            }
        })
        .collect();
    Page { items, next: page.next, total: page.total }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::{metadata, server_with_auth};
    use housecat_protocol::spotify::extendedmetadata as ext;
    use housecat_protocol::spotify::metadata as md;
    use prost::Message;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture file")
    }

    /// A catalog whose pathfinder URL points at the mock.
    fn catalog(server: &MockServer) -> Catalog {
        Catalog::with_metadata(metadata(server)).with_search_url(format!("{}/pathfinder/v2/query", server.uri()))
    }

    #[tokio::test]
    async fn search_posts_the_persisted_query_and_maps_the_answer() {
        let server = server_with_auth().await;
        Mock::given(method("POST"))
            .and(path("/pathfinder/v2/query"))
            .and(body_partial_json(serde_json::json!({
                "operationName": "searchDesktop",
                "variables": { "searchTerm": "boards of canada", "limit": 3 },
                "extensions": { "persistedQuery": { "sha256Hash": consts::SEARCH_DESKTOP_QUERY_HASH } }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(fixture("search.json")))
            .expect(1)
            .mount(&server)
            .await;

        let results =
            catalog(&server).search("boards of canada", SearchKinds::TRACKS | SearchKinds::ARTISTS, 3).await.unwrap();
        assert_eq!(results.tracks.len(), 3);
        assert_eq!(results.artists[0].name, "Boards of Canada");
        assert!(results.albums.is_empty());
    }

    #[tokio::test]
    async fn a_stale_query_hash_fails_loudly() {
        let server = server_with_auth().await;
        Mock::given(method("POST"))
            .and(path("/pathfinder/v2/query"))
            .respond_with(ResponseTemplate::new(412).set_body_string("\"Invalid query hash\""))
            .mount(&server)
            .await;
        let err = catalog(&server).search("x", SearchKinds::ALL, 10).await.unwrap_err();
        assert!(
            matches!(&err, Error::Protocol { what: "search", detail } if detail.contains("SEARCH_DESKTOP_QUERY_HASH")),
            "got {err:?}"
        );
    }

    fn collection_page(items: &[(String, u64)]) -> serde_json::Value {
        serde_json::json!({
            "items": items.iter().map(|(uri, at)| serde_json::json!({ "uri": uri, "added_at": at.to_string() })).collect::<Vec<_>>()
        })
    }

    async fn mount_collection(server: &MockServer, set: &str, items: &[(String, u64)]) {
        Mock::given(method("POST"))
            .and(path("/collection/v2/paging"))
            .and(body_partial_json(serde_json::json!({ "set": set })))
            .respond_with(ResponseTemplate::new(200).set_body_json(collection_page(items)))
            .mount(server)
            .await;
    }

    /// Answers every extended-metadata request with the given entities.
    async fn mount_entities(server: &MockServer, entries: Vec<(SpotifyId, Vec<u8>)>) {
        let body = ext::BatchedExtensionResponse {
            header: None,
            extended_metadata: vec![ext::EntityExtensionDataArray {
                header: None,
                extension_kind: ext::ExtensionKind::TrackV4 as i32,
                extension_data: entries
                    .into_iter()
                    .map(|(id, payload)| ext::EntityExtensionData {
                        header: Some(ext::EntityExtensionDataHeader {
                            status_code: 200,
                            etag: String::new(),
                            locale: String::new(),
                            cache_ttl_in_seconds: 0,
                            offline_ttl_in_seconds: 0,
                        }),
                        entity_uri: id.to_uri(),
                        extension_data: Some(prost_types::Any { type_url: String::new(), value: payload }),
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

    fn track_proto(id: SpotifyId, name: &str) -> Vec<u8> {
        md::Track {
            gid: Some(id.to_raw().to_vec()),
            name: Some(name.to_string()),
            album: Some(md::Album { gid: Some(vec![9; 16]), name: Some("a".to_string()), ..Default::default() }),
            ..Default::default()
        }
        .encode_to_vec()
    }

    fn id(kind: IdKind, byte: u8) -> SpotifyId {
        SpotifyId::from_raw(kind, &[byte; 16]).unwrap()
    }

    #[tokio::test]
    async fn saved_tracks_pages_newest_first_and_hydrates_in_order() {
        let server = server_with_auth().await;
        let old = id(IdKind::Track, 1);
        let newer = id(IdKind::Track, 2);
        let newest = id(IdKind::Track, 3);
        let album = id(IdKind::Album, 4);
        mount_collection(
            &server,
            "collection",
            &[(old.to_uri(), 100), (album.to_uri(), 400), (newest.to_uri(), 300), (newer.to_uri(), 200)],
        )
        .await;
        mount_entities(
            &server,
            vec![(old, track_proto(old, "old")), (newer, track_proto(newer, "newer")), (newest, track_proto(newest, "newest"))],
        )
        .await;

        let catalog = catalog(&server);
        let page = catalog.saved_tracks(Paging::first(2)).await.unwrap();
        assert_eq!(page.total, Some(3), "albums are not tracks");
        assert_eq!(page.items.iter().map(|s| s.item.name.as_str()).collect::<Vec<_>>(), vec!["newest", "newer"]);
        assert_eq!(page.items[0].added_at, std::time::UNIX_EPOCH + std::time::Duration::from_secs(300));
        let next = page.next.expect("a second page");
        let page = catalog.saved_tracks(next).await.unwrap();
        assert_eq!(page.items.iter().map(|s| s.item.name.as_str()).collect::<Vec<_>>(), vec!["old"]);
        assert!(page.next.is_none());

        let ids = catalog.saved_album_ids().await.unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].item, album);
        let collection_requests =
            server.received_requests().await.unwrap().iter().filter(|r| r.url.path() == "/collection/v2/paging").count();
        assert_eq!(collection_requests, 1, "one snapshot served every call");
    }

    #[tokio::test]
    async fn unknown_entities_are_dropped_from_a_page() {
        let server = server_with_auth().await;
        let known = id(IdKind::Track, 1);
        let gone = id(IdKind::Track, 2);
        mount_collection(&server, "collection", &[(known.to_uri(), 100), (gone.to_uri(), 200)]).await;
        mount_entities(&server, vec![(known, track_proto(known, "known"))]).await;
        let page = catalog(&server).saved_tracks(Paging::first(10)).await.unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].item.name, "known");
        assert_eq!(page.total, Some(2));
    }

    #[tokio::test]
    async fn the_other_sets_map_to_their_kinds() {
        let server = server_with_auth().await;
        let artist = id(IdKind::Artist, 5);
        let show = id(IdKind::Show, 6);
        let episode = id(IdKind::Episode, 7);
        mount_collection(&server, "artist", &[(artist.to_uri(), 1)]).await;
        mount_collection(&server, "show", &[(show.to_uri(), 2)]).await;
        mount_collection(&server, "listenlater", &[(episode.to_uri(), 3)]).await;
        let catalog = catalog(&server);
        assert_eq!(catalog.followed_artist_ids().await.unwrap()[0].item, artist);
        assert_eq!(catalog.saved_show_ids().await.unwrap()[0].item, show);
        assert_eq!(catalog.saved_episode_ids().await.unwrap()[0].item, episode);
    }
}
