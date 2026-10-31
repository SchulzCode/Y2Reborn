//! Input: an action, the wheel and focus, selecting a row.
use crate::*;

/// Only the wheel's fastest cadence (4 steps: ten detents under 45 ms apart)
/// shows the fast-scroll letter; a quick but steady turn moves rows. At 2 steps
/// the letter came after four quick detents, so rows could not be read through
/// at speed (Y2Linux #64).
const LETTER_SCROLL_STEPS: i32 = 4;

impl Ui {
    pub fn action(&mut self, m: &mut AppModel, tracks: &[Track], action: Action) -> Effect {
        let before = (m.screen, m.navigation.filter.clone());
        let effect = self.action_inner(m, tracks, action);
        // Cover Flow and the album page hand the centre cover to each other.
        if m.settings.album_view == reborn_core::AlbumView::CoverFlow && !self.motion_fixed {
            let kind = match (before.0, m.screen) {
                (Screen::Albums, Screen::Album) => Some(flow::TransitionKind::Open),
                (Screen::Album, Screen::Albums) => Some(flow::TransitionKind::Close),
                _ => None,
            };
            if let Some(kind) = kind {
                self.transition.set(Some(flow::Transition {
                    kind,
                    start: Instant::now(),
                }));
            }
        }
        effect
    }

    pub(crate) fn action_inner(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        action: Action,
    ) -> Effect {
        if !m.screen_off {
            self.normalize(m, tracks);
        }
        // The one-step update owns the screen until it restarts the player
        // or ends; Select is its Cancel while the download runs.
        if self.update_run {
            return match action {
                Action::ScreenWake => Effect::ScreenWake,
                Action::ScreenSleep | Action::PowerShort => Effect::ScreenSleep,
                Action::VolumeUp => Effect::AdjustVolume(2),
                Action::VolumeDown => Effect::AdjustVolume(-2),
                Action::Select if update_cancellable(m) => Effect::CancelDownload,
                _ => Effect::None,
            };
        }
        // Installing/restoring the root must not compete with navigation,
        // shutdown or playback. The platform owns the resulting restart.
        if matches!(
            m.platform.busy,
            Some(PlatformTask::UpdateApply | PlatformTask::UpdateRollback)
        ) || m.platform.shutting_down.is_some()
        {
            return match action {
                Action::ScreenWake => Effect::ScreenWake,
                Action::VolumeUp => Effect::AdjustVolume(2),
                Action::VolumeDown => Effect::AdjustVolume(-2),
                _ => Effect::None,
            };
        }
        match action {
            Action::ScreenSleep => return Effect::ScreenSleep,
            Action::ScreenWake => return Effect::ScreenWake,
            Action::PowerShort => {
                if matches!(
                    m.playback,
                    reborn_core::PlaybackState::Playing | reborn_core::PlaybackState::Buffering
                ) {
                    return Effect::ScreenSleep;
                }
                // As stock: the screen goes off at once and a wake returns to
                // where the person was (the Sleep page showed "Preparing to
                // Sleep…" to a press during the preparation; Y2Linux F48).
                return Effect::Platform(reborn_core::PlatformTask::SleepRequest);
            }
            Action::PowerHold => {
                self.power_hold = Some(Instant::now());
                return Effect::None;
            }
            Action::PowerHoldCancel => {
                self.power_hold = None;
                return Effect::None;
            }
            Action::PlayPause => return Effect::TogglePlayback,
            Action::ShowNowPlaying
                if !m.screen_off
                    && m.navigation.modal.is_none()
                    && self.keyboard.is_none()
                    && self.pairing.is_none() =>
            {
                Self::go(m, Screen::NowPlaying, "");
                return Effect::None;
            }
            Action::PreviousTrack => return Effect::PreviousTrack,
            Action::NextTrack => return Effect::NextTrack,
            Action::SeekBackward => return Effect::Seek(-1),
            Action::SeekForward => return Effect::Seek(1),
            Action::VolumeUp => return Effect::AdjustVolume(2),
            Action::VolumeDown => return Effect::AdjustVolume(-2),
            _ => {}
        }
        if m.screen_off {
            return Effect::None;
        }
        if let Some(effect) = self.overlay_action(m, action) {
            return effect;
        }
        if self.pairing.is_some() {
            return match action {
                Action::Select => {
                    self.pairing = None;
                    Effect::ConfirmPairing(self.pairing_focus == 0)
                }
                Action::WheelClockwise(_) | Action::NavigateDown => {
                    self.pairing_focus = 1;
                    Effect::None
                }
                Action::WheelCounterClockwise(_) | Action::NavigateUp => {
                    self.pairing_focus = 0;
                    Effect::None
                }
                Action::Back => {
                    self.pairing = None;
                    Effect::ConfirmPairing(false)
                }
                _ => Effect::None,
            };
        }
        if self.keyboard.is_some() {
            return self.entry(m, tracks, action);
        }
        // Up Next's song moving with the wheel; Select Songs.
        if let Some(effect) = self.list_mode_input(m, tracks, action) {
            return effect;
        }
        if m.screen == Screen::Playlist && m.navigation.modal.is_none() {
            if let Some(effect) = self.moving_action(m, tracks, action) {
                return effect;
            }
        }
        match action {
            Action::Home => {
                if m.navigation.modal.is_some() {
                    Self::back(m);
                } else {
                    Self::home(m);
                }
                return Effect::None;
            }
            Action::Back => {
                // The search's results: Back goes up one level, to typing
                // (#44); one section alone (See All) back to all of them.
                if m.screen == Screen::Search
                    && m.navigation.modal.is_none()
                    && catalog::search_parts(&m.navigation.filter).1.is_none()
                {
                    self.open_library_search(m, tracks);
                    return Effect::None;
                }
                Self::back(m);
                return Effect::None;
            }
            Action::ContextMenu => {
                self.open_context(m, tracks);
                return Effect::None;
            }
            Action::WheelClockwise(steps) => return self.wheel(m, tracks, steps as i32),
            Action::WheelCounterClockwise(steps) => return self.wheel(m, tracks, -(steps as i32)),
            Action::NavigateUp => {
                self.move_focus(m, tracks, -1);
                return Effect::None;
            }
            Action::NavigateDown => {
                self.move_focus(m, tracks, 1);
                return Effect::None;
            }
            Action::NavigateLeft | Action::NavigateRight => return Effect::None,
            Action::Select => {}
            _ => return Effect::None,
        }
        if m.navigation.modal.is_some() {
            return self.modal_action(m, tracks);
        }
        if m.screen == Screen::NowPlaying && m.current().is_some() {
            // Select on Now Playing opens its options; the wheel is volume
            // (or seeks: Customize > Now Playing Wheel).
            Self::open_modal(m, Modal::ContextMenu);
            m.navigation.context_target = Some(0);
            return Effect::None;
        }
        if self.row_count(m, tracks) == 0 {
            return match m.screen {
                Screen::Queue => {
                    Self::home(m);
                    Effect::None
                }
                s if Self::is_catalog(s) && !m.library.scanning => Effect::ScanLibrary,
                _ => Effect::None,
            };
        }
        let Some(row) = self
            .row_at(m, tracks, m.navigation.focus)
            .filter(|row| row.enabled)
        else {
            return Effect::None;
        };
        self.select_row(m, tracks, &row.key.clone())
    }

    pub(crate) fn open_context(&mut self, m: &mut AppModel, tracks: &[Track]) {
        if m.navigation.modal.is_some() {
            return;
        }
        if m.screen == Screen::NowPlaying {
            if m.current().is_some() {
                Self::open_modal(m, Modal::ContextMenu);
                m.navigation.context_target = Some(0);
            }
            return;
        }
        let Some(row) = self
            .row_at(m, tracks, m.navigation.focus)
            .filter(|row| row.enabled)
        else {
            return;
        };
        m.navigation.context_target = Some(m.navigation.focus);
        m.navigation.context_key = Some(row.key);
        if self.context_rows(m, tracks).is_empty() {
            m.navigation.context_target = None;
            m.navigation.context_key = None;
        } else {
            Self::open_modal(m, Modal::ContextMenu);
        }
    }

    /// Context-specific wheel policy. Only long library lists honour the
    /// input layer's acceleration suggestion; every other context moves
    /// exactly one row, item or value per detent.
    pub(crate) fn wheel(&mut self, m: &mut AppModel, tracks: &[Track], suggested: i32) -> Effect {
        let direction = suggested.signum();
        if m.navigation.modal.is_none() && m.screen == Screen::NowPlaying && m.current().is_some() {
            // Customize > Now Playing Wheel (#34): Seek moves through the song
            // as holding Previous/Next does, small steps first. A station has
            // no place to move to: its wheel stays the volume.
            let seek = m.settings.now_playing_wheel == reborn_core::NowPlayingWheel::Seek
                && !m.current().is_some_and(Track::is_radio);
            return if seek {
                Effect::Seek(i64::from(direction))
            } else {
                Effect::AdjustVolume(WHEEL_VOLUME_STEP * direction as i8)
            };
        }
        // The country list (≈240) scrolls as fast as a long library list.
        let accelerate = m.navigation.modal.is_none()
            && (Self::is_catalog(m.screen) || m.screen == Screen::TimeZone)
            && self.row_count(m, tracks) >= ACCELERATED_LIST_MINIMUM;
        let now = Instant::now();
        // #30: while the letter shows, each detent goes to the next or the
        // previous letter, and the letter stays `FAST_SCROLL_LINGER` after
        // the last one: turning back after overshooting stays by letter.
        let lettered = accelerate && self.fast_scroll_until.is_some_and(|until| now < until);
        if let Some(target) = lettered
            .then(|| self.letter_step(m, tracks, direction > 0))
            .flatten()
        {
            self.fast_scroll_until = Some(now + FAST_SCROLL_LINGER);
            // At an end one row at a time: a rested wheel wraps as always.
            let delta = match target as i32 - m.navigation.focus as i32 {
                0 => direction,
                jump => jump,
            };
            self.move_focus(m, tracks, delta);
            return Effect::None;
        }
        if accelerate && suggested.abs() >= LETTER_SCROLL_STEPS {
            self.fast_scroll_until = Some(now + FAST_SCROLL_LINGER);
        }
        self.move_focus(m, tracks, if accelerate { suggested } else { direction });
        Effect::None
    }

    /// Where a letter jump from the focused row lands (#30), in a list that
    /// shows its letter (`row_letter`); None elsewhere.
    pub(crate) fn letter_step(
        &self,
        m: &AppModel,
        tracks: &[Track],
        forward: bool,
    ) -> Option<usize> {
        if !Self::lettered(m) {
            return None;
        }
        let mut c = self.catalog.borrow_mut();
        self.ensure_catalog(&mut c, m, tracks);
        let by_artist = m.settings.album_order == reborn_core::AlbumOrder::Artist;
        c.letter_jump(tracks, m.navigation.focus, forward, m.screen, by_artist)
    }

    /// The A to Z lists that show the letter while scrolling fast: Songs
    /// (a genre's keep the library's order), Artists, Albums (a genre's too;
    /// #56: by artist, the artist's letter) and Folders.
    pub(crate) fn lettered(m: &AppModel) -> bool {
        m.navigation.modal.is_none()
            && match m.screen {
                Screen::Artists | Screen::Albums | Screen::Folders => true,
                Screen::Tracks => m.navigation.filter.is_empty(),
                _ => false,
            }
    }

    /// The letter row `index` files under in a lettered list (`initial`).
    pub(crate) fn row_letter(&self, m: &AppModel, tracks: &[Track], index: usize) -> Option<char> {
        if !Self::lettered(m) {
            return None;
        }
        let mut c = self.catalog.borrow_mut();
        self.ensure_catalog(&mut c, m, tracks);
        let by_artist = m.settings.album_order == reborn_core::AlbumOrder::Artist;
        c.letter(tracks, index, m.screen, by_artist)
    }

    pub(crate) fn move_focus(&mut self, m: &mut AppModel, tracks: &[Track], delta: i32) {
        if delta == 0 {
            return;
        }
        let modal = m.navigation.modal.is_some();
        let rows = if modal {
            Some(self.modal_rows(m, tracks))
        } else if !Self::is_catalog(m.screen) && m.screen != Screen::Queue {
            Some(self.rows(m, tracks))
        } else {
            None
        };
        let count = rows
            .as_ref()
            .map(|r| r.len())
            .unwrap_or_else(|| self.row_count(m, tracks));
        if count == 0 {
            return;
        }
        // A scale (an equalizer band's dB) never jumps from one end to the other.
        let scale = matches!(
            m.navigation.modal,
            Some(Modal::EqBand(_) | Modal::SleepTimer)
        );
        let wraps = match m.settings.wrap_around {
            _ if scale => false,
            WrapAround::Off => false,
            WrapAround::Menus => modal || !Self::is_long_list(m.screen),
            WrapAround::Everywhere => true,
        };
        // A long list wraps only from a single step taken at its end after a
        // rest; a short menu at once.
        let now = Instant::now();
        let rested = count < ACCELERATED_LIST_MINIMUM
            || self
                .last_step
                .is_none_or(|at| now.duration_since(at) >= WRAP_REST);
        self.last_step = Some(now);
        // Up Next's parts, for its headings below (before `focus` borrows).
        let up_next = (!modal && m.screen == Screen::Queue).then(|| self.up_next(m, tracks));
        let focus = if modal {
            &mut m.navigation.modal_focus
        } else {
            &mut m.navigation.focus
        };
        let last = count as i32 - 1;
        let target = *focus as i32 + delta;
        let at_end = (*focus == 0 && delta < 0) || (*focus as i32 == last && delta > 0);
        let wrap = wraps && rested && delta.abs() == 1 && at_end;
        let mut next = if wrap {
            if delta > 0 {
                0
            } else {
                last as usize
            }
        } else {
            target.clamp(0, last) as usize
        };
        if let Some(rows) = rows {
            let step = delta.signum();
            let mut skipped = 0;
            while !rows[next].enabled {
                let mut n = next as i32 + step;
                if wraps && rested && delta.abs() == 1 && (n < 0 || n > last) {
                    n = n.rem_euclid(count as i32);
                }
                skipped += 1;
                if n < 0 || n > last || skipped >= count {
                    return;
                }
                next = n as usize;
            }
        }
        if !modal && matches!(m.screen, Screen::Search | Screen::Queue) {
            // The search page's and Up Next's headings are not stops.
            let catalog = self.catalog.borrow();
            let heading = |i: usize| match &up_next {
                Some(layout) => layout.row(i).is_some_and(up_next::Row::heading),
                None => catalog
                    .rows
                    .get(i)
                    .is_some_and(|r| r.search_kind() == catalog::SearchKind::Heading),
            };
            while heading(next) {
                let n = next as i32 + delta.signum();
                if n < 0 || n > last {
                    next = *focus;
                    break;
                }
                next = n as usize;
            }
        }
        *focus = next;
        if !modal && self.native_search(m, tracks) {
            self.scroll_search(m, tracks);
        } else if !modal && self.native_header_page(m, tracks) {
            let (lead, count) = (self.header_lead(m, tracks), self.row_count(m, tracks));
            m.navigation.scroll =
                components::header_list_first(next, lead, count, m.navigation.scroll);
        } else if !modal {
            let visible = self.visible_count(m, tracks);
            if next < m.navigation.scroll {
                m.navigation.scroll = next;
            } else if next >= m.navigation.scroll + visible {
                m.navigation.scroll = next + 1 - visible;
            }
        }
    }

    pub(crate) fn select_row(&mut self, m: &mut AppModel, tracks: &[Track], key: &str) -> Effect {
        if key == "back" {
            Self::back(m);
            return Effect::None;
        }
        if let Some(book) = key.strip_prefix("book:") {
            Self::go(m, Screen::Book, book);
            return Effect::LoadChapters(book.to_owned());
        }
        if m.screen == Screen::Book {
            let start = match key {
                "book_resume" => Some(reborn_core::BookStart::Resume),
                "book_restart" => Some(reborn_core::BookStart::Beginning),
                _ => key
                    .strip_prefix("chapter:")
                    .and_then(|n| n.parse().ok())
                    .map(reborn_core::BookStart::Chapter),
            };
            if let Some(start) = start {
                let book = m.navigation.filter.clone();
                Self::go(m, Screen::NowPlaying, "");
                return Effect::PlayBook(book, start);
            }
        }
        if key == "home" {
            Self::home(m);
            return Effect::None;
        }
        if key.starts_with("value:") {
            Self::go(m, Screen::ValueDetail, key.trim_start_matches("value:"));
            return Effect::None;
        }
        if let Some(id) = key.strip_prefix("diag:") {
            Self::go(m, Screen::DiagnosticSection, id);
            return Effect::None;
        }
        if key == "update_now" || key == "update_install" {
            if !update_charge_ok(m) {
                self.flash("Connect the charger or charge to half first");
                return Effect::None;
            }
            self.update_run = true;
            return Effect::Platform(if key == "update_now" {
                PlatformTask::UpdateStage
            } else {
                PlatformTask::UpdateApply
            });
        }
        if let Some(name) = key.strip_prefix("task:") {
            return Effect::Platform(platform_task(name));
        }
        if key == "report_toggle" {
            let on = m.platform.snapshot.reports.as_ref().and_then(|r| r.enabled) == Some(true);
            return Effect::Platform(PlatformTask::ReportEnable(!on));
        }
        if key == "report_info" {
            Self::go(m, Screen::ErrorReports, "info");
            return Effect::None;
        }
        if key == "report_save" {
            Self::go(m, Screen::ErrorReports, "save");
            return Effect::None;
        }
        if let Some(destination) = key
            .strip_prefix("report_dest:")
            .and_then(reborn_core::platform::ReportDestination::from_argument)
        {
            // A saved report always has everything, so there is no topic to
            // choose. Back to the Error Reports page (not the picker): it shows
            // the progress, then where the file went.
            while !(m.screen == Screen::ErrorReports && m.navigation.filter.is_empty())
                && !m.navigation.history.is_empty()
            {
                Self::back(m);
            }
            return Effect::Platform(PlatformTask::ReportSave(destination));
        }
        if key == "report_send" {
            // Every report holds everything: no topic to choose. The page shows
            // the progress, then the result.
            return Effect::Platform(PlatformTask::ReportSend);
        }
        if let Some(index) = key
            .strip_prefix("eq_band:")
            .and_then(|s| s.parse::<usize>().ok())
        {
            let defaults = reborn_core::flat_eq_bands();
            let bands = if m.settings.eq_bands.is_empty() {
                &defaults
            } else {
                &m.settings.eq_bands
            };
            if let Some(band) = bands.get(index) {
                let focus = (band.gain_db.round().clamp(-12., 12.) as i32 + 12) as usize;
                Self::open_modal(m, Modal::EqBand(index));
                m.navigation.modal_focus = focus;
            }
            return Effect::None;
        }
        if let Some(name) = key.strip_prefix("confirm:") {
            if let Some(action) = confirm_action(name) {
                Self::open_modal(m, Modal::Confirm(action));
            }
            return Effect::None;
        }
        if key == "eq_custom" {
            Self::go(m, Screen::Equalizer, "custom");
            return Effect::None;
        }
        if key == "eq_profile_saved" {
            return Effect::None; // already playing; its file is gone
        }
        if key == "eq_profile" {
            // The list shows what is on the storage now (files arrive over MTP).
            Self::go(m, Screen::Equalizer, "profile");
            return Effect::ScanEqProfiles;
        }
        if let Some(index) = key
            .strip_prefix("eq_profile_file:")
            .and_then(|s| s.parse::<usize>().ok())
        {
            return self
                .eq_profiles
                .get(index)
                .map_or(Effect::None, |f| Effect::ImportEqProfile(f.path.clone()));
        }
        if key == "date_time" {
            // From the clock as it reads now.
            date_time::begin(self, m);
            Self::go(m, Screen::DateTime, "");
            return Effect::None;
        }
        if key.starts_with("date_") && date_time::network_time(m) {
            return Effect::None;
        }
        if let Some(field) = key.strip_prefix("date_field:").and_then(date_time::field) {
            let focus = date_time::picker(self, m, field).1;
            Self::open_modal(m, Modal::DateField(field));
            m.navigation.modal_focus = focus;
            return Effect::None;
        }
        if key == "date_set" {
            if m.platform.busy.is_some() {
                self.flash("Try again in a moment.");
                return Effect::None;
            }
            return match date_time::instant(self, m) {
                Ok(utc) => {
                    // Back to Display, whose row shows the clock it sets.
                    self.date_draft = None;
                    Self::back(m);
                    Effect::Platform(PlatformTask::SetClock(utc))
                }
                Err(why) => {
                    self.flash(why);
                    Effect::None
                }
            };
        }
        if key == "time_zone" {
            // Open at the choice in use.
            Self::go(m, Screen::TimeZone, "");
            m.navigation.focus = time_zone::focus(self, m);
            return Effect::None;
        }
        if let Some(code) = key.strip_prefix("tz_country:") {
            let Some((_, _, zones)) = reborn_core::clock::countries()
                .iter()
                .find(|(c, _, _)| *c == code)
            else {
                return Effect::None;
            };
            // One zone: chosen at once; several: the country's cities.
            if let Some(zone) = time_zone::single_zone(zones) {
                return Effect::SetTimeZone(zone.into());
            }
            Self::go(m, Screen::TimeZone, format!("country:{code}"));
            m.navigation.focus = time_zone::focus(self, m);
            return Effect::None;
        }
        if key == "version" {
            self.version_taps = self.version_taps.saturating_add(1);
            if self.version_taps >= VERSION_TAPS {
                self.version_taps = 0;
                Self::go(m, Screen::SettingsSystem, "");
            } else if self.version_taps >= 3 {
                self.flash(format!(
                    "{} more taps for the service menu",
                    VERSION_TAPS - self.version_taps
                ));
            }
            return Effect::None;
        }
        if key == "shuffle:collection" {
            return self.play_filtered(m, tracks, ShuffleMode::Songs);
        }
        if key == "playlists" {
            Self::go(m, Screen::Playlists, "");
            self.playlists.loading = !self.playlists.loaded;
            return Effect::Playlist(Box::new(reborn_core::PlaylistRequest::Refresh));
        }
        if matches!(m.screen, Screen::Playlists | Screen::Playlist) {
            return self.playlist_select(m, tracks, key);
        }
        if key == "search" || key == catalog::SEARCH_FIELD {
            self.open_library_search(m, tracks);
            // The playlists are searched too: read them if not yet.
            if !self.playlists.loaded && !self.playlists.loading {
                self.playlists.loading = true;
                return Effect::Playlist(Box::new(reborn_core::PlaylistRequest::Refresh));
            }
            return Effect::None;
        }
        if key == catalog::SEARCH_PLAY_ALL {
            return self.play_filtered(m, tracks, ShuffleMode::Off);
        }
        if let Some(screen) = route(key) {
            Self::go(m, screen, "");
            if screen == Screen::Balance {
                // 21 rows: open on the current balance, not Left +10 dB.
                m.navigation.focus =
                    (m.settings.balance + reborn_core::MAX_BALANCE).max(0) as usize;
            }
            // The picker lists what is installed now (skins arrive over MTP).
            return if screen == Screen::Theme {
                Effect::ScanThemes
            } else {
                Effect::None
            };
        }
        if let Some(effect) = self.setting(m, key) {
            Self::keep_home_focus(m, tracks, &effect);
            return effect;
        }
        match m.screen {
            Screen::Artists => Self::go(m, Screen::Artist, key),
            Screen::Genres => Self::go(m, Screen::Albums, key),
            // A genre's albums start with all of its songs.
            Screen::Albums if key == "all_songs" => {
                Self::go(m, Screen::Tracks, m.navigation.filter.clone())
            }
            Screen::Albums => Self::go(m, Screen::Album, key),
            Screen::Folders if !key.starts_with("track:") => Self::go(m, Screen::Folders, key),
            // An album, or on a genre's album its whole tracklist (#50).
            Screen::Artist | Screen::Album if key.starts_with("album:") => {
                Self::go(m, Screen::Album, key)
            }
            // See All: that section alone, Back returns (#44).
            Screen::Search if key.starts_with(catalog::SEARCH_SEE_ALL) => {
                let (query, _) = catalog::search_parts(&m.navigation.filter);
                let section = &key[catalog::SEARCH_SEE_ALL.len()..];
                let filter = format!("{query}\u{1f}{section}");
                Self::go(m, Screen::Search, filter);
            }
            Screen::Search if key.starts_with("playlist:") => {
                let path = key
                    .strip_prefix("playlist:")
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| self.playlists.lists.get(i))
                    .map(|p| p.path.to_string_lossy().into_owned());
                if let Some(path) = path {
                    Self::go(m, Screen::Playlist, path);
                }
            }
            Screen::Search if key.starts_with("artist:") => Self::go(m, Screen::Artist, key),
            Screen::Search if key.starts_with("album:") => Self::go(m, Screen::Album, key),
            Screen::Search => return self.play_track(m, tracks, key),
            Screen::Artist | Screen::Album | Screen::Tracks | Screen::Folders => {
                if key == "artist_albums" {
                    Self::go(m, Screen::Albums, m.navigation.filter.clone());
                    return Effect::None;
                }
                if key.starts_with("track:") {
                    return self.play_track(m, tracks, key);
                }
                if key == "play:collection" {
                    return self.play_filtered(m, tracks, ShuffleMode::Off);
                }
            }
            Screen::Queue => {
                // A song from the history plays now, before what was playing.
                if let Some(lib) = key.strip_prefix("hist:").and_then(|v| v.parse().ok()) {
                    Self::go(m, Screen::NowPlaying, "");
                    return Effect::PlayNow(lib);
                }
                if let Some(index) = self.queue_target(m, Some(key)) {
                    Self::go(m, Screen::NowPlaying, "");
                    if index == m.queue_position {
                        return Effect::None;
                    }
                    return Effect::PlayQueue(index);
                }
            }
            Screen::Bluetooth => return self.bluetooth_select(m, key),
            Screen::Wifi => return self.wifi_select(m, key),
            Screen::Radio | Screen::RadioStations | Screen::RadioCategories | Screen::RadioInfo => {
                return self.radio_select(m, key)
            }
            _ => {}
        }
        Effect::None
    }
}
