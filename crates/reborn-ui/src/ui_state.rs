//! The UI's own state: notices, animation and motion, covers and thumbnails, the volume line and the power hold.
use crate::*;

impl Ui {
    /// Borrow the authoritative library for one synchronous semantic action.
    /// Restore it before the caller executes effects; never clone every Track
    /// merely to satisfy disjoint borrowing of navigation and the library.
    pub fn model_action(&mut self, model: &mut AppModel, action: Action) -> Effect {
        let tracks = std::mem::take(&mut model.library.tracks);
        let effect = self.action(model, &tracks, action);
        model.library.tracks = tracks;
        effect
    }

    /// An asynchronous operation may finish after navigation moved elsewhere.
    /// Keep that current page as the parent of the result, just like Select.
    pub fn show_operation_result(&mut self, model: &mut AppModel) {
        Self::go(model, Screen::DiagnosticSection, "result");
    }

    pub fn normalize(&self, m: &mut AppModel, tracks: &[Track]) {
        let count = self.row_count(m, tracks);
        m.navigation.focus = m.navigation.focus.min(count.saturating_sub(1));
        if m.screen == Screen::Queue {
            self.keep_up_next_focus(m, tracks);
        }
        if count > 0
            && !self
                .row_at(m, tracks, m.navigation.focus)
                .is_some_and(|r| r.enabled)
        {
            // The nearest row that takes the focus, below first.
            let focus = m.navigation.focus;
            m.navigation.focus = (focus..count)
                .chain((0..focus).rev())
                .find(|i| self.row_at(m, tracks, *i).is_some_and(|r| r.enabled))
                .unwrap_or(0);
        }
        if m.screen == Screen::Queue {
            self.follow_moving(m, tracks);
            self.up_next_focus.replace(
                self.row_at(m, tracks, m.navigation.focus)
                    .map(|r| (m.navigation.focus, r.key)),
            );
        }
        if m.navigation.modal.is_some() {
            let rows = self.modal_rows(m, tracks);
            if rows.is_empty() {
                m.navigation.modal = None;
            } else {
                m.navigation.modal_focus = m.navigation.modal_focus.min(rows.len() - 1);
                if !rows[m.navigation.modal_focus].enabled {
                    m.navigation.modal_focus = rows.iter().position(|r| r.enabled).unwrap_or(0);
                }
            }
        }
    }
    pub fn flash(&mut self, message: impl Into<String>) {
        self.notice = message.into();
        self.notice_until = Some(Instant::now() + Duration::from_millis(1800));
    }

    pub fn expire_notice(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if self.notice_until.is_some_and(|until| now >= until) {
            self.notice.clear();
            self.notice_until = None;
            changed = true;
        }
        if self.volume_until.is_some_and(|until| now >= until) {
            self.volume_until = None;
            changed = true;
        }
        if self.fast_scroll_until.is_some_and(|until| now >= until) {
            self.fast_scroll_until = None;
            changed = true;
        }
        changed
    }

    /// Whether something on screen changes with time (a fade, a ring), so the
    /// caller keeps redrawing.
    pub fn animating(&self) -> bool {
        self.volume_until.is_some()
            || self.power_hold.is_some()
            || self.spinning
            || self.gliding()
            || glyph_cache::pending()
    }

    /// Motion that wants the panel's full frame rate: Cover Flow's glide and
    /// the cover's flight, and (Beta 7.2) every eased value on its way, a
    /// screen sliding in and the playing wave. Spinners stay at the calm rate.
    pub fn gliding(&self) -> bool {
        self.flow.borrow().moving()
            || self.transition_progress().is_some()
            || (self.ui_motion
                && (self.motion.moving()
                    || self.slide.borrow().is_some()
                    || self.folding.borrow().is_some()
                    || self.download_shown))
    }

    /// Motion a button started (a glide, a slide, Cover Flow): it earns the
    /// short Interactive CPU lease. The playing wave alone does not: its
    /// ≈3-ms frames fit the lowest clock (Beta 7.4: less heat on Now Playing).
    pub fn responding(&self) -> bool {
        self.flow.borrow().moving()
            || self.transition_progress().is_some()
            || (self.ui_motion
                && (self.motion.settling()
                    || self.slide.borrow().is_some()
                    || self.folding.borrow().is_some()))
    }

    /// `target` eased in time under `key` (see `motion::Motion::ease`);
    /// exactly `target` without UI motion.
    pub(crate) fn ease(
        &self,
        key: &str,
        start: Option<f32>,
        target: f32,
        rate: f32,
        settle: f32,
    ) -> f32 {
        if !self.ui_motion {
            return target;
        }
        self.motion
            .ease(key, start, target, rate, settle, Instant::now())
    }

    /// Now Playing's changes since its last frame: (new song, new play state).
    pub(crate) fn np_changes(&self, song: i64, playing: bool) -> (bool, bool) {
        let before = self.np_seen.replace(Some((song, playing)));
        match before {
            Some((s, p)) => (s != song, p != playing),
            None => (false, false),
        }
    }

    /// The frame draws motion that goes on by itself (the playing wave).
    pub(crate) fn mark_live(&self) {
        if self.ui_motion {
            self.motion.mark_live();
        }
    }

    /// The playing position for this frame: the last report moved on by the
    /// time since, while playing (reports come a few times a second).
    pub(crate) fn shown_position(&self, m: &AppModel) -> u64 {
        let now = Instant::now();
        let (reported, at) = match self.position_seen.get() {
            Some((p, at)) if p == m.position_ms => (p, at),
            _ => {
                self.position_seen.set(Some((m.position_ms, now)));
                (m.position_ms, now)
            }
        };
        let playing = m.playback == reborn_core::PlaybackState::Playing;
        if !self.ui_motion || !playing {
            return reported;
        }
        let ahead = (now.saturating_duration_since(at).as_millis() as u64).min(1_500);
        // A faster (or slower) book moves through its time at its speed.
        let speed = m.book_part().map_or(1., |(book, _)| book.speed);
        let ahead = (ahead as f32 * speed) as u64;
        let length = m.current().map_or(u64::MAX, |t| t.duration_ms.max(1));
        (reported + ahead).min(length)
    }

    /// Every album's songs and length, counted once per library.
    pub(crate) fn count_albums(&self, tracks: &[Track]) {
        let library = crate::library_stamp(tracks);
        let mut totals = self.album_totals.borrow_mut();
        if totals.as_ref().is_some_and(|(l, _)| *l == library) {
            return;
        }
        let mut map: HashMap<(String, String), crate::AlbumTotal> = HashMap::new();
        for t in tracks.iter().filter(|t| t.is_music()) {
            let entry = map
                .entry((t.album_artist.clone(), t.album.clone()))
                .or_default();
            entry.songs += 1;
            entry.ms += t.duration_ms;
            entry.year = crate::earliest_year(entry.year, t.year);
        }
        *totals = Some((library, map));
    }

    /// While the screen is off, the work a long library's lists do when
    /// first opened is done ahead, one piece per call (each up to a second
    /// or two on the player with 120 000 songs): Music's counts, Songs,
    /// Artists, Albums, Genres, Folders, the search, the album lines and the
    /// playlists' songs.
    /// Returns whether a piece was done (more may follow).
    pub fn prewarm(&self, m: &AppModel, tracks: &[Track]) -> bool {
        const STEPS: usize = 9;
        let library = crate::library_stamp(tracks);
        let (seen, step) = self.prewarmed.get();
        let step = if seen == Some(library) { step } else { 0 };
        if step >= STEPS || tracks.is_empty() {
            return false;
        }
        match step {
            0 => {
                self.library_counts(m, tracks);
            }
            1..=4 => {
                let screen = [
                    Screen::Tracks,
                    Screen::Artists,
                    Screen::Albums,
                    Screen::Genres,
                ][step - 1];
                self.catalog.borrow_mut().prewarm(m, tracks, screen);
            }
            5 => self.catalog.borrow_mut().folders.ensure(tracks),
            6 => self.catalog.borrow_mut().index.prepare(tracks),
            7 => self.count_albums(tracks),
            // The playlists matched to the library (Playlists, the search),
            // once they are read.
            _ if !self.playlists.loaded => return false,
            _ => {
                for p in &self.playlists.lists {
                    self.facts(tracks, p);
                }
            }
        }
        self.prewarmed.set((Some(library), step + 1));
        true
    }

    /// "9 songs · 38 min" for the album with filter `key`, computed once per
    /// album and library rather than on every animation frame.
    pub(crate) fn album_detail(&self, m: &AppModel, tracks: &[Track], key: &str) -> String {
        let library = crate::library_stamp(tracks);
        let mut detail = self.detail.borrow_mut();
        if let Some((l, k, text)) = detail.as_ref() {
            if *l == library && k == key {
                return text.clone();
            }
        }
        // A plain album from the totals counted once; one opened from a
        // genre (only that genre's songs) is counted on its own.
        let text = match album_parts(key) {
            Some((artist, album, None)) => {
                self.count_albums(tracks);
                let totals = self.album_totals.borrow();
                let total = totals
                    .as_ref()
                    .and_then(|(_, map)| map.get(&(artist.to_owned(), album.to_owned())))
                    .copied()
                    .unwrap_or_default();
                views_library::album_line(total)
            }
            _ => views_library::album_detail(m, tracks, key),
        };
        *detail = Some((library, key.to_owned(), text.clone()));
        text
    }

    /// Cover Flow's fractional centre for this frame, advanced in time.
    pub(crate) fn flow_position(&self, m: &AppModel, focus: usize) -> f32 {
        self.flow.borrow_mut().advance(
            (m.screen, m.navigation.filter.clone()),
            focus,
            self.motion_fixed,
            Instant::now(),
        )
    }

    /// The running cover flight and its eased 0..1 progress.
    pub(crate) fn transition_progress(&self) -> Option<(flow::TransitionKind, f32)> {
        let t = self.transition.get()?;
        if self.motion_fixed {
            return None;
        }
        match t.progress(Instant::now()) {
            Some(p) => Some((t.kind, p)),
            None => {
                self.transition.set(None);
                None
            }
        }
    }

    /// The centre cover wants its full-resolution image once the row rests.
    pub(crate) fn request_cover(&self, key: u64, track_index: usize) {
        if self.collection_art.get() != Some(key) && self.flow.borrow().rested(120, Instant::now())
        {
            self.cover_request.set(Some((key, track_index)));
        }
    }

    /// The full-resolution cover the screen wants: art key and library track.
    /// The caller loads it and reports it with `collection_art`.
    pub fn take_cover_request(&self) -> Option<(u64, usize)> {
        self.cover_request.take()
    }

    /// The download bar's fill for this frame: it glides toward the reported
    /// value (reports come four times a second), and starts from empty when
    /// the step changes from download to check.
    pub(crate) fn download_fill(&self, target: f32, verifying: bool) -> f32 {
        let target = target.clamp(0., 1.);
        if self.motion_fixed || self.spin_fixed.is_some() {
            return target;
        }
        let now = Instant::now();
        let (shown, step, at) = self.download_bar.get();
        let shown = if step != verifying { 0. } else { shown };
        let dt = at.map_or(0., |t| now.duration_since(t).as_secs_f32().min(0.1));
        let next = if target < shown {
            target
        } else {
            shown + (target - shown) * (1. - (-dt * 6.).exp())
        };
        self.download_bar.set((next, verifying, Some(now)));
        next
    }

    /// A 0..1 phase for spinners, one turn about every 1.6 s.
    pub fn spin_phase(&self) -> f32 {
        if let Some(fixed) = self.spin_fixed {
            return fixed;
        }
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let elapsed = START.get_or_init(Instant::now).elapsed().as_millis() % 1600;
        elapsed as f32 / 1600.
    }

    /// A computer connected since the last call (#28): one haptic tick, as
    /// its message shows. Taking it clears it.
    pub fn take_connect_tick(&mut self) -> bool {
        std::mem::take(&mut self.connect_tick)
    }

    /// Watch platform state the UI reacts to by itself: a computer connecting,
    /// a Bluetooth request finishing or failing, the battery running low,
    /// work that shows a spinner and the clock's minute turning over.
    /// Returns whether the screen needs redrawing.
    pub fn observe(&mut self, m: &AppModel) -> bool {
        let before = (
            self.usb_state,
            self.alert.clone(),
            self.bt_busy.is_some(),
            self.spinning,
        );
        let usb = m.platform.snapshot.usb;
        if self.usb_state != Some(usb) {
            // Beta 7.3: a computer never covers the player; a short message
            // says what happened and the status bar shows the cable.
            if let Some(was) = self.usb_state {
                let attached = |s| matches!(s, UsbTransfer::Ready | UsbTransfer::Copying);
                let connected = attached(usb) && !attached(was);
                if usb == UsbTransfer::Copying && was != UsbTransfer::Copying {
                    self.flash("Copying files from the computer…");
                } else if connected {
                    self.flash("Connected to a computer");
                }
                self.connect_tick |= connected;
            }
            self.usb_state = Some(usb);
        }
        let level = match m.platform.battery.level {
            LowBattery::Normal => 0,
            LowBattery::Low => 1,
            LowBattery::Critical => 2,
            LowBattery::ShuttingDown => 3,
        };
        if level == 0 {
            self.battery_acked = 0;
        } else if level > self.battery_acked && level < 3 && self.alert.is_none() {
            self.alert = Some(Alert::BatteryLow);
            self.alert_focus = 0;
        }
        if let Some((path, name, operation, since)) = self.bt_busy.clone() {
            let connected = self
                .bluetooth
                .devices
                .iter()
                .any(|d| d.path == path && d.connected);
            if connected || since.elapsed() > Duration::from_secs(45) {
                self.bt_busy = None;
            } else if self.bluetooth.problem.is_some()
                && since.elapsed() > Duration::from_millis(800)
            {
                self.bt_busy = None;
                self.alert = Some(Alert::BluetoothFailed {
                    path,
                    name,
                    operation,
                });
                self.alert_focus = 1;
            }
        }
        let downloading = m.platform.snapshot.update.phase
            == reborn_core::platform::UpdatePhase::Downloading
            || m.platform.busy == Some(reborn_core::PlatformTask::UpdateStage);
        self.spinning = m.library.scanning || downloading || self.bt_busy.is_some();
        // Before Beta 7.6 the download drew at the spinners' calm rate
        // (≤ 29 fps): its sweep stepped ≈8 px a frame.
        self.download_shown =
            downloading && !m.screen_off && (self.update_run || m.screen == Screen::Update);
        let clock = self.clock(m);
        let ticked = clock != self.clock_shown && !m.screen_off;
        self.clock_shown = clock;
        before
            != (
                self.usb_state,
                self.alert.clone(),
                self.bt_busy.is_some(),
                self.spinning,
            )
            || self.spinning
            || ticked
    }

    /// The status-bar clock in the chosen zone, once the system time is set.
    pub fn clock(&self, m: &AppModel) -> Option<String> {
        time_zone::read(self, m).map(|clock| clock.time)
    }

    /// The short date skins show (`{date}`), once the system time is set.
    pub fn date(&self, m: &AppModel) -> Option<String> {
        time_zone::read(self, m).map(|clock| clock.date)
    }

    /// The device a Bluetooth request is waiting on, with its operation.
    pub(crate) fn bluetooth_busy(&self) -> Option<(&str, &str)> {
        self.bt_busy
            .as_ref()
            .map(|(_, name, operation, _)| (name.as_str(), operation.as_str()))
    }

    pub(crate) fn alert_focus(&self) -> usize {
        self.alert_focus
    }

    /// Input for the overlays above every screen. `Some` means consumed.
    pub(crate) fn overlay_action(&mut self, m: &AppModel, action: Action) -> Option<Effect> {
        if let Some(alert) = self.alert.clone() {
            let buttons = match alert {
                Alert::BluetoothFailed { .. } => 2,
                Alert::BatteryLow => 1,
            };
            return Some(match action {
                Action::WheelClockwise(_) | Action::NavigateDown => {
                    self.alert_focus = (self.alert_focus + 1).min(buttons - 1);
                    Effect::None
                }
                Action::WheelCounterClockwise(_) | Action::NavigateUp => {
                    self.alert_focus = self.alert_focus.saturating_sub(1);
                    Effect::None
                }
                Action::Back | Action::Home => {
                    self.dismiss_alert(m);
                    Effect::None
                }
                Action::Select => {
                    let retry = self.alert_focus == 1;
                    self.dismiss_alert(m);
                    match alert {
                        Alert::BluetoothFailed {
                            path, operation, ..
                        } if retry => {
                            self.bt_busy = self
                                .bluetooth
                                .devices
                                .iter()
                                .find(|d| d.path == path)
                                .map(|d| {
                                    (
                                        path.clone(),
                                        d.name.clone(),
                                        operation.clone(),
                                        Instant::now(),
                                    )
                                });
                            Effect::BluetoothDevice { path, operation }
                        }
                        _ => Effect::None,
                    }
                }
                _ => Effect::None,
            });
        }
        None
    }

    pub(crate) fn dismiss_alert(&mut self, m: &AppModel) {
        if self.alert == Some(Alert::BatteryLow) {
            self.battery_acked = match m.platform.battery.level {
                LowBattery::Low => 1,
                LowBattery::Critical => 2,
                _ => 3,
            };
        }
        self.alert = None;
        self.alert_focus = 0;
    }

    /// The turn-off hold completed: the effect to run, once.
    pub fn power_hold_complete(&mut self) -> Option<Effect> {
        if self.power_hold_progress().is_some_and(|p| p >= 1.) {
            self.power_hold = None;
            return Some(Effect::PowerOff);
        }
        None
    }

    /// Artists, albums and songs in the library, recounted only when it or
    /// Customize > Artists changes.
    pub(crate) fn library_counts(&self, m: &AppModel, tracks: &[Track]) -> [usize; 4] {
        let stamp = crate::library_stamp(tracks);
        let list = m.settings.artists_list;
        if let Some((seen, seen_list, counts)) = *self.counts.borrow() {
            if seen == stamp && seen_list == list {
                return counts;
            }
        }
        let mut artists = std::collections::BTreeSet::new();
        let mut albums = std::collections::BTreeSet::new();
        let mut genres = std::collections::BTreeSet::new();
        let mut songs = 0;
        for t in tracks.iter().filter(|t| t.is_music()) {
            if list == reborn_core::ArtistsList::AlbumArtists {
                artists.insert(album_artist_of(t));
            } else {
                artists.extend(t.artists());
            }
            albums.insert((t.album.as_str(), t.album_artist.as_str()));
            genres.extend(t.genres());
            songs += 1;
        }
        let counts = [artists.len(), albums.len(), songs, genres.len()];
        *self.counts.borrow_mut() = Some((stamp, list, counts));
        counts
    }

    /// The accent of every control and state (UI v4): a skin's fixed accent,
    /// otherwise the one theme accent. It never follows the cover, so lists,
    /// Settings and sheets look the same with every song.
    pub fn accent(&self) -> u32 {
        self.skin
            .as_deref()
            .and_then(skin::Skin::accent)
            .unwrap_or(theme::color::ACCENT)
    }

    /// The playing cover's colour: Now Playing's backdrop and the Home cover
    /// card only, and the accent skins without a fixed one are remapped to.
    pub fn cover_accent(&self) -> u32 {
        if let Some(fixed) = self.skin.as_deref().and_then(skin::Skin::accent) {
            return fixed;
        }
        if self.accent == 0 {
            theme::color::ACCENT
        } else {
            self.accent
        }
    }

    /// Show the volume line for about a second.
    pub fn show_volume(&mut self) {
        self.volume_until = Some(Instant::now() + VOLUME_LINE);
    }

    /// Whether the volume line is visible now, and how opaque (0..=1).
    pub fn volume_visibility(&self) -> f32 {
        match self.volume_until {
            Some(until) => {
                let left = until.saturating_duration_since(Instant::now());
                (left.as_secs_f32() / VOLUME_FADE.as_secs_f32()).clamp(0., 1.)
            }
            None => 0.,
        }
    }

    /// Progress (0..=1) of the Power hold toward turning the player off.
    pub fn power_hold_progress(&self) -> Option<f32> {
        self.power_hold
            .map(|since| (since.elapsed().as_secs_f32() / POWER_HOLD.as_secs_f32()).min(1.))
    }

    /// Replace the set of thumbnails the renderer holds.
    pub fn set_resident_thumbs(&mut self, keys: impl IntoIterator<Item = u64>) {
        self.resident = keys.into_iter().collect();
    }

    pub(crate) fn thumb_resident(&self, key: u64) -> bool {
        self.resident.contains(&key)
    }

    pub(crate) fn want_thumb(&self, key: u64, track_index: usize) {
        let mut wanted = self.wanted.borrow_mut();
        if wanted.len() < 64 && !wanted.iter().any(|(k, _)| *k == key) {
            wanted.push((key, track_index));
        }
    }

    pub(crate) fn want_logo(&self, key: u64, url: &str) {
        let mut wanted = self.wanted_logos.borrow_mut();
        if wanted.len() < 32 && !wanted.iter().any(|(k, _)| *k == key) {
            wanted.push((key, url.to_owned()));
        }
    }

    /// Station logos the last `draw` could not show yet, with their address.
    pub fn take_wanted_logos(&self) -> Vec<(u64, String)> {
        std::mem::take(&mut self.wanted_logos.borrow_mut())
    }

    /// Thumbnails the last `draw` could not show yet, with a library track
    /// index that carries each cover.
    pub fn take_wanted_thumbs(&self) -> Vec<(u64, usize)> {
        std::mem::take(&mut self.wanted.borrow_mut())
    }
}
