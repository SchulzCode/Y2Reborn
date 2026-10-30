//! Protobuf entities to the plain model (spec section 6.1).

use std::time::Duration;

use housecat_core::{FileId, IdKind, SpotifyId};
use housecat_protocol::spotify::metadata as md;

use crate::model::{
    Album, AlbumKind, AlbumRef, Artist, ArtistRef, Availability, Episode, EpisodeRef, Image, ReleaseDate, Show,
    ShowRef, Track, TrackRef,
};

const PREMIUM_CATALOGUE: &str = "premium";

fn id(kind: IdKind, gid: Option<&[u8]>) -> Option<SpotifyId> {
    SpotifyId::from_raw(kind, gid?).ok()
}

fn millis(ms: Option<i32>) -> Duration {
    Duration::from_millis(u64::try_from(ms.unwrap_or(0)).unwrap_or(0))
}

fn popularity(value: Option<i32>) -> Option<u8> {
    value.and_then(|v| u8::try_from(v).ok())
}

fn image(image: &md::Image) -> Option<Image> {
    Some(Image {
        id: FileId::from_slice(image.file_id.as_deref()?).ok()?,
        width: image.width.and_then(|w| u32::try_from(w).ok()),
        height: image.height.and_then(|h| u32::try_from(h).ok()),
    })
}

/// The grouped images when present, else the flat legacy list.
fn images(group: Option<&md::ImageGroup>, flat: &[md::Image]) -> Vec<Image> {
    let source = group.map(|g| g.image.as_slice()).filter(|g| !g.is_empty()).unwrap_or(flat);
    source.iter().filter_map(image).collect()
}

fn release_date(date: Option<&md::Date>) -> Option<ReleaseDate> {
    let date = date?;
    Some(ReleaseDate {
        year: date.year?,
        month: date.month.and_then(|m| u8::try_from(m).ok()).filter(|m| (1..=12).contains(m)),
        day: date.day.and_then(|d| u8::try_from(d).ok()).filter(|d| (1..=31).contains(d)),
    })
}

fn country_in(list: &str, country: &str) -> bool {
    list.as_bytes().chunks(2).any(|code| code.eq_ignore_ascii_case(country.as_bytes()))
}

/// Spec section 6.1: computed from the entity's restrictions against the session country.
/// A restriction applies when its catalogue list is empty or names `premium`; an
/// applicable allow list without the country, or a forbid list with it, restricts the entity.
pub(crate) fn availability(restrictions: &[md::Restriction], country: &str) -> Availability {
    for restriction in restrictions {
        let applies = restriction.catalogue_str.is_empty()
            || restriction.catalogue_str.iter().any(|c| c.eq_ignore_ascii_case(PREMIUM_CATALOGUE));
        if !applies {
            continue;
        }
        match &restriction.country_restriction {
            Some(md::restriction::CountryRestriction::CountriesAllowed(allowed)) if !country_in(allowed, country) => {
                return Availability::Restricted { reason: format!("not available in {country}") };
            }
            Some(md::restriction::CountryRestriction::CountriesForbidden(forbidden)) if country_in(forbidden, country) => {
                return Availability::Restricted { reason: format!("blocked in {country}") };
            }
            _ => {}
        }
    }
    Availability::Available
}

pub(crate) fn artist_ref(artist: &md::Artist) -> Option<ArtistRef> {
    Some(ArtistRef { id: id(IdKind::Artist, artist.gid.as_deref())?, name: artist.name.clone().unwrap_or_default() })
}

pub(crate) fn album_ref(album: &md::Album) -> Option<AlbumRef> {
    Some(AlbumRef {
        id: id(IdKind::Album, album.gid.as_deref())?,
        name: album.name.clone().unwrap_or_default(),
        images: images(album.cover_group.as_ref(), &album.cover),
    })
}

pub(crate) fn track_ref(track: &md::Track) -> Option<TrackRef> {
    Some(TrackRef {
        id: id(IdKind::Track, track.gid.as_deref())?,
        name: track.name.clone().unwrap_or_default(),
        duration: millis(track.duration),
    })
}

pub(crate) fn show_ref(show: &md::Show) -> Option<ShowRef> {
    Some(ShowRef { id: id(IdKind::Show, show.gid.as_deref())?, name: show.name.clone().unwrap_or_default() })
}

pub(crate) fn episode_ref(episode: &md::Episode) -> Option<EpisodeRef> {
    Some(EpisodeRef {
        id: id(IdKind::Episode, episode.gid.as_deref())?,
        name: episode.name.clone().unwrap_or_default(),
        duration: episode.duration.map(|d| millis(Some(d))),
        release_date: release_date(episode.publish_time.as_ref()),
    })
}

/// `None` when the track, or its album, has no usable gid.
pub(crate) fn track(track: &md::Track, country: &str) -> Option<Track> {
    Some(Track {
        id: id(IdKind::Track, track.gid.as_deref())?,
        name: track.name.clone().unwrap_or_default(),
        artists: track.artist.iter().filter_map(artist_ref).collect(),
        album: track.album.as_ref().and_then(album_ref)?,
        duration: millis(track.duration),
        disc_number: u32::try_from(track.disc_number.unwrap_or(1)).unwrap_or(1).max(1),
        track_number: u32::try_from(track.number.unwrap_or(0)).unwrap_or(0),
        explicit: track.explicit.unwrap_or(false),
        popularity: popularity(track.popularity),
        availability: availability(&track.restriction, country),
        alternatives: track.alternative.iter().filter_map(|alt| id(IdKind::Track, alt.gid.as_deref())).collect(),
    })
}

fn album_kind(kind: Option<i32>) -> AlbumKind {
    match kind.and_then(|k| md::album::Type::try_from(k).ok()) {
        Some(md::album::Type::Album) => AlbumKind::Album,
        Some(md::album::Type::Single) => AlbumKind::Single,
        Some(md::album::Type::Compilation) => AlbumKind::Compilation,
        Some(md::album::Type::Ep) => AlbumKind::Ep,
        Some(md::album::Type::Audiobook) => AlbumKind::Audiobook,
        Some(md::album::Type::Podcast) => AlbumKind::Podcast,
        None => AlbumKind::Unknown,
    }
}

pub(crate) fn album(album: &md::Album) -> Option<Album> {
    Some(Album {
        id: id(IdKind::Album, album.gid.as_deref())?,
        name: album.name.clone().unwrap_or_default(),
        artists: album.artist.iter().filter_map(artist_ref).collect(),
        release_date: release_date(album.date.as_ref()),
        images: images(album.cover_group.as_ref(), &album.cover),
        discs: album.disc.iter().map(|disc| disc.track.iter().filter_map(track_ref).collect()).collect(),
        kind: album_kind(album.r#type),
        label: album.label.clone(),
    })
}

/// Top tracks come from the entry for `country`, else the first entry the server sent.
pub(crate) fn artist(artist: &md::Artist, country: &str) -> Option<Artist> {
    let top = artist
        .top_track
        .iter()
        .find(|t| t.country.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(country)))
        .or_else(|| artist.top_track.first());
    Some(Artist {
        id: id(IdKind::Artist, artist.gid.as_deref())?,
        name: artist.name.clone().unwrap_or_default(),
        images: images(artist.portrait_group.as_ref(), &artist.portrait),
        popularity: popularity(artist.popularity),
        top_tracks: top.map(|t| t.track.iter().filter_map(track_ref).collect()),
        albums: Some(artist.album_group.iter().filter_map(|g| g.album.first()).filter_map(album_ref).collect()),
        singles: Some(artist.single_group.iter().filter_map(|g| g.album.first()).filter_map(album_ref).collect()),
    })
}

pub(crate) fn show(show: &md::Show) -> Option<Show> {
    Some(Show {
        id: id(IdKind::Show, show.gid.as_deref())?,
        name: show.name.clone().unwrap_or_default(),
        publisher: show.publisher.clone().unwrap_or_default(),
        description: show.description.clone().unwrap_or_default(),
        images: images(show.cover_image.as_ref(), &[]),
        episodes: show.episode.iter().filter_map(episode_ref).collect(),
    })
}

/// `None` when the episode, or its show, has no usable gid.
pub(crate) fn episode(episode: &md::Episode) -> Option<Episode> {
    Some(Episode {
        id: id(IdKind::Episode, episode.gid.as_deref())?,
        name: episode.name.clone().unwrap_or_default(),
        show: episode.show.as_ref().and_then(show_ref)?,
        duration: millis(episode.duration),
        release_date: release_date(episode.publish_time.as_ref()),
        description: episode.description.clone().unwrap_or_default(),
        images: images(episode.cover_image.as_ref(), &[]),
        explicit: episode.explicit.unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture file")
    }

    #[test]
    fn track_fixture_maps() {
        let proto = md::Track::decode(&fixture("track.pb")[..]).unwrap();
        let track = track(&proto, "US").unwrap();
        assert_eq!(track.id.to_uri(), "spotify:track:4PTG3Z6ehGkBFwjybzWkR8");
        assert_eq!(track.name, "Never Gonna Give You Up");
        assert_eq!(track.artists.len(), 1);
        assert_eq!(track.artists[0].name, "Rick Astley");
        assert_eq!(track.artists[0].id.to_uri(), "spotify:artist:0gxyHStUsqpMadRV0Di1Qt");
        let album = &track.album;
        assert_eq!(album.id.to_uri(), "spotify:album:6eUW0wxWtzkFdaEFsTJto6");
        assert_eq!(album.name, "Whenever You Need Somebody");
        assert_eq!(album.images.len(), 3);
        assert!(track.duration > Duration::from_secs(180) && track.duration < Duration::from_secs(300));
        assert_eq!(track.track_number, 1);
        assert_eq!(track.disc_number, 1);
        assert!(!track.explicit);
        assert_eq!(track.availability, Availability::Available, "the fixture carries no restrictions");
        assert!(track.popularity.is_some());
        assert!(track.alternatives.is_empty());
    }

    #[test]
    fn album_fixture_maps() {
        let proto = md::Album::decode(&fixture("album.pb")[..]).unwrap();
        let album = album(&proto).unwrap();
        assert_eq!(album.id.to_uri(), "spotify:album:6eUW0wxWtzkFdaEFsTJto6");
        assert_eq!(album.name, "Whenever You Need Somebody");
        assert_eq!(album.artists.len(), 1);
        assert_eq!(album.kind, AlbumKind::Album);
        assert_eq!(album.release_date.map(|d| d.to_string()), Some("1987-11-12".to_string()));
        assert!(album.label.is_some());
        assert_eq!(album.discs.len(), 1);
        assert_eq!(album.discs[0].len(), 10);
        assert_eq!(album.images.len(), 3);
        assert!(album.images.iter().all(|i| i.width.is_some() && i.height.is_some()));
        assert!(album.images[0].url().starts_with("https://i.scdn.co/image/"));
    }

    #[test]
    fn artist_fixture_maps() {
        let proto = md::Artist::decode(&fixture("artist.pb")[..]).unwrap();
        let artist = artist(&proto, "US").unwrap();
        assert_eq!(artist.id.to_uri(), "spotify:artist:0gxyHStUsqpMadRV0Di1Qt");
        assert_eq!(artist.name, "Rick Astley");
        assert_eq!(artist.images.len(), 3);
        assert_eq!(artist.top_tracks.as_ref().map(Vec::len), Some(10));
        assert_eq!(artist.albums.as_ref().map(Vec::len), Some(13));
        assert_eq!(artist.singles.as_ref().map(Vec::len), Some(43));
        assert!(artist.popularity.is_some());
    }

    #[test]
    fn artist_top_tracks_fall_back_to_the_first_country() {
        let proto = md::Artist::decode(&fixture("artist.pb")[..]).unwrap();
        let artist = artist(&proto, "ZZ").unwrap();
        assert_eq!(artist.top_tracks.as_ref().map(Vec::len), Some(10));
    }

    #[test]
    fn show_fixture_maps() {
        let proto = md::Show::decode(&fixture("show.pb")[..]).unwrap();
        let show = show(&proto).unwrap();
        assert_eq!(show.id.to_uri(), "spotify:show:4rOoJ6Egrf8K2IrywzwOMk");
        assert_eq!(show.name, "The Joe Rogan Experience");
        assert_eq!(show.publisher, "Joe Rogan");
        assert!(show.episodes.len() > 2000);
        assert_eq!(show.images.len(), 3);
        // ShowV4 lists episodes by id and publish date only; names and durations need `episodes()`.
        assert!(show.episodes[0].duration.is_none());
        assert!(show.episodes[0].release_date.is_some());
    }

    #[test]
    fn episode_fixture_maps() {
        let proto = md::Episode::decode(&fixture("episode.pb")[..]).unwrap();
        let episode = episode(&proto).unwrap();
        assert_eq!(episode.id.to_uri(), "spotify:episode:1arAXEEsM5yVp3cFrDRuZ6");
        assert_eq!(episode.name, "#2556 - Siddharth Kara");
        assert_eq!(episode.duration, Duration::from_millis(8_510_079));
        assert_eq!(episode.show.name, "The Joe Rogan Experience");
        assert_eq!(episode.release_date.map(|d| d.to_string()), Some("2026-09-22".to_string()));
        assert!(episode.explicit);
        assert_eq!(episode.images.len(), 3);
    }

    fn restriction(allowed: Option<&str>, forbidden: Option<&str>, catalogues: &[&str]) -> md::Restriction {
        md::Restriction {
            catalogue: vec![],
            r#type: None,
            catalogue_str: catalogues.iter().map(|s| s.to_string()).collect(),
            country_restriction: match (allowed, forbidden) {
                (Some(a), _) => Some(md::restriction::CountryRestriction::CountriesAllowed(a.to_string())),
                (None, Some(f)) => Some(md::restriction::CountryRestriction::CountriesForbidden(f.to_string())),
                (None, None) => None,
            },
        }
    }

    #[test]
    fn availability_rules() {
        assert_eq!(availability(&[], "US"), Availability::Available);
        assert_eq!(availability(&[restriction(Some("USSE"), None, &["premium"])], "US"), Availability::Available);
        assert!(matches!(availability(&[restriction(Some("SE"), None, &["premium"])], "US"), Availability::Restricted { .. }));
        assert!(matches!(availability(&[restriction(Some(""), None, &[])], "US"), Availability::Restricted { .. }));
        assert!(matches!(availability(&[restriction(None, Some("GBUS"), &["premium"])], "US"), Availability::Restricted { .. }));
        assert_eq!(availability(&[restriction(None, Some("GB"), &["premium"])], "US"), Availability::Available);
        assert_eq!(availability(&[restriction(Some("SE"), None, &["ad"])], "US"), Availability::Available, "ad-catalogue restrictions do not apply");
    }

    #[test]
    fn entities_without_a_gid_are_none() {
        assert!(track(&md::Track::default(), "US").is_none());
        assert!(album(&md::Album::default()).is_none());
        assert!(artist(&md::Artist::default(), "US").is_none());
        assert!(show(&md::Show::default()).is_none());
        assert!(episode(&md::Episode::default()).is_none());
    }

    #[test]
    fn a_track_without_an_album_is_none() {
        let proto = md::Track { gid: Some(vec![1; 16]), name: Some("x".to_string()), ..Default::default() };
        assert!(track(&proto, "US").is_none());
    }
}
