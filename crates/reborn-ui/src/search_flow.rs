//! Library search: typing, folding the keyboard, the result heights and preview.
use crate::*;

impl Ui {
    /// Open the search with its keys up: on the search page with its text,
    /// else a new, empty search (#44: a search
    /// left is not offered again). The search index is built here (once per
    /// library), not on the first letter.
    pub(crate) fn open_library_search(&mut self, m: &mut AppModel, tracks: &[Track]) {
        let k = if m.screen == Screen::Search {
            let text = catalog::search_parts(&m.navigation.filter).0.to_owned();
            let mut k = keyboard::Keyboard::new(keyboard::Purpose::LibrarySearch, text.clone());
            k.restore = Some(text);
            k
        } else {
            Self::go(m, Screen::Search, "");
            keyboard::Keyboard::new(keyboard::Purpose::LibrarySearch, "")
        };
        self.keyboard = Some(k);
        self.folding.borrow_mut().take();
        self.search_typed(m, tracks);
    }

    /// The search's text changed: the page under the keys searches it, and
    /// the field counts what it finds.
    pub(crate) fn search_typed(&mut self, m: &mut AppModel, tracks: &[Track]) {
        let Some(text) = self.keyboard.as_ref().map(|k| k.text.clone()) else {
            return;
        };
        m.navigation.filter = text.clone();
        m.navigation.focus = 0;
        m.navigation.scroll = 0;
        let (found, summary) = {
            let mut catalog = self.catalog.borrow_mut();
            self.ensure_catalog(&mut catalog, m, tracks);
            (catalog.found, catalog.summary.clone())
        };
        let blank = text.trim().is_empty();
        if let Some(k) = self.keyboard.as_mut() {
            k.found = if blank { 0 } else { found };
            k.note = if blank { String::new() } else { summary };
        }
    }

    /// Out of the search to the screen it was opened from, past its own
    /// levels (a section's See All is a search page too).
    pub(crate) fn leave_search(m: &mut AppModel) {
        while m.screen == Screen::Search && m.navigation.modal.is_none() {
            Self::back(m);
        }
    }

    /// Fold the search's keys away: the page lists everything found with
    /// the focus on the first result (on the field when nothing was found).
    pub(crate) fn fold_search(&mut self, m: &mut AppModel, tracks: &[Track]) {
        let Some(k) = self.keyboard.take() else {
            return;
        };
        let text = k.text.trim().to_owned();
        m.navigation.filter = text;
        m.navigation.scroll = 0;
        m.navigation.focus = {
            let mut catalog = self.catalog.borrow_mut();
            self.ensure_catalog(&mut catalog, m, tracks);
            catalog
                .rows
                .iter()
                .position(|r| r.search_kind() == catalog::SearchKind::Result)
                .unwrap_or(0)
        };
        self.scroll_search(m, tracks);
        self.start_fold(k);
    }

    pub(crate) fn start_fold(&self, k: keyboard::Keyboard) {
        if self.ui_motion {
            *self.folding.borrow_mut() = Some((Instant::now(), k));
        }
    }

    /// The search's keys folding away over its page, until they are gone.
    pub(crate) fn draw_folding(&self, c: &mut canvas::Canvas) {
        let mut folding = self.folding.borrow_mut();
        let Some((at, k)) = folding.as_ref() else {
            return;
        };
        let p = motion::ease_out(at.elapsed().as_secs_f32() / FOLD_S);
        if p >= 1. || self.keyboard.is_some() || !self.ui_motion {
            *folding = None;
            return;
        }
        keyboard::draw_folding(c, self, k, p);
    }

    /// The search page or Up Next in Reborn's own drawing, rows of
    /// differing heights (a skin's list scrolls by its own rows).
    pub(crate) fn native_search(&self, m: &AppModel, tracks: &[Track]) -> bool {
        matches!(m.screen, Screen::Search | Screen::Queue)
            && self
                .skin
                .as_deref()
                .and_then(|skin| screens::skin_layout(skin, self, m, tracks))
                .is_none()
    }

    /// An album, a playlist or a book drawn by Reborn: its header scrolls
    /// stays and its list scrolls (`components::header_list_first`).
    pub(crate) fn native_header_page(&self, m: &AppModel, tracks: &[Track]) -> bool {
        matches!(m.screen, Screen::Album | Screen::Playlist | Screen::Book)
            && self
                .skin
                .as_deref()
                .and_then(|skin| screens::skin_layout(skin, self, m, tracks))
                .is_none()
    }

    /// The music's levels as heard now (none while nothing plays).
    pub fn set_heard(&mut self, levels: Option<Levels>) {
        self.heard = levels;
    }

    /// The levels the wave shows this frame: they rise with the music at
    /// once and fall back slowly, as a meter does; none without music.
    pub(crate) fn wave_levels(&self) -> Option<Levels> {
        let target = self.heard?;
        let now = Instant::now();
        let mut shown = self.shown_levels.borrow_mut();
        let (last, at) = shown.unwrap_or((target, now));
        let dt = now.duration_since(at).as_secs_f32().min(0.25);
        let step = |from: f32, to: f32| {
            let rate = if to > from { 18. } else { 4. };
            from + (to - from) * (1. - (-rate * dt).exp())
        };
        let levels = Levels {
            bass: step(last.bass, target.bass),
            mid: step(last.mid, target.mid),
            high: step(last.high, target.high),
        };
        *shown = Some((levels, now));
        Some(levels)
    }

    /// A settings page's first row shown: kept until the focus reaches an
    /// edge, as every list (8.37).
    pub(crate) fn group_window(
        &self,
        m: &AppModel,
        focus: usize,
        visible: usize,
        count: usize,
    ) -> usize {
        let mut last = self.group_scroll.borrow_mut();
        let here = (m.screen, m.navigation.filter.as_str());
        let prev = last
            .as_ref()
            .filter(|(s, f, _)| (*s, f.as_str()) == here)
            .map_or(0, |(_, _, first)| *first);
        let first = prev
            .min(focus)
            .max((focus + 1).saturating_sub(visible))
            .min(count.saturating_sub(visible));
        *last = Some((m.screen, m.navigation.filter.clone(), first));
        first
    }

    /// The buttons in such a page's header (Play, Shuffle, Continue…): its
    /// first rows.
    pub(crate) fn header_lead(&self, m: &AppModel, tracks: &[Track]) -> usize {
        const BUTTONS: [&str; 6] = [
            "play:collection",
            "shuffle:collection",
            "pl_play",
            "pl_shuffle",
            "book_resume",
            "book_restart",
        ];
        (0..2)
            .take_while(|&i| {
                self.row_at(m, tracks, i)
                    .is_some_and(|r| BUTTONS.contains(&r.key.as_str()))
            })
            .count()
    }

    pub(crate) fn scroll_search(&self, m: &mut AppModel, tracks: &[Track]) {
        let heights = self.search_heights(m, tracks);
        let (focus, scroll) = (m.navigation.focus, m.navigation.scroll);
        m.navigation.scroll = if m.screen == Screen::Queue {
            views_search::window(&heights, focus, scroll)
        } else {
            let fixed = self
                .row_at(m, tracks, 0)
                .is_some_and(|r| r.key == catalog::SEARCH_FIELD);
            views_search::search_first(&heights, focus, scroll, fixed)
        };
    }

    /// Each search page (or Up Next) row's height.
    pub(crate) fn search_heights(&self, m: &AppModel, tracks: &[Track]) -> Vec<f32> {
        if m.screen == Screen::Queue {
            let layout = self.up_next(m, tracks);
            return (0..layout.len())
                .map(|i| {
                    if layout.row(i).is_some_and(up_next::Row::heading) {
                        views_search::HEADING_ROW
                    } else {
                        views_search::ROW
                    }
                })
                .collect();
        }
        let mut catalog = self.catalog.borrow_mut();
        self.ensure_catalog(&mut catalog, m, tracks);
        catalog
            .rows
            .iter()
            .map(|r| views_search::row_height(r.search_kind()))
            .collect()
    }

    /// What the search found, in words, and how many results.
    pub(crate) fn search_summary(&self, m: &AppModel, tracks: &[Track]) -> (String, usize) {
        let mut catalog = self.catalog.borrow_mut();
        self.ensure_catalog(&mut catalog, m, tracks);
        (catalog.summary.clone(), catalog.found)
    }

    /// The search page's first `n` results, each saying what it is: the
    /// preview above the keys, where there are no headings.
    pub(crate) fn search_preview(&self, m: &AppModel, tracks: &[Track], n: usize) -> Vec<Item> {
        let mut catalog = self.catalog.borrow_mut();
        self.ensure_catalog(&mut catalog, m, tracks);
        catalog
            .rows
            .iter()
            .filter(|r| r.search_kind() == catalog::SearchKind::Result)
            .take(n)
            .map(|r| {
                let mut item = r.item(tracks);
                item.secondary = match r {
                    catalog::Row::SearchSong(i) => {
                        format!("Song · {}", names_or_unknown(&tracks[*i].artist))
                    }
                    _ if item.key.starts_with("artist:") => format!("Artist · {}", item.secondary),
                    _ if item.key.starts_with("album:") => format!("Album · {}", item.secondary),
                    _ if item.secondary == "Audiobook" => item.secondary,
                    _ => format!("Audiobook · {}", item.secondary),
                };
                item
            })
            .collect()
    }
}
