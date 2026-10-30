//! The plain data model every transport maps into (spec section 6.1).
//!
//! Nothing here knows about protobuf or JSON. Fields only one transport provides are
//! `Option`.

use std::fmt;
use std::time::{Duration, SystemTime};

use housecat_core::{FileId, SpotifyId};
use serde::Serialize;

pub const IMAGE_CDN: &str = "https://i.scdn.co/image/";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Image {
    pub id: FileId,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Image {
    pub fn url(&self) -> String {
        format!("{IMAGE_CDN}{}", self.id.to_hex())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ReleaseDate {
    pub year: i32,
    pub month: Option<u8>,
    pub day: Option<u8>,
}

impl fmt::Display for ReleaseDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.month, self.day) {
            (Some(month), Some(day)) => write!(f, "{}-{month:02}-{day:02}", self.year),
            (Some(month), None) => write!(f, "{}-{month:02}", self.year),
            _ => write!(f, "{}", self.year),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Availability {
    Available,
    Restricted { reason: String },
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum AlbumKind {
    Album,
    Single,
    Compilation,
    Ep,
    Audiobook,
    Podcast,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArtistRef {
    pub id: SpotifyId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AlbumRef {
    pub id: SpotifyId,
    pub name: String,
    pub images: Vec<Image>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TrackRef {
    pub id: SpotifyId,
    pub name: String,
    pub duration: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ShowRef {
    pub id: SpotifyId,
    pub name: String,
}

/// An episode as a show lists it. Spotify's show listing carries only the id and the
/// release date, so `name` is empty and `duration` is `None` there; fetch the episodes
/// for the rest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EpisodeRef {
    pub id: SpotifyId,
    pub name: String,
    pub duration: Option<Duration>,
    pub release_date: Option<ReleaseDate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlaylistRef {
    pub id: SpotifyId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Track {
    pub id: SpotifyId,
    pub name: String,
    pub artists: Vec<ArtistRef>,
    pub album: AlbumRef,
    pub duration: Duration,
    pub disc_number: u32,
    pub track_number: u32,
    pub explicit: bool,
    pub popularity: Option<u8>,
    pub availability: Availability,
    /// Other track ids Spotify offers when this one is unavailable in the account's country.
    pub alternatives: Vec<SpotifyId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Album {
    pub id: SpotifyId,
    pub name: String,
    pub artists: Vec<ArtistRef>,
    pub release_date: Option<ReleaseDate>,
    pub images: Vec<Image>,
    /// One inner list per disc, in disc order.
    pub discs: Vec<Vec<TrackRef>>,
    pub kind: AlbumKind,
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Artist {
    pub id: SpotifyId,
    pub name: String,
    pub images: Vec<Image>,
    pub popularity: Option<u8>,
    /// Top tracks for the session country; `None` when the transport does not provide them.
    pub top_tracks: Option<Vec<TrackRef>>,
    pub albums: Option<Vec<AlbumRef>>,
    pub singles: Option<Vec<AlbumRef>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Show {
    pub id: SpotifyId,
    pub name: String,
    pub publisher: String,
    pub description: String,
    pub images: Vec<Image>,
    pub episodes: Vec<EpisodeRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Episode {
    pub id: SpotifyId,
    pub name: String,
    pub show: ShowRef,
    pub duration: Duration,
    pub release_date: Option<ReleaseDate>,
    pub description: String,
    pub images: Vec<Image>,
    pub explicit: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlaylistItem {
    pub id: SpotifyId,
    pub added_at: Option<SystemTime>,
    pub added_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Playlist {
    pub id: SpotifyId,
    pub name: String,
    pub description: String,
    pub owner: String,
    pub image: Option<Image>,
    pub collaborative: bool,
    /// Hex of the server's revision bytes; changes whenever the playlist changes.
    pub revision: String,
    /// The total number of items the playlist holds.
    pub length: u32,
    pub items: Vec<PlaylistItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RootlistEntry {
    pub playlist: PlaylistRef,
    /// Folder names from the root down; empty for a playlist at the top level.
    pub folder_path: Vec<String>,
    pub length: Option<u32>,
    pub owner: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LyricLine {
    pub start: Duration,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Lyrics {
    /// True when every line carries a start time.
    pub synced: bool,
    pub lines: Vec<LyricLine>,
    pub language: Option<String>,
    pub provider: Option<String>,
}

/// A window into a list: `offset` items skipped, at most `limit` returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Paging {
    pub offset: u32,
    pub limit: u32,
}

impl Paging {
    pub fn first(limit: u32) -> Self {
        Paging { offset: 0, limit }
    }

    /// The window right after this one.
    pub fn next(self) -> Self {
        Paging { offset: self.offset.saturating_add(self.limit), limit: self.limit }
    }
}

/// One page of results; pass `next` straight back to get the following page.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<Paging>,
    pub total: Option<u32>,
}

/// A library entry: what was saved and when.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Saved<T> {
    pub item: T,
    pub added_at: SystemTime,
}

/// Which result kinds a search should map. A bitset; combine with `|`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct SearchKinds(u8);

impl SearchKinds {
    pub const NONE: SearchKinds = SearchKinds(0);
    pub const TRACKS: SearchKinds = SearchKinds(1);
    pub const ALBUMS: SearchKinds = SearchKinds(2);
    pub const ARTISTS: SearchKinds = SearchKinds(4);
    pub const PLAYLISTS: SearchKinds = SearchKinds(8);
    pub const SHOWS: SearchKinds = SearchKinds(16);
    pub const EPISODES: SearchKinds = SearchKinds(32);
    pub const ALL: SearchKinds = SearchKinds(63);

    pub fn contains(self, other: SearchKinds) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for SearchKinds {
    type Output = SearchKinds;
    fn bitor(self, rhs: SearchKinds) -> SearchKinds {
        SearchKinds(self.0 | rhs.0)
    }
}

/// Spotify's hit counts per kind, when it reports them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SearchTotals {
    pub tracks: Option<u32>,
    pub albums: Option<u32>,
    pub artists: Option<u32>,
    pub playlists: Option<u32>,
    pub shows: Option<u32>,
    pub episodes: Option<u32>,
}

/// What search knows about a playlist: no items, no revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlaylistSummary {
    pub id: SpotifyId,
    pub name: String,
    pub description: String,
    /// The owner's username.
    pub owner: String,
    pub owner_name: Option<String>,
    pub image: Option<Image>,
}

/// Search hits mapped into the model. Tracks carry no disc or track number (0) and no
/// popularity; albums carry no discs or label; artists carry no top tracks; shows carry no
/// description or episodes. Fetch the entity for the rest.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub playlists: Vec<PlaylistSummary>,
    pub shows: Vec<Show>,
    pub episodes: Vec<Episode>,
    pub totals: SearchTotals,
}

impl Image {
    /// From a CDN URL such as `https://i.scdn.co/image/<40 hex>`; other hosts give `None`.
    pub fn from_url(url: &str, width: Option<u32>, height: Option<u32>) -> Option<Image> {
        let hex = url.strip_prefix(IMAGE_CDN)?;
        let id = FileId::from_hex(hex).ok()?;
        Some(Image { id, width, height })
    }
}

impl ReleaseDate {
    /// `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, or a full ISO 8601 timestamp.
    pub fn from_iso(text: &str) -> Option<ReleaseDate> {
        let date = text.split('T').next()?;
        let mut parts = date.split('-');
        let year: i32 = parts.next()?.parse().ok()?;
        let month = parts.next().and_then(|m| m.parse::<u8>().ok()).filter(|m| (1..=12).contains(m));
        let day = parts.next().and_then(|d| d.parse::<u8>().ok()).filter(|d| (1..=31).contains(d));
        Some(ReleaseDate { year, month, day })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_from_cdn_url_only() {
        let hex = "ab67616d00001e029effb26eea559ce6f3fed751";
        let image = Image::from_url(&format!("https://i.scdn.co/image/{hex}"), Some(300), Some(300)).unwrap();
        assert_eq!(image.id.to_hex(), hex);
        assert_eq!(image.width, Some(300));
        assert!(Image::from_url("https://pickasso.spotifycdn.com/image/x/dt/v1/img/radio/artist/y/en", None, None).is_none());
        assert!(Image::from_url("https://i.scdn.co/image/short", None, None).is_none());
    }

    #[test]
    fn release_date_from_iso() {
        assert_eq!(ReleaseDate::from_iso("2026-07-10T11:00:00Z"), Some(ReleaseDate { year: 2026, month: Some(7), day: Some(10) }));
        assert_eq!(ReleaseDate::from_iso("1998-04"), Some(ReleaseDate { year: 1998, month: Some(4), day: None }));
        assert_eq!(ReleaseDate::from_iso("1998"), Some(ReleaseDate { year: 1998, month: None, day: None }));
        assert_eq!(ReleaseDate::from_iso("soon"), None);
    }

    #[test]
    fn paging_and_search_kinds() {
        let first = Paging::first(50);
        assert_eq!(first, Paging { offset: 0, limit: 50 });
        assert_eq!(first.next(), Paging { offset: 50, limit: 50 });
        let kinds = SearchKinds::TRACKS | SearchKinds::ALBUMS;
        assert!(kinds.contains(SearchKinds::TRACKS));
        assert!(!kinds.contains(SearchKinds::SHOWS));
        assert!(SearchKinds::ALL.contains(SearchKinds::EPISODES));
        assert!(!SearchKinds::NONE.contains(SearchKinds::TRACKS));
    }

    #[test]
    fn image_url_is_the_cdn_path_with_the_hex_id() {
        let image = Image { id: FileId([0xab; 20]), width: Some(300), height: None };
        assert_eq!(image.url(), format!("https://i.scdn.co/image/{}", "ab".repeat(20)));
    }

    #[test]
    fn release_date_formats_what_it_knows() {
        assert_eq!(ReleaseDate { year: 1987, month: Some(11), day: Some(16) }.to_string(), "1987-11-16");
        assert_eq!(ReleaseDate { year: 1987, month: Some(11), day: None }.to_string(), "1987-11");
        assert_eq!(ReleaseDate { year: 1987, month: None, day: Some(3) }.to_string(), "1987");
    }
}
