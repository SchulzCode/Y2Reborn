//! Cached ordered row identities, rebuilt on catalog/route change, not wheel ticks.
use crate::{album_filter, display_or_unknown, track_matches, Item};
use reborn_core::{AppModel, Screen, Track};
use std::collections::{BTreeMap, BTreeSet};
pub enum Row {
    Track(usize),
    Item(Item),
}
impl Row {
    pub fn item(&self, tracks: &[Track]) -> Item {
        match self {
            Self::Track(i) => track_item(*i, &tracks[*i]),
            Self::Item(i) => i.clone(),
        }
    }
}
#[derive(Default)]
pub struct Catalog {
    stamp: Option<(usize, usize, Screen, String)>,
    pub rows: Vec<Row>,
}
impl Catalog {
    fn push(&mut self, item: Item) {
        self.rows.push(Row::Item(item));
    }
    pub fn clear(&mut self) {
        self.stamp = None;
    }
    pub fn ensure(&mut self, m: &AppModel, tracks: &[Track]) {
        let stamp = (
            tracks.as_ptr() as usize,
            tracks.len(),
            m.screen,
            m.navigation.filter.clone(),
        );
        if self.stamp.as_ref() == Some(&stamp) {
            return;
        }
        self.stamp = Some(stamp);
        self.rows.clear();
        match m.screen {
            Screen::Albums => {
                let mut albums = BTreeMap::new();
                for t in tracks.iter().filter(|t| {
                    t.online
                        && (m.navigation.filter.is_empty()
                            || track_matches(t, &m.navigation.filter))
                }) {
                    albums
                        .entry((t.album.as_str(), t.album_artist.as_str()))
                        .or_insert(t);
                }
                for ((album, artist), t) in albums {
                    self.push(
                        Item::new(display_or_unknown(album), album_filter(t)).with_secondary(
                            display_or_unknown(if artist.is_empty() { &t.artist } else { artist }),
                        ),
                    );
                }
            }
            Screen::Artists => {
                let artists: BTreeSet<_> = tracks
                    .iter()
                    .filter(|t| t.online)
                    .map(|t| t.artist.as_str())
                    .collect();
                for artist in artists {
                    self.push(Item::new(
                        display_or_unknown(artist),
                        format!("artist:{artist}"),
                    ));
                }
            }
            Screen::Folders => {
                let parent =
                    std::path::Path::new(m.navigation.filter.strip_prefix("folder:").unwrap_or(""));
                let mut children = BTreeSet::new();
                if parent.as_os_str().is_empty() {
                    for s in m.sources.iter().filter(|s| s.online) {
                        children.insert(s.root.clone());
                    }
                    if children.is_empty() {
                        for t in tracks.iter().filter(|t| t.online) {
                            if let Some(p) = t.path.parent() {
                                children.insert(p.to_path_buf());
                            }
                        }
                    }
                } else {
                    for t in tracks.iter().filter(|t| t.online) {
                        if let Ok(suffix) = t.path.strip_prefix(parent) {
                            if let Some(first) = suffix.components().next() {
                                let child = parent.join(first);
                                if child != t.path {
                                    children.insert(child);
                                }
                            }
                        }
                    }
                }
                for child in children {
                    self.push(
                        Item::new(
                            child
                                .file_name()
                                .map(|x| x.to_string_lossy().into_owned())
                                .unwrap_or_else(|| child.display().to_string()),
                            format!("folder:{}", child.display()),
                        )
                        .with_secondary("Folder"),
                    );
                }
                if !parent.as_os_str().is_empty() {
                    for (i, _) in tracks
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| t.online && t.path.parent() == Some(parent))
                    {
                        self.rows.push(Row::Track(i));
                    }
                }
            }
            Screen::Tracks | Screen::Album | Screen::Artist => {
                if matches!(m.screen, Screen::Album | Screen::Artist) {
                    self.push(
                        Item::new("Play All", "play:collection")
                            .with_secondary("Hold Select for collection actions"),
                    );
                }
                if m.screen == Screen::Artist {
                    self.push(Item::new("Albums by this Artist", "artist_albums"));
                }
                let mut ids: Vec<_> = tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.online && track_matches(t, &m.navigation.filter))
                    .collect();
                if ids.is_empty() {
                    self.rows.clear();
                    return;
                }
                if m.screen == Screen::Tracks && m.navigation.filter.is_empty() {
                    ids.sort_by_cached_key(|(_, t)| {
                        if t.title.is_empty() {
                            t.filename.to_lowercase()
                        } else {
                            t.title.to_lowercase()
                        }
                    });
                }
                if m.screen == Screen::Album {
                    ids.sort_by_key(|(_, t)| (t.disc, t.track, t.filename.as_str()));
                }
                if matches!(m.screen, Screen::Album | Screen::Artist) {
                    if let Some(Row::Item(item)) = self.rows.first_mut() {
                        item.secondary = format!(
                            "{} songs · {}",
                            ids.len(),
                            crate::components::time(ids.iter().map(|(_, t)| t.duration_ms).sum())
                        );
                    }
                }
                for (i, _) in ids {
                    self.rows.push(Row::Track(i));
                }
            }
            _ => {}
        }
    }
}
fn track_item(i: usize, t: &Track) -> Item {
    Item::new(
        if t.title.is_empty() {
            &t.filename
        } else {
            &t.title
        },
        format!("track:{i}"),
    )
    .with_secondary(display_or_unknown(&t.artist))
}
