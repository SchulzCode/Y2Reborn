//! Playlists and the rootlist over the playlist v2 endpoints (spec section 6.2).

use std::time::{Duration, UNIX_EPOCH};

use housecat_core::{FileId, IdKind, SpotifyId};
use housecat_protocol::spotify::playlist4::proto as pl;

use crate::model::{Image, Playlist, PlaylistItem, PlaylistRef, RootlistEntry};

/// The `decorate` query that makes the rootlist carry one `meta_items` entry per item.
pub(crate) const ROOTLIST_DECORATE: &str = "revision,attributes,length,owner,capabilities,status_code";

pub(crate) fn from_proto(id: SpotifyId, content: &pl::SelectedListContent) -> Playlist {
    let attributes = content.attributes.as_ref();
    Playlist {
        id,
        name: attributes.and_then(|a| a.name.clone()).unwrap_or_default(),
        description: attributes.and_then(|a| a.description.clone()).unwrap_or_default(),
        owner: content.owner_username.clone().unwrap_or_default(),
        image: attributes
            .and_then(|a| a.picture.as_deref())
            .and_then(|picture| FileId::from_slice(picture).ok())
            .map(|id| Image { id, width: None, height: None }),
        collaborative: attributes.and_then(|a| a.collaborative).unwrap_or(false),
        revision: content.revision.as_deref().map(hex::encode).unwrap_or_default(),
        length: content.length.and_then(|l| u32::try_from(l).ok()).unwrap_or(0),
        items: content.contents.as_ref().map(|c| c.items.iter().filter_map(item).collect()).unwrap_or_default(),
    }
}

/// Items whose URI is not a catalog entity (local files, for one) are skipped.
fn item(item: &pl::Item) -> Option<PlaylistItem> {
    let id = SpotifyId::from_uri(&item.uri).ok()?;
    let attributes = item.attributes.as_ref();
    Some(PlaylistItem {
        id,
        added_at: attributes
            .and_then(|a| a.timestamp)
            .and_then(|ms| u64::try_from(ms).ok())
            .map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
        added_by: attributes.and_then(|a| a.added_by.clone()).filter(|s| !s.is_empty()),
    })
}

/// `spotify:playlist:<id>` or the legacy `spotify:user:<user>:playlist:<id>`.
fn playlist_id(uri: &str) -> Option<SpotifyId> {
    let parts: Vec<&str> = uri.split(':').collect();
    let base62 = match parts.as_slice() {
        ["spotify", "playlist", id] => id,
        ["spotify", "user", _, "playlist", id] => id,
        _ => return None,
    };
    SpotifyId::from_base62(IdKind::Playlist, base62).ok()
}

/// Flatten the rootlist's items into entries with folder paths. Folder markers are
/// `spotify:start-group:<id>:<percent-encoded name>` and `spotify:end-group:<id>`.
/// `metas` decorates the entries when it is index-aligned with `items`, which the server
/// guarantees when `decorate` was requested.
pub(crate) fn flatten_rootlist(items: &[pl::Item], metas: &[pl::MetaItem]) -> Vec<RootlistEntry> {
    let metas = (metas.len() == items.len()).then_some(metas);
    let mut path: Vec<String> = Vec::new();
    let mut entries = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let parts: Vec<&str> = item.uri.split(':').collect();
        match parts.as_slice() {
            ["spotify", "start-group", _, name] => path.push(percent_decode(name)),
            ["spotify", "start-group", _] => path.push(String::new()),
            ["spotify", "end-group", ..] => {
                path.pop();
            }
            _ => {
                let Some(id) = playlist_id(&item.uri) else { continue };
                let meta = metas.map(|m| &m[index]);
                entries.push(RootlistEntry {
                    playlist: PlaylistRef {
                        id,
                        name: meta.and_then(|m| m.attributes.as_ref()).and_then(|a| a.name.clone()).unwrap_or_default(),
                    },
                    folder_path: path.clone(),
                    length: meta.and_then(|m| m.length).and_then(|l| u32::try_from(l).ok()),
                    owner: meta.and_then(|m| m.owner_username.clone()),
                });
            }
        }
    }
    entries
}

/// Decode `%XX` escapes and `+` as space, as folder names in rootlist URIs are encoded.
pub(crate) fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // Three bytes are needed: `%` and two hex digits, and slicing must not split a char.
            b'%' if i + 2 < bytes.len() && input.is_char_boundary(i + 3) => {
                match u8::from_str_radix(&input[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Encode everything but unreserved characters, for usernames in request paths.
pub(crate) fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    fn item(uri: &str) -> pl::Item {
        pl::Item { uri: uri.to_string(), attributes: None }
    }

    fn meta(name: &str, length: i32, owner: &str) -> pl::MetaItem {
        pl::MetaItem {
            revision: Some(vec![1, 2, 3]),
            attributes: Some(pl::ListAttributes { name: Some(name.to_string()), ..Default::default() }),
            length: Some(length),
            owner_username: Some(owner.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn editorial_playlist_fixture_maps() {
        let bytes = std::fs::read(format!("{}/tests/fixtures/playlist.pb", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let content = pl::SelectedListContent::decode(&bytes[..]).unwrap();
        let id = SpotifyId::from_uri("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M").unwrap();
        let playlist = from_proto(id, &content);
        assert_eq!(playlist.id, id);
        assert!(playlist.name.starts_with("Today"));
        assert_eq!(playlist.owner, "spotify");
        assert_eq!(playlist.length, 50);
        assert_eq!(playlist.items.len(), 50);
        assert_eq!(playlist.revision.len(), 48, "24 revision bytes as hex");
        assert!(playlist.items.iter().all(|i| i.id.kind == IdKind::Track));
        assert!(playlist.items[0].added_at.is_some());
        assert!(!playlist.collaborative);
    }

    #[test]
    fn items_with_unknown_uris_are_skipped_and_attributes_are_kept() {
        let content = pl::SelectedListContent {
            attributes: Some(pl::ListAttributes {
                name: Some("Mix".to_string()),
                description: Some("desc".to_string()),
                picture: Some(vec![7; 20]),
                collaborative: Some(true),
                ..Default::default()
            }),
            contents: Some(pl::ListItems {
                pos: 0,
                truncated: false,
                items: vec![
                    pl::Item {
                        uri: "spotify:track:4PTG3Z6ehGkBFwjybzWkR8".to_string(),
                        attributes: Some(pl::ItemAttributes {
                            added_by: Some("bob".to_string()),
                            timestamp: Some(1_700_000_000_000),
                            ..Default::default()
                        }),
                    },
                    item("spotify:local:artist:album:title:180"),
                    item("spotify:episode:1arAXEEsM5yVp3cFrDRuZ6"),
                ],
                meta_items: vec![],
                available_signals: vec![],
                continuation_token: None,
            }),
            owner_username: Some("alice".to_string()),
            revision: Some(vec![0xab, 0xcd]),
            length: Some(3),
            ..Default::default()
        };
        let playlist = from_proto(SpotifyId::from_uri("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M").unwrap(), &content);
        assert_eq!(playlist.name, "Mix");
        assert_eq!(playlist.description, "desc");
        assert!(playlist.collaborative);
        assert_eq!(playlist.image.as_ref().map(|i| i.id), Some(FileId([7; 20])));
        assert_eq!(playlist.revision, "abcd");
        assert_eq!(playlist.length, 3);
        assert_eq!(playlist.items.len(), 2, "the local file is skipped");
        assert_eq!(playlist.items[0].added_by.as_deref(), Some("bob"));
        assert_eq!(
            playlist.items[0].added_at,
            Some(std::time::UNIX_EPOCH + Duration::from_millis(1_700_000_000_000))
        );
        assert_eq!(playlist.items[1].id.kind, IdKind::Episode);
    }

    #[test]
    fn rootlist_folders_are_flattened_with_names_from_meta_items() {
        let items = vec![
            item("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"),
            item("spotify:start-group:g1:Rock%20Hits"),
            item("spotify:user:alice:playlist:4PTG3Z6ehGkBFwjybzWkR8"),
            item("spotify:start-group:g2:Old+Stuff"),
            item("spotify:playlist:1arAXEEsM5yVp3cFrDRuZ6"),
            item("spotify:end-group:g2"),
            item("spotify:end-group:g1"),
            item("spotify:playlist:6eUW0wxWtzkFdaEFsTJto6"),
        ];
        let metas = vec![
            meta("Top", 50, "spotify"),
            pl::MetaItem::default(),
            meta("Legacy", 3, "alice"),
            pl::MetaItem::default(),
            meta("Deep", 1, "alice"),
            pl::MetaItem::default(),
            pl::MetaItem::default(),
            meta("Last", 0, "alice"),
        ];
        let entries = flatten_rootlist(&items, &metas);
        let summary: Vec<(String, Vec<String>)> =
            entries.iter().map(|e| (e.playlist.name.clone(), e.folder_path.clone())).collect();
        assert_eq!(
            summary,
            vec![
                ("Top".to_string(), vec![]),
                ("Legacy".to_string(), vec!["Rock Hits".to_string()]),
                ("Deep".to_string(), vec!["Rock Hits".to_string(), "Old Stuff".to_string()]),
                ("Last".to_string(), vec![]),
            ]
        );
        assert_eq!(entries[1].playlist.id.to_uri(), "spotify:playlist:4PTG3Z6ehGkBFwjybzWkR8");
        assert_eq!(entries[0].length, Some(50));
        assert_eq!(entries[0].owner.as_deref(), Some("spotify"));
    }

    #[test]
    fn rootlist_without_aligned_meta_items_still_lists_playlists() {
        let items = vec![item("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M")];
        let entries = flatten_rootlist(&items, &[]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].playlist.name, "");
        assert_eq!(entries[0].length, None);
    }

    #[test]
    fn percent_coding() {
        assert_eq!(percent_decode("Rock%20Hits+%26+more"), "Rock Hits & more");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("trailing%2"), "trailing%2");
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        assert_eq!(percent_encode("alice"), "alice");
        assert_eq!(percent_encode("a b/c"), "a%20b%2Fc");
    }
}
