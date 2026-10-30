//! The desktop client's `searchDesktop` persisted query on pathfinder (spec section 6.2):
//! the request body and the mapping of its JSON into the model.

use std::time::Duration;

use serde::Deserialize;

use housecat_core::{consts, Error, IdKind, Result, SpotifyId};

use crate::model::{
    Album, AlbumKind, AlbumRef, Artist, ArtistRef, Availability, Episode, Image, PlaylistSummary, ReleaseDate,
    SearchKinds, SearchResults, SearchTotals, Show, ShowRef, Track,
};

pub(crate) fn request_body(query: &str, limit: u32, offset: u32) -> String {
    serde_json::json!({
        "operationName": "searchDesktop",
        "variables": {
            "searchTerm": query,
            "offset": offset,
            "limit": limit,
            "numberOfTopResults": 5,
            "includeAudiobooks": false,
            "includeArtistHasConcertsField": false,
            "includePreReleases": false,
            "includeLocalConcertsField": false,
            "includeAuthors": false
        },
        "extensions": { "persistedQuery": { "version": 1, "sha256Hash": consts::SEARCH_DESKTOP_QUERY_HASH } }
    })
    .to_string()
}

// --- the slice of the response housecat reads; everything is optional and defaulted ---

#[derive(Deserialize, Default)]
#[serde(default)]
struct Envelope {
    data: Option<Data>,
    errors: Vec<GraphError>,
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct GraphError {
    message: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Data {
    search_v2: Search,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Search {
    tracks_v2: Section<TrackItem>,
    albums_v2: Section<Wrapper<AlbumData>>,
    artists: Section<Wrapper<ArtistData>>,
    playlists: Section<Wrapper<PlaylistData>>,
    podcasts: Section<Wrapper<PodcastData>>,
    episodes: Section<Wrapper<EpisodeData>>,
}

#[derive(Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Section<T> {
    total_count: Option<u32>,
    items: Vec<T>,
}

impl<T> Default for Section<T> {
    fn default() -> Self {
        Section { total_count: None, items: Vec::new() }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TrackItem {
    item: Wrapper<TrackData>,
}

/// `{"__typename": ..., "data": {...}}`; a `GenericError` item carries no usable data and
/// deserializes to defaults, which the mappers then drop for lack of a URI.
#[derive(Deserialize)]
#[serde(default)]
struct Wrapper<T> {
    data: Option<T>,
}

impl<T> Default for Wrapper<T> {
    fn default() -> Self {
        Wrapper { data: None }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct TrackData {
    uri: String,
    name: String,
    duration: DurationMs,
    content_rating: ContentRating,
    playability: Playability,
    album_of_track: AlbumRefData,
    artists: Items<ArtistRefData>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DurationMs {
    total_milliseconds: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ContentRating {
    label: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Playability {
    playable: Option<bool>,
    reason: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct AlbumRefData {
    uri: String,
    name: String,
    cover_art: CoverArt,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CoverArt {
    sources: Vec<Source>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Source {
    url: String,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Deserialize)]
#[serde(default)]
struct Items<T> {
    items: Vec<T>,
}

impl<T> Default for Items<T> {
    fn default() -> Self {
        Items { items: Vec::new() }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ArtistRefData {
    uri: String,
    profile: Profile,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Profile {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct AlbumData {
    uri: String,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    date: DateYear,
    artists: Items<ArtistRefData>,
    cover_art: CoverArt,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DateYear {
    year: Option<i32>,
    month: Option<u8>,
    day: Option<u8>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ArtistData {
    uri: String,
    profile: Profile,
    visuals: Visuals,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Visuals {
    avatar_image: CoverArt,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PlaylistData {
    uri: String,
    name: String,
    description: String,
    owner_v2: Wrapper<OwnerData>,
    images: Items<CoverArt>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OwnerData {
    name: String,
    username: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PodcastData {
    uri: String,
    name: String,
    publisher: Publisher,
    cover_art: CoverArt,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Publisher {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct EpisodeData {
    uri: String,
    name: String,
    description: String,
    duration: DurationMs,
    release_date: IsoDate,
    content_rating: ContentRating,
    cover_art: CoverArt,
    podcast_v2: Wrapper<PodcastData>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct IsoDate {
    iso_string: String,
}

// --- mapping ---

fn images(art: &CoverArt) -> Vec<Image> {
    art.sources.iter().filter_map(|s| Image::from_url(&s.url, s.width, s.height)).collect()
}

fn artist_refs(items: &Items<ArtistRefData>) -> Vec<ArtistRef> {
    items
        .items
        .iter()
        .filter_map(|a| Some(ArtistRef { id: SpotifyId::from_uri(&a.uri).ok()?, name: a.profile.name.clone() }))
        .collect()
}

fn availability(playability: &Playability) -> Availability {
    match (playability.playable, playability.reason.as_deref()) {
        (Some(true), _) | (None, Some("PLAYABLE")) => Availability::Available,
        (Some(false), reason) => Availability::Restricted { reason: reason.unwrap_or("not playable").to_string() },
        (None, Some(reason)) => Availability::Restricted { reason: reason.to_string() },
        (None, None) => Availability::Unknown,
    }
}

fn track(data: &TrackData) -> Option<Track> {
    let id = SpotifyId::from_uri(&data.uri).ok().filter(|id| id.kind == IdKind::Track)?;
    let album = AlbumRef {
        id: SpotifyId::from_uri(&data.album_of_track.uri).ok()?,
        name: data.album_of_track.name.clone(),
        images: images(&data.album_of_track.cover_art),
    };
    Some(Track {
        id,
        name: data.name.clone(),
        artists: artist_refs(&data.artists),
        album,
        duration: Duration::from_millis(data.duration.total_milliseconds),
        disc_number: 0,
        track_number: 0,
        explicit: data.content_rating.label == "EXPLICIT",
        popularity: None,
        availability: availability(&data.playability),
        alternatives: Vec::new(),
    })
}

fn album_kind(kind: &str) -> AlbumKind {
    match kind {
        "ALBUM" => AlbumKind::Album,
        "SINGLE" => AlbumKind::Single,
        "EP" => AlbumKind::Ep,
        "COMPILATION" => AlbumKind::Compilation,
        "AUDIOBOOK" => AlbumKind::Audiobook,
        "PODCAST" => AlbumKind::Podcast,
        _ => AlbumKind::Unknown,
    }
}

fn album(data: &AlbumData) -> Option<Album> {
    Some(Album {
        id: SpotifyId::from_uri(&data.uri).ok().filter(|id| id.kind == IdKind::Album)?,
        name: data.name.clone(),
        artists: artist_refs(&data.artists),
        release_date: data.date.year.map(|year| ReleaseDate { year, month: data.date.month, day: data.date.day }),
        images: images(&data.cover_art),
        discs: Vec::new(),
        kind: album_kind(&data.kind),
        label: None,
    })
}

fn artist(data: &ArtistData) -> Option<Artist> {
    Some(Artist {
        id: SpotifyId::from_uri(&data.uri).ok().filter(|id| id.kind == IdKind::Artist)?,
        name: data.profile.name.clone(),
        images: images(&data.visuals.avatar_image),
        popularity: None,
        top_tracks: None,
        albums: None,
        singles: None,
    })
}

fn playlist(data: &PlaylistData) -> Option<PlaylistSummary> {
    let owner = data.owner_v2.data.as_ref();
    Some(PlaylistSummary {
        id: SpotifyId::from_uri(&data.uri).ok().filter(|id| id.kind == IdKind::Playlist)?,
        name: data.name.clone(),
        description: data.description.clone(),
        owner: owner.map(|o| o.username.clone()).unwrap_or_default(),
        owner_name: owner.map(|o| o.name.clone()).filter(|n| !n.is_empty()),
        image: data.images.items.iter().flat_map(images).next(),
    })
}

fn show(data: &PodcastData) -> Option<Show> {
    Some(Show {
        id: SpotifyId::from_uri(&data.uri).ok().filter(|id| id.kind == IdKind::Show)?,
        name: data.name.clone(),
        publisher: data.publisher.name.clone(),
        description: String::new(),
        images: images(&data.cover_art),
        episodes: Vec::new(),
    })
}

fn episode(data: &EpisodeData) -> Option<Episode> {
    let podcast = data.podcast_v2.data.as_ref()?;
    Some(Episode {
        id: SpotifyId::from_uri(&data.uri).ok().filter(|id| id.kind == IdKind::Episode)?,
        name: data.name.clone(),
        show: ShowRef { id: SpotifyId::from_uri(&podcast.uri).ok()?, name: podcast.name.clone() },
        duration: Duration::from_millis(data.duration.total_milliseconds),
        release_date: ReleaseDate::from_iso(&data.release_date.iso_string),
        description: data.description.clone(),
        images: images(&data.cover_art),
        explicit: data.content_rating.label == "EXPLICIT",
    })
}

/// Map the response; only the kinds asked for are mapped, the rest stay empty. Items that
/// are error placeholders or carry no catalog URI are dropped.
pub(crate) fn parse(json: &[u8], kinds: SearchKinds) -> Result<SearchResults> {
    let envelope: Envelope =
        serde_json::from_slice(json).map_err(|e| Error::protocol("search", format!("unexpected json: {e}")))?;
    let Some(data) = envelope.data else {
        let messages: Vec<String> = envelope.errors.into_iter().map(|e| e.message).collect();
        return Err(Error::protocol("search", format!("no data; errors: {}", messages.join("; "))));
    };
    if !envelope.errors.is_empty() {
        tracing::warn!(errors = ?envelope.errors, "search answered with partial errors");
    }
    let search = data.search_v2;
    let mut results = SearchResults {
        totals: SearchTotals {
            tracks: search.tracks_v2.total_count,
            albums: search.albums_v2.total_count,
            artists: search.artists.total_count,
            playlists: search.playlists.total_count,
            shows: search.podcasts.total_count,
            episodes: search.episodes.total_count,
        },
        ..Default::default()
    };
    if kinds.contains(SearchKinds::TRACKS) {
        results.tracks = search.tracks_v2.items.iter().filter_map(|i| i.item.data.as_ref()).filter_map(track).collect();
    }
    if kinds.contains(SearchKinds::ALBUMS) {
        results.albums = search.albums_v2.items.iter().filter_map(|w| w.data.as_ref()).filter_map(album).collect();
    }
    if kinds.contains(SearchKinds::ARTISTS) {
        results.artists = search.artists.items.iter().filter_map(|w| w.data.as_ref()).filter_map(artist).collect();
    }
    if kinds.contains(SearchKinds::PLAYLISTS) {
        results.playlists = search.playlists.items.iter().filter_map(|w| w.data.as_ref()).filter_map(playlist).collect();
    }
    if kinds.contains(SearchKinds::SHOWS) {
        results.shows = search.podcasts.items.iter().filter_map(|w| w.data.as_ref()).filter_map(show).collect();
    }
    if kinds.contains(SearchKinds::EPISODES) {
        results.episodes = search.episodes.items.iter().filter_map(|w| w.data.as_ref()).filter_map(episode).collect();
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/search.json", env!("CARGO_MANIFEST_DIR"))).expect("fixture file")
    }

    #[test]
    fn request_body_names_the_persisted_query() {
        let body: serde_json::Value = serde_json::from_str(&request_body("boards of canada", 10, 20)).unwrap();
        assert_eq!(body["operationName"], "searchDesktop");
        assert_eq!(body["variables"]["searchTerm"], "boards of canada");
        assert_eq!(body["variables"]["limit"], 10);
        assert_eq!(body["variables"]["offset"], 20);
        assert_eq!(body["extensions"]["persistedQuery"]["sha256Hash"], consts::SEARCH_DESKTOP_QUERY_HASH);
        assert_eq!(body["extensions"]["persistedQuery"]["version"], 1);
    }

    #[test]
    fn tracks_map_with_album_artists_and_playability() {
        let results = parse(&fixture(), SearchKinds::ALL).unwrap();
        assert_eq!(results.tracks.len(), 3);
        let track = &results.tracks[0];
        assert_eq!(track.id.to_uri(), "spotify:track:5Hf2h59YLInKlic7ooWZVd");
        assert_eq!(track.name, "Roygbiv");
        assert_eq!(track.duration, Duration::from_millis(149_080));
        assert_eq!(track.artists.len(), 1);
        assert_eq!(track.artists[0].name, "Boards of Canada");
        assert_eq!(track.artists[0].id.to_uri(), "spotify:artist:2VAvhf61GgLYmC6C8anyX1");
        assert_eq!(track.album.id.to_uri(), "spotify:album:6LZiNXaDvhzvnXUubVOmNU");
        assert_eq!(track.album.name, "Music Has The Right To Children");
        assert_eq!(track.album.images.len(), 3);
        assert!(track.album.images.iter().any(|i| i.width == Some(640)));
        assert_eq!(track.availability, Availability::Available);
        assert!(!track.explicit);
        assert_eq!(track.disc_number, 0);
        assert_eq!(track.popularity, None);
        assert!(results.totals.tracks.is_some_and(|t| t >= 100));
    }

    #[test]
    fn albums_artists_playlists_shows_and_episodes_map() {
        let results = parse(&fixture(), SearchKinds::ALL).unwrap();
        let album = &results.albums[0];
        assert_eq!(album.id.to_uri(), "spotify:album:0mu3EvWYfNwBfISSg0q03p");
        assert_eq!(album.name, "Inferno");
        assert_eq!(album.kind, AlbumKind::Album);
        assert_eq!(album.release_date.map(|d| d.year), Some(2026));
        assert_eq!(album.artists[0].name, "Boards of Canada");
        assert_eq!(album.images.len(), 3);
        assert!(album.discs.is_empty());

        let artist = &results.artists[0];
        assert_eq!(artist.id.to_uri(), "spotify:artist:2VAvhf61GgLYmC6C8anyX1");
        assert_eq!(artist.name, "Boards of Canada");
        assert_eq!(artist.images.len(), 3);
        assert!(artist.top_tracks.is_none());

        let playlist = &results.playlists[0];
        assert_eq!(playlist.id.to_uri(), "spotify:playlist:37i9dQZF1E4uAm8CRhXdEq");
        assert_eq!(playlist.name, "Boards of Canada Radio");
        assert_eq!(playlist.owner, "spotify");
        assert_eq!(playlist.owner_name.as_deref(), Some("Spotify"));
        assert!(playlist.image.is_none(), "radio covers are not CDN images");

        // The fixture's first podcast hit is a `GenericError` placeholder, which is dropped.
        assert_eq!(results.shows.len(), 2);
        let show = &results.shows[0];
        assert_eq!(show.id.to_uri(), "spotify:show:0PGWvSyTvOPW0fNRIIECFG");
        assert_eq!(show.name, "Unspookable");
        assert!(show.publisher.starts_with("Soundsington Media"));
        assert_eq!(show.images.len(), 3);
        assert!(show.episodes.is_empty());

        let episode = &results.episodes[0];
        assert_eq!(episode.id.to_uri(), "spotify:episode:0vaTSD6UvNSakCmCIZoS3Q");
        assert_eq!(episode.name, "The Inferno Discussion");
        assert_eq!(episode.duration, Duration::from_millis(2_490_834));
        assert_eq!(episode.show.id.to_uri(), "spotify:show:0d02T7qZ0HPfOIpzza7Zc1");
        assert_eq!(episode.show.name, "Night Clerk Radio: Haunted Music Reviews");
        assert_eq!(episode.release_date.map(|d| d.to_string()), Some("2026-06-29".to_string()));
        assert!(!episode.description.is_empty());
        assert!(episode.explicit);
        assert_eq!(episode.images.len(), 3);
    }

    #[test]
    fn kinds_filter_what_is_mapped() {
        let results = parse(&fixture(), SearchKinds::ARTISTS | SearchKinds::SHOWS).unwrap();
        assert!(results.tracks.is_empty());
        assert!(results.albums.is_empty());
        assert_eq!(results.artists.len(), 3);
        assert_eq!(results.shows.len(), 2);
        assert!(results.episodes.is_empty());
        assert!(results.totals.artists.is_some());
        assert!(results.totals.tracks.is_some(), "totals are reported for every kind");
    }

    #[test]
    fn graphql_errors_without_data_are_protocol_errors() {
        let err = parse(br#"{"errors":[{"message":"PersistedQueryNotFound"}]}"#, SearchKinds::ALL).unwrap_err();
        assert!(matches!(&err, Error::Protocol { what: "search", detail } if detail.contains("PersistedQueryNotFound")), "got {err:?}");
        assert!(matches!(parse(b"nope", SearchKinds::ALL), Err(Error::Protocol { .. })));
    }
}
