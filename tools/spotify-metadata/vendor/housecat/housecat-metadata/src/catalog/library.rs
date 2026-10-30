//! The user's library over `/collection/v2/paging` (spec section 6.2): sets of saved
//! things, fetched whole, snapshotted, and paged locally.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use serde::Deserialize;
use tokio::time::Instant;

use housecat_core::spclient::SpClient;
use housecat_core::{consts, Error, Result, SpotifyId};

use crate::model::{Page, Paging, Saved};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LibrarySet {
    /// Saved tracks and saved albums together.
    Collection,
    Artists,
    Shows,
    /// Saved episodes; Spotify calls the set "listen later".
    Episodes,
}

impl LibrarySet {
    pub(crate) fn name(self) -> &'static str {
        match self {
            LibrarySet::Collection => "collection",
            LibrarySet::Artists => "artist",
            LibrarySet::Shows => "show",
            LibrarySet::Episodes => "listenlater",
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PageJson {
    items: Vec<ItemJson>,
    next_page_token: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ItemJson {
    uri: String,
    /// Unix seconds, as a string.
    added_at: String,
}

/// Every item of `set`, newest first. URIs that are not catalog ids (local files) are skipped.
pub(crate) async fn fetch_set(spclient: &SpClient, username: &str, set: LibrarySet) -> Result<Vec<Saved<SpotifyId>>> {
    let mut items = Vec::new();
    let mut token = String::new();
    loop {
        let body = serde_json::json!({
            "username": username,
            "set": set.name(),
            "paginationToken": token,
            "limit": consts::COLLECTION_PAGE_LIMIT,
        })
        .to_string();
        let bytes = spclient.post_json(consts::COLLECTION_PATH, body).await?;
        let page: PageJson =
            serde_json::from_slice(&bytes).map_err(|e| Error::protocol("collection", format!("unexpected json: {e}")))?;
        let received = page.items.len();
        for item in page.items {
            let Ok(id) = SpotifyId::from_uri(&item.uri) else { continue };
            let seconds: u64 = item.added_at.trim().parse().unwrap_or(0);
            items.push(Saved { item: id, added_at: UNIX_EPOCH + Duration::from_secs(seconds) });
        }
        match page.next_page_token {
            Some(next) if !next.is_empty() && received > 0 => token = next,
            _ => break,
        }
    }
    items.sort_by_key(|saved| std::cmp::Reverse(saved.added_at));
    Ok(items)
}

/// The window `paging` of `items`, with `next` when more follow.
pub(crate) fn page_of<T: Clone>(items: &[T], paging: Paging) -> Page<T> {
    let total = items.len();
    let start = (paging.offset as usize).min(total);
    let end = start.saturating_add(paging.limit as usize).min(total);
    let next = (end < total && paging.limit > 0).then_some(Paging { offset: end as u32, limit: paging.limit });
    Page { items: items[start..end].to_vec(), next, total: Some(total as u32) }
}

/// A fetched set, shared between the pages served from it.
type SetItems = Arc<Vec<Saved<SpotifyId>>>;

/// Recently fetched sets, so paging through the library does not refetch it every page.
pub(crate) struct Snapshots {
    ttl: Duration,
    sets: Mutex<HashMap<LibrarySet, (Instant, SetItems)>>,
}

impl Snapshots {
    pub(crate) fn new(ttl: Duration) -> Self {
        Snapshots { ttl, sets: Mutex::new(HashMap::new()) }
    }

    pub(crate) async fn get(
        &self,
        set: LibrarySet,
        fetch: impl Future<Output = Result<Vec<Saved<SpotifyId>>>>,
    ) -> Result<Arc<Vec<Saved<SpotifyId>>>> {
        let cached = self
            .sets
            .lock()
            .expect("snapshots lock")
            .get(&set)
            .filter(|(at, _)| at.elapsed() < self.ttl)
            .map(|(_, items)| items.clone());
        if let Some(items) = cached {
            return Ok(items);
        }
        let items = Arc::new(fetch.await?);
        self.sets.lock().expect("snapshots lock").insert(set, (Instant::now(), items.clone()));
        Ok(items)
    }

    pub(crate) fn invalidate(&self) {
        self.sets.lock().expect("snapshots lock").clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::{server_with_auth, spclient};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn item(uri: &str, added_at: u64) -> serde_json::Value {
        serde_json::json!({ "uri": uri, "added_at": added_at.to_string() })
    }

    #[tokio::test]
    async fn fetch_set_walks_pages_sorts_newest_first_and_skips_junk() {
        let server: MockServer = server_with_auth().await;
        Mock::given(method("POST"))
            .and(path("/collection/v2/paging"))
            .and(body_partial_json(serde_json::json!({ "username": "alice", "set": "collection", "paginationToken": "" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [
                    item("spotify:album:0K5ljicneNOkdgx2rNxYBv", 1_702_670_966),
                    item("spotify:track:4PTG3Z6ehGkBFwjybzWkR8", 1_789_520_075),
                    item("spotify:local:x:y:z:1", 1_800_000_000),
                ],
                "next_page_token": "tok-2"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/collection/v2/paging"))
            .and(body_partial_json(serde_json::json!({ "set": "collection", "paginationToken": "tok-2" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [item("spotify:track:1arAXEEsM5yVp3cFrDRuZ6", 1_750_000_000)],
                "sync_token": "s"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let items = fetch_set(&spclient(&server), "alice", LibrarySet::Collection).await.unwrap();
        let uris: Vec<String> = items.iter().map(|s| s.item.to_uri()).collect();
        assert_eq!(
            uris,
            vec![
                "spotify:track:4PTG3Z6ehGkBFwjybzWkR8",
                "spotify:track:1arAXEEsM5yVp3cFrDRuZ6",
                "spotify:album:0K5ljicneNOkdgx2rNxYBv",
            ]
        );
        assert_eq!(items[0].added_at, UNIX_EPOCH + Duration::from_secs(1_789_520_075));
        let requests = server.received_requests().await.unwrap();
        let first = requests.iter().find(|r| r.url.path() == "/collection/v2/paging").unwrap();
        let body: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
        assert_eq!(body["limit"], consts::COLLECTION_PAGE_LIMIT);
    }

    #[test]
    fn sets_have_spotify_names() {
        assert_eq!(LibrarySet::Collection.name(), "collection");
        assert_eq!(LibrarySet::Artists.name(), "artist");
        assert_eq!(LibrarySet::Shows.name(), "show");
        assert_eq!(LibrarySet::Episodes.name(), "listenlater");
    }

    #[tokio::test]
    async fn a_bad_answer_is_a_protocol_error() {
        let server = server_with_auth().await;
        Mock::given(method("POST"))
            .and(path("/collection/v2/paging"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
            .mount(&server)
            .await;
        let err = fetch_set(&spclient(&server), "alice", LibrarySet::Artists).await.unwrap_err();
        assert!(matches!(err, Error::Protocol { what: "collection", .. }), "got {err:?}");
    }

    #[test]
    fn page_of_slices_and_links() {
        let items: Vec<u32> = (0..7).collect();
        let first = page_of(&items, Paging::first(3));
        assert_eq!(first.items, vec![0, 1, 2]);
        assert_eq!(first.next, Some(Paging { offset: 3, limit: 3 }));
        assert_eq!(first.total, Some(7));
        let last = page_of(&items, Paging { offset: 6, limit: 3 });
        assert_eq!(last.items, vec![6]);
        assert_eq!(last.next, None);
        let beyond = page_of(&items, Paging { offset: 20, limit: 3 });
        assert!(beyond.items.is_empty());
        assert_eq!(beyond.next, None);
        assert_eq!(page_of(&items, Paging { offset: 0, limit: 0 }).items.len(), 0);
    }

    #[tokio::test]
    async fn snapshots_cache_per_set_until_the_ttl_and_invalidate() {
        tokio::time::pause();
        let snapshots = Snapshots::new(Duration::from_secs(30));
        let calls = Arc::new(AtomicUsize::new(0));
        let fetch = |calls: Arc<AtomicUsize>| async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![Saved { item: SpotifyId::from_uri("spotify:track:4PTG3Z6ehGkBFwjybzWkR8").unwrap(), added_at: UNIX_EPOCH }])
        };
        let a = snapshots.get(LibrarySet::Collection, fetch(calls.clone())).await.unwrap();
        let b = snapshots.get(LibrarySet::Collection, fetch(calls.clone())).await.unwrap();
        assert!(Arc::ptr_eq(&a, &b), "served from the snapshot");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        snapshots.get(LibrarySet::Artists, fetch(calls.clone())).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "sets are cached separately");
        tokio::time::advance(Duration::from_secs(31)).await;
        snapshots.get(LibrarySet::Collection, fetch(calls.clone())).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3, "expired");
        snapshots.invalidate();
        snapshots.get(LibrarySet::Collection, fetch(calls.clone())).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 4, "invalidated");
    }
}
