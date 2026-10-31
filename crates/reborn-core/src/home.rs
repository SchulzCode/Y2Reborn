//! Home's places and the order the listener gave them (Customize > Home Menu, Y2Reborn #53), and the
//! order of the Playlists list (#33). Now Playing stays first and Settings last.
use crate::{Settings, HOME_HIDEABLE, HOME_PINNABLE};

/// Every place Home can show between Now Playing and Settings, in the standard order: (key, label).
pub const HOME_PLACES: [(&str, &str); 10] = [
    ("music", "Music"),
    ("audiobooks", "Audiobooks"),
    ("artists", "Artists"),
    ("albums", "Albums"),
    ("tracks", "Songs"),
    ("genres", "Genres"),
    ("folders", "Folders"),
    ("playlists", "Playlists"),
    ("radio", "Radio"),
    ("search", "Search"),
];

/// `known` in `order`'s order first (unknown and repeated names skipped), the rest as they come.
pub fn ordered<'a, T>(known: &'a [T], order: &[String], name: impl Fn(&T) -> &str) -> Vec<&'a T> {
    let mut out: Vec<&T> = Vec::with_capacity(known.len());
    for wanted in order {
        if let Some(item) = known.iter().find(|k| name(k) == wanted) {
            if !out.iter().any(|o| name(o) == wanted) {
                out.push(item);
            }
        }
    }
    out.extend(known.iter().filter(|k| !order.iter().any(|o| o == name(k))));
    out
}

/// `items` with the one at `from` moved to `to`.
pub fn moved<T: Clone>(items: &[T], from: usize, to: usize) -> Vec<T> {
    let mut out = items.to_vec();
    if from < out.len() {
        let item = out.remove(from);
        out.insert(to.min(out.len()), item);
    }
    out
}

impl Settings {
    /// Home's places in the listener's order (all of them, shown or not).
    pub fn home_places(&self) -> Vec<(&'static str, &'static str)> {
        ordered(&HOME_PLACES, &self.home_order, |(key, _)| key)
            .into_iter()
            .copied()
            .collect()
    }
    /// Whether a place stands on Home: Audiobooks once the library holds a book (`has_books`), a
    /// Music list once pinned, Home's own places unless hidden.
    pub fn home_place_shown(&self, key: &str, has_books: bool) -> bool {
        if key == "audiobooks" {
            self.audiobooks_on_home && has_books
        } else if HOME_PINNABLE.iter().any(|(k, _)| *k == key) {
            self.home_pins.iter().any(|p| p == key)
        } else {
            HOME_HIDEABLE.iter().any(|(k, _)| *k == key) && self.home_shows(key)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_order_comes_first_and_new_places_keep_their_standard_place_after_it() {
        let mut s = Settings::default();
        assert_eq!(s.home_places(), HOME_PLACES);
        s.home_order = vec![
            "radio".into(),
            "music".into(),
            "gone".into(),
            "radio".into(),
        ];
        let keys: Vec<_> = s.home_places().into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            [
                "radio",
                "music",
                "audiobooks",
                "artists",
                "albums",
                "tracks",
                "genres",
                "folders",
                "playlists",
                "search"
            ]
        );
        assert_eq!(moved(&[1, 2, 3, 4], 0, 2), [2, 3, 1, 4]);
        assert_eq!(moved(&[1, 2, 3, 4], 3, 0), [4, 1, 2, 3]);
    }

    #[test]
    fn home_shows_its_own_places_unless_hidden_and_music_lists_once_pinned() {
        let mut s = Settings::default();
        assert!(s.home_place_shown("radio", false));
        assert!(!s.home_place_shown("albums", false));
        assert!(
            !s.home_place_shown("audiobooks", false),
            "no books, no Audiobooks"
        );
        s.pin_home("albums", true);
        s.show_on_home("radio", false);
        assert!(s.home_place_shown("albums", false));
        assert!(!s.home_place_shown("radio", false));
    }
}
