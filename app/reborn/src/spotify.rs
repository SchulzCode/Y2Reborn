//! Spotify on the player: the sign-in, the lists, and spotifyd as a source.
//!
//! One queue entry (`Track::spotify`) stands for whatever spotifyd plays;
//! its tags, length and position follow spotifyd's MPRIS metadata. Play,
//! Pause, Previous, Next and the wheel's seek go to spotifyd, and its sound
//! comes back through Reborn's own decoder and output
//! (`reborn_platform::spotify::Audio`, opened in `playback::BoundDecoder`).
//!
//! spotifyd starts when Spotify is first used after a sign-in (opening its
//! page warms it up), not at boot. Network work runs in short worker threads
//! that answer on a channel, as Last.fm's does. No token is ever logged.
use crate::Runtime;
use reborn_core::{PlaybackState, SpotifyRequest, Track};
use reborn_observability::Level;
use reborn_platform::spotify::{self as sp, Command, Daemon, DeviceCode, Paths, Phase, Poll};
use reborn_ui::{SpotifyList, SpotifyRow};
use serde_json::json;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

/// A sign-in gives up after this long (Spotify's codes last about as long).
const SIGN_IN_FOR: Duration = Duration::from_secs(15 * 60);
/// After a command, spotifyd's own state is not followed back for a moment:
/// it may still report the state from before.
const SETTLE: Duration = Duration::from_secs(3);
/// spotifyd that stopped by itself is started again at most this often.
const RESTART_AFTER: [u64; 4] = [5, 15, 60, 300];
/// One step of the wheel or a held button while Spotify plays.
const SEEK_STEP_MS: i64 = 10_000;

pub enum Reply {
    Code(Result<DeviceCode, sp::Error>),
    Polled(Result<Poll, sp::Error>),
    /// spotifyd's credentials are ready (or why not).
    Prepared(Result<(), sp::Error>),
    Listing {
        key: String,
        more: bool,
        result: Result<sp::Listing, String>,
    },
}

/// A worker's answer, tagged with the request sequence it belongs to.
type Tagged = (u64, Reply);

pub struct Spotify {
    paths: Paths,
    available: bool,
    signed_in: bool,
    /// A sign-in in progress and when it started.
    code: Option<(DeviceCode, Instant)>,
    next_poll: Instant,
    polling: bool,
    busy: bool,
    daemon: Option<Daemon>,
    /// Credentials are being prepared before spotifyd starts.
    preparing: bool,
    next_start: Instant,
    failures: usize,
    /// What to play once spotifyd answers: the command and the entry.
    pending: Option<(Command, Track)>,
    /// When Reborn last told spotifyd what to do.
    commanded: Option<Instant>,
    /// spotifyd's play state has differed from the player's since then:
    /// (Spotify is the entry, the player plays, spotifyd plays).
    differs: Option<((bool, bool, bool), Instant)>,
    /// spotifyd's phase last shown on the Spotify page.
    phase_seen: Option<Phase>,
    /// The song last shown (MPRIS track id) and the refusals already logged.
    song_id: String,
    refused_seen: u64,
    /// The newest request per list: older replies are dropped.
    sequence: u64,
    requests: std::collections::HashMap<String, u64>,
    replies: (Sender<Tagged>, Receiver<Tagged>),
}
impl Spotify {
    pub fn load(root: &std::path::Path) -> Self {
        let paths = Paths::new(root);
        Self {
            signed_in: sp::load_account(&paths).is_some(),
            available: sp::installed(),
            paths,
            code: None,
            next_poll: Instant::now(),
            polling: false,
            busy: false,
            daemon: None,
            preparing: false,
            next_start: Instant::now(),
            failures: 0,
            pending: None,
            commanded: None,
            differs: None,
            phase_seen: None,
            song_id: String::new(),
            refused_seen: 0,
            sequence: 0,
            requests: Default::default(),
            replies: channel(),
        }
    }
    fn ready(&self) -> bool {
        self.daemon
            .as_ref()
            .is_some_and(|d| d.status().phase == Phase::Ready)
    }
    /// Tell spotifyd; false when it isn't running.
    fn send(&mut self, command: Command) -> bool {
        self.commanded = Some(Instant::now());
        self.daemon.as_ref().is_some_and(|d| d.send(command))
    }
    fn settled(&self) -> bool {
        self.commanded.is_none_or(|at| at.elapsed() > SETTLE)
    }
}

/// The Spotify context a song plays in: its playlist or album, Liked Songs,
/// else its own album (top songs, an artist's songs).
pub fn context_for(key: &str, list: &SpotifyList, row: &SpotifyRow) -> Option<String> {
    let (kind, id) = reborn_ui::spotify_list_kind(key);
    match kind {
        "playlist-tracks" => Some(format!("spotify:playlist:{id}")),
        "album-tracks" => Some(format!("spotify:album:{id}")),
        "liked-songs" if list.context.is_some() => list.context.clone(),
        _ if !row.id.is_empty() => Some(format!("spotify:album:{}", row.id)),
        _ => None,
    }
}

/// A list key as the helper's command and argument.
fn command_of(key: &str) -> Option<(&str, &str)> {
    let (kind, id) = reborn_ui::spotify_list_kind(key);
    let command = match kind {
        "playlist-tracks" | "album-tracks" | "artist-albums" | "artist-top-tracks"
        | "artist-all-tracks" => kind,
        "playlists" | "liked-songs" | "saved-albums" | "followed-artists" if id.is_empty() => kind,
        _ => return None,
    };
    Some((command, id))
}

impl Runtime {
    fn spotify_spawn(&self, name: &str, job: impl FnOnce() -> Reply + Send + 'static) -> bool {
        let reply = self.spotify.replies.0.clone();
        let sequence = self.spotify.sequence;
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let _ = reply.send((sequence, job()));
            })
            .is_ok()
    }
    /// What the Spotify page shows (sign-in, problems, the song).
    pub(crate) fn spotify_view(&mut self) {
        let s = &self.spotify;
        let approval = s.code.as_ref().map(|(c, _)| c.url.clone());
        let code = if approval == self.ui.spotify.approval {
            self.ui.spotify.code.clone()
        } else {
            approval
                .as_deref()
                .and_then(|url| reborn_ui::qr::encode(url.as_bytes()))
        };
        let view = &mut self.ui.spotify;
        view.available = s.available;
        view.signed_in = s.signed_in;
        view.user_code = s
            .code
            .as_ref()
            .map(|(c, _)| c.user_code.clone())
            .unwrap_or_default();
        view.approval = approval;
        view.code = code;
        view.busy = s.busy;
        view.problem = match s.daemon.as_ref().map(Daemon::status) {
            Some(status) => match status.phase {
                Phase::Failed(reason) => Some(reason),
                Phase::Starting => Some("Starting Spotify…".into()),
                Phase::Ready => None,
            },
            None if s.preparing => Some("Starting Spotify…".into()),
            None => None,
        };
        self.dirty.mark_render();
    }

    pub(crate) fn spotify_request(&mut self, request: SpotifyRequest) -> Result<(), String> {
        let result = self.spotify_request_inner(request);
        self.spotify_view();
        result
    }

    fn spotify_request_inner(&mut self, request: SpotifyRequest) -> Result<(), String> {
        let needs_network = !matches!(request, SpotifyRequest::Cancel | SpotifyRequest::Disconnect);
        if needs_network && !self.spotify.available {
            return Err("Spotify isn't installed on this player".into());
        }
        if needs_network && !self.online() {
            return Err("Connect to Wi-Fi first".into());
        }
        match request {
            SpotifyRequest::Connect if self.spotify.busy => Ok(()),
            // Certificate checks need the right date.
            SpotifyRequest::Connect if self.trusted_unix().is_none() => {
                Err("The clock isn't set yet; try again in a moment".into())
            }
            SpotifyRequest::Connect => {
                self.spotify.busy =
                    self.spotify_spawn("spotify-code", || Reply::Code(sp::request_code()));
                Ok(())
            }
            SpotifyRequest::Cancel => {
                self.spotify.code = None;
                Ok(())
            }
            SpotifyRequest::Disconnect => {
                self.spotify_forget();
                self.ui.flash("Disconnected from Spotify");
                Ok(())
            }
            SpotifyRequest::Load(key) => self.spotify_load(key, None),
            SpotifyRequest::More(key) => {
                let cursor = self.ui.spotify.lists.get(&key).and_then(|l| l.next.clone());
                match cursor {
                    Some(cursor) => self.spotify_load(key, Some(cursor)),
                    None => Ok(()),
                }
            }
            SpotifyRequest::Play { list, index } => {
                let l = self
                    .ui
                    .spotify
                    .lists
                    .get(&list)
                    .ok_or("Spotify list not found")?;
                let row = l.rows.get(index).ok_or("Spotify song not found")?;
                if row.uri.is_empty() {
                    return Err("Spotify can't play this song".into());
                }
                let command = match context_for(&list, l, row) {
                    Some(context) => Command::PlayInContext {
                        context,
                        track: row.uri.clone(),
                    },
                    None => Command::OpenUri(row.uri.clone()),
                };
                let mut track = Track::spotify(&row.uri, &row.title);
                track.artist = row.detail.clone();
                self.spotify_play(command, track, &l.title.clone())
            }
            SpotifyRequest::PlayAll(list) => {
                let l = self
                    .ui
                    .spotify
                    .lists
                    .get(&list)
                    .ok_or("Spotify list not found")?;
                let (kind, id) = reborn_ui::spotify_list_kind(&list);
                let uri = match kind {
                    "playlist-tracks" => format!("spotify:playlist:{id}"),
                    "album-tracks" => format!("spotify:album:{id}"),
                    _ => match (&l.context, l.rows.first()) {
                        (Some(context), Some(first)) => {
                            let title = l.title.clone();
                            let command = Command::PlayInContext {
                                context: context.clone(),
                                track: first.uri.clone(),
                            };
                            let track = Track::spotify(&first.uri, &first.title);
                            return self.spotify_play(command, track, &title);
                        }
                        _ => return Err("Spotify can't play this list".into()),
                    },
                };
                let title = l.title.clone();
                let track = Track::spotify(&uri, &title);
                self.spotify_play(Command::OpenUri(uri), track, &title)
            }
        }
    }

    fn spotify_load(&mut self, key: String, cursor: Option<String>) -> Result<(), String> {
        let (command, arg) = command_of(&key).ok_or("unknown Spotify list")?;
        let (command, arg) = (command.to_owned(), arg.to_owned());
        self.spotify.sequence += 1;
        self.spotify
            .requests
            .insert(key.clone(), self.spotify.sequence);
        let paths = self.spotify.paths.clone();
        let more = cursor.is_some();
        let now = self.trusted_unix().unwrap_or(0).max(0) as u64;
        let started = self.spotify_spawn("spotify-list", move || Reply::Listing {
            result: sp::browse(&paths, &command, &arg, cursor.as_deref().unwrap_or(""), now),
            key,
            more,
        });
        if !started {
            return Err("Couldn't ask Spotify".into());
        }
        // Lists come faster while spotifyd starts alongside.
        self.spotify_start();
        Ok(())
    }

    /// Start playing on spotifyd; the queue becomes the Spotify entry.
    fn spotify_play(&mut self, command: Command, track: Track, source: &str) -> Result<(), String> {
        self.save_book();
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) && !self.model.current().is_some_and(Track::is_spotify)
        {
            self.model.invalidate();
            self.playback.stop(self.model.generation);
        }
        self.model.replace_queue(vec![track.clone()], 0)?;
        let label = if source.starts_with("Spotify") {
            source.to_owned()
        } else {
            format!("Spotify: {source}")
        };
        self.model.set_source(label, None);
        self.model.position_ms = 0;
        // Until spotifyd answers and the sound arrives.
        self.model.playback = PlaybackState::Buffering;
        self.art = false;
        self.spotify.song_id.clear();
        self.ui.spotify.song = Some((track.title.clone(), track.artist.clone()));
        self.spotify.pending = Some((command, track));
        self.log.emit(
            Level::Info,
            "spotify",
            "play_requested",
            "Spotify playback requested",
            None,
            json!({"running": self.spotify.daemon.is_some()}),
        );
        self.spotify_start();
        self.spotify_flush_pending()
    }

    /// Hand the waiting play to spotifyd once it answers, then listen.
    fn spotify_flush_pending(&mut self) -> Result<(), String> {
        // Paused while it waited: it starts when Play is pressed.
        if !self.spotify.ready() || self.model.playback == PlaybackState::Paused {
            return Ok(());
        }
        let Some((command, _)) = self.spotify.pending.take() else {
            return Ok(());
        };
        if !self.spotify.send(command) {
            return Err("Spotify isn't answering".into());
        }
        if self.model.current().is_some_and(Track::is_spotify) {
            self.load()?;
        }
        Ok(())
    }

    /// Start spotifyd if it should run and does not (credentials first).
    fn spotify_start(&mut self) {
        let s = &self.spotify;
        if !s.available
            || !s.signed_in
            || s.daemon.is_some()
            || s.preparing
            || Instant::now() < s.next_start
            || !self.online()
        {
            return;
        }
        let Some(now) = self.trusted_unix() else {
            return;
        };
        let paths = self.spotify.paths.clone();
        self.spotify.preparing = self.spotify_spawn("spotify-prepare", move || {
            Reply::Prepared(sp::prepare(&paths, now.max(0) as u64))
        });
    }

    /// Before local music starts: Spotify pauses (both would play at once).
    pub(crate) fn spotify_yield(&mut self) {
        if self.model.current().is_some_and(Track::is_spotify) {
            return;
        }
        self.spotify.pending = None;
        if self
            .spotify
            .daemon
            .as_ref()
            .is_some_and(|d| d.status().playing)
        {
            self.spotify.send(Command::Pause);
        }
    }

    /// Every pause of the Spotify entry (Play/Pause, Stop, the sleep timer,
    /// headphones gone) pauses spotifyd too.
    pub(crate) fn spotify_pause(&mut self) {
        if self.model.current().is_some_and(Track::is_spotify) && self.spotify.daemon.is_some() {
            self.spotify.send(Command::Pause);
        }
    }

    /// Play on the paused Spotify entry: spotifyd first, then the output as
    /// for any song (`None`); after a restart of the player, Spotify starts
    /// over with the entry (`Some`). Pausing goes through `spotify_pause`.
    pub(crate) fn spotify_toggle(&mut self) -> Option<Result<(), String>> {
        if !self.model.current().is_some_and(Track::is_spotify)
            || matches!(
                self.model.playback,
                PlaybackState::Playing | PlaybackState::Buffering
            )
        {
            return None;
        }
        if self.spotify.pending.is_some() {
            self.model.playback = PlaybackState::Buffering;
            return Some(self.spotify_flush_pending());
        }
        if self.spotify.daemon.is_some() {
            self.spotify.send(Command::Play);
            return None;
        }
        let track = self.model.current().cloned()?;
        let uri = track.path.to_string_lossy().into_owned();
        // An entry Spotify started by itself names nothing to start again.
        if !uri.starts_with("spotify:") || uri == "spotify:" {
            return Some(Err("Choose something to play in Spotify".into()));
        }
        if !self.spotify.signed_in {
            return Some(Err("Connect Spotify first".into()));
        }
        if !self.online() {
            return Some(Err("Connect to Wi-Fi first".into()));
        }
        let source = self.model.queue_source.clone();
        Some(self.spotify_play(Command::OpenUri(uri), track, &source))
    }

    /// Previous/Next on the Spotify entry move through spotifyd's own queue.
    pub(crate) fn spotify_step(&mut self, delta: i32) -> bool {
        if !self.model.current().is_some_and(Track::is_spotify) {
            return false;
        }
        self.spotify.send(if delta > 0 {
            Command::Next
        } else {
            Command::Previous
        });
        true
    }

    pub(crate) fn spotify_seek(&mut self, delta: i64) -> bool {
        if !self.model.current().is_some_and(Track::is_spotify) {
            return false;
        }
        let step = delta.signum() * SEEK_STEP_MS;
        self.spotify.send(Command::Seek(step));
        self.model.position_ms = self.model.position_ms.saturating_add_signed(step);
        self.dirty.mark_render();
        true
    }

    fn spotify_forget(&mut self) {
        if self.model.current().is_some_and(Track::is_spotify) {
            self.model.invalidate();
            self.playback.stop(self.model.generation);
            self.model.playback = PlaybackState::Stopped;
        }
        self.spotify.daemon = None;
        self.spotify.pending = None;
        self.spotify.code = None;
        if let Err(e) = sp::forget(&self.spotify.paths) {
            self.log.emit(
                Level::Warn,
                "spotify",
                "forget_failed",
                "Spotify sign-in not removed",
                None,
                json!({"error": e.to_string()}),
            );
        }
        self.spotify.signed_in = false;
        self.ui.spotify.lists.clear();
        self.ui.spotify.song = None;
        self.log.emit(
            Level::Info,
            "spotify",
            "disconnected",
            "Spotify disconnected",
            None,
            json!({}),
        );
    }

    /// Replies, the approval poll, spotifyd's state and the Spotify entry.
    pub(crate) fn spotify_tick(&mut self) {
        let replies: Vec<Tagged> = self.spotify.replies.1.try_iter().collect();
        for (sequence, reply) in replies {
            self.spotify_reply(sequence, reply);
        }
        if let Some(started) = self.spotify.code.as_ref().map(|c| c.1) {
            if started.elapsed() > SIGN_IN_FOR {
                self.spotify.code = None;
                self.ui.flash("The Spotify sign-in timed out");
                self.spotify_view();
            } else if !self.spotify.polling
                && Instant::now() >= self.spotify.next_poll
                && self.online()
            {
                let now = self.trusted_unix().unwrap_or(0).max(0) as u64;
                if let Some((mut code, _)) = self.spotify.code.clone() {
                    self.spotify.polling = self.spotify_spawn("spotify-poll", move || {
                        Reply::Polled(sp::poll(&mut code, now))
                    });
                }
            }
        }
        // Opening Spotify warms spotifyd up for the first song.
        if matches!(
            self.model.screen,
            reborn_core::Screen::Spotify | reborn_core::Screen::SpotifyList
        ) && self.spotify.code.is_none()
        {
            self.spotify_start();
        }
        self.spotify_follow();
    }

    /// spotifyd's side: it stopped, it answers now, its song and state.
    fn spotify_follow(&mut self) {
        let Some(status) = self.spotify.daemon.as_ref().map(Daemon::status) else {
            return;
        };
        if self.spotify.phase_seen.as_ref() != Some(&status.phase) {
            self.spotify.phase_seen = Some(status.phase.clone());
            self.spotify_view();
        }
        if let Phase::Failed(reason) = &status.phase {
            let wait = RESTART_AFTER[self.spotify.failures.min(RESTART_AFTER.len() - 1)];
            self.log.emit(
                Level::Warn,
                "spotify",
                "daemon_stopped",
                reason,
                None,
                json!({"restart_in_s": wait}),
            );
            self.spotify.failures += 1;
            self.spotify.daemon = None;
            self.spotify.next_start = Instant::now() + Duration::from_secs(wait);
            if self.model.current().is_some_and(Track::is_spotify)
                && self.model.playback == PlaybackState::Playing
            {
                self.pause();
            }
            self.spotify_view();
            return;
        }
        if status.refused != self.spotify.refused_seen {
            self.spotify.refused_seen = status.refused;
            if let Some((command, error)) = &status.last_refusal {
                self.log.emit(
                    Level::Warn,
                    "spotify",
                    "command_refused",
                    "spotifyd refused a command",
                    None,
                    json!({"command": command, "error": error}),
                );
            }
        }
        if status.phase == Phase::Ready && self.spotify.pending.is_some() {
            self.spotify.failures = 0;
            if let Err(e) = self.spotify_flush_pending() {
                self.ui.flash(e);
            }
            self.spotify_view();
        }
        let current = self.model.current().is_some_and(Track::is_spotify);
        if !current && self.spotify_cover.is_some() {
            self.spotify_cover = None;
        }
        if current {
            if let Some(song) = &status.song {
                let position = self.model.queue_position;
                if self.spotify.song_id != song.id {
                    self.spotify.song_id = song.id.clone();
                    if let Some(track) = self.model.queue.get_mut(position) {
                        track.title = song.title.clone();
                        track.artist = song.artist.clone();
                        track.album = song.album.clone();
                        track.album_artist = song.artist.clone();
                        track.duration_ms = song.length_ms;
                    }
                    self.ui.spotify.song = Some((song.title.clone(), song.artist.clone()));
                    self.spotify_art(&song.art_url);
                    self.dirty.mark_render();
                }
                let position_ms = status.position_now();
                if position_ms / 1000 != self.model.position_ms / 1000 {
                    self.dirty.mark_render();
                }
                self.model.position_ms = position_ms;
            }
        }
        if !status.active || !self.spotify.settled() || self.spotify.pending.is_some() {
            self.spotify.differs = None;
            return;
        }
        let ours = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        let state = (current, ours, status.playing);
        // A song change can show "Paused" for a moment: only a difference
        // that lasts is followed.
        let lasting = match self.spotify.differs {
            Some((seen, since)) if seen == state => since.elapsed() >= Duration::from_secs(2),
            _ => {
                self.spotify.differs = Some((state, Instant::now()));
                false
            }
        };
        if !lasting {
            return;
        }
        self.spotify.differs = None;
        match state {
            // Paused from a phone (Spotify Connect) or at the end of the list.
            (true, true, false) => self.pause(),
            // Resumed from a phone (not after a failure: that would retry
            // for as long as spotifyd plays).
            (true, false, true) if self.model.playback == PlaybackState::Paused => {
                if let Err(e) = self.load() {
                    self.ui.flash(e);
                }
            }
            // Started from a phone while the player was quiet: listen.
            (false, false, true) => {
                let track = Track::spotify("spotify:", "Spotify");
                if self.model.replace_queue(vec![track], 0).is_ok() {
                    self.model.set_source("Spotify", None);
                    self.spotify.song_id.clear();
                    if let Err(e) = self.load() {
                        self.ui.flash(e);
                    }
                }
            }
            // Local music plays: Spotify waits.
            (false, true, true) => {
                self.spotify.send(Command::Pause);
            }
            _ => {}
        }
    }

    /// The cover spotifyd names, fetched into the cache like a station logo
    /// (the artwork pass picks it up while `art` is unset).
    fn spotify_art(&mut self, url: &str) {
        self.art = false;
        self.radio_art_pending = None;
        self.spotify_cover =
            (!url.is_empty()).then(|| (reborn_core::logo_key(url), url.to_owned()));
    }

    fn spotify_reply(&mut self, sequence: u64, reply: Reply) {
        match reply {
            Reply::Code(result) => {
                self.spotify.busy = false;
                match result {
                    Ok(code) => {
                        self.spotify.next_poll = Instant::now() + code.interval;
                        self.spotify.code = Some((code, Instant::now()));
                    }
                    Err(e) => self.ui.flash(e.message()),
                }
            }
            Reply::Polled(result) => {
                self.spotify.polling = false;
                let interval = self
                    .spotify
                    .code
                    .as_ref()
                    .map_or(Duration::from_secs(5), |c| c.0.interval);
                match result {
                    Ok(Poll::Pending) => {
                        self.spotify.next_poll = Instant::now() + interval;
                    }
                    Ok(Poll::Approved(account)) => {
                        self.spotify.code = None;
                        if let Err(e) = sp::sign_in(&self.spotify.paths, &account) {
                            self.ui
                                .flash(format!("Couldn't save the Spotify sign-in: {e}"));
                            return self.spotify_view();
                        }
                        self.spotify.signed_in = true;
                        self.spotify.daemon = None;
                        self.spotify.failures = 0;
                        self.spotify.next_start = Instant::now();
                        self.ui.spotify.lists.clear();
                        self.ui.flash("Spotify connected");
                        self.log.emit(
                            Level::Info,
                            "spotify",
                            "connected",
                            "Spotify connected",
                            None,
                            json!({}),
                        );
                        self.spotify_start();
                    }
                    // A slow_down reply already lengthened the interval.
                    Err(sp::Error::Offline(_)) => {
                        self.spotify.next_poll = Instant::now() + interval * 2;
                    }
                    Err(e) => {
                        self.spotify.code = None;
                        self.ui.flash(e.message());
                    }
                }
            }
            Reply::Prepared(result) => {
                self.spotify.preparing = false;
                match result {
                    Ok(()) => {
                        let name = self.spotify_device_name();
                        match Daemon::start(&self.spotify.paths, &name) {
                            Ok(daemon) => {
                                self.log.emit(
                                    Level::Info,
                                    "spotify",
                                    "daemon_started",
                                    "spotifyd started",
                                    None,
                                    json!({}),
                                );
                                self.spotify.daemon = Some(daemon);
                            }
                            Err(e) => {
                                self.spotify.next_start =
                                    Instant::now() + Duration::from_secs(RESTART_AFTER[1]);
                                self.ui.flash(format!("Spotify couldn't start: {e}"));
                            }
                        }
                    }
                    // The sign-in was revoked: sign in again.
                    Err(sp::Error::Refused(reason)) => {
                        self.log.emit(
                            Level::Warn,
                            "spotify",
                            "sign_in_refused",
                            "Spotify refused the stored sign-in",
                            None,
                            json!({"reason": reason}),
                        );
                        self.spotify_forget();
                        self.ui.flash("Spotify needs you to connect again");
                    }
                    Err(sp::Error::Offline(_)) => {
                        self.spotify.next_start =
                            Instant::now() + Duration::from_secs(RESTART_AFTER[0]);
                    }
                }
            }
            Reply::Listing { key, more, result } => {
                if self.spotify.requests.get(&key) != Some(&sequence) {
                    return;
                }
                let list = self.ui.spotify.lists.entry(key).or_default();
                list.loading = false;
                match result {
                    Ok(listing) => {
                        let rows = listing.items.iter().map(|r| {
                            SpotifyRow::from_label(
                                &r.label,
                                r.uri.as_deref().unwrap_or(""),
                                r.id.as_deref().unwrap_or(""),
                            )
                        });
                        if more {
                            list.rows.extend(rows);
                        } else {
                            list.rows = rows.collect();
                        }
                        list.next = listing.next;
                        if listing.context.is_some() {
                            list.context = listing.context;
                        }
                        list.error = None;
                    }
                    Err(e) => list.error = Some(e),
                }
            }
        }
        self.spotify_view();
    }

    /// How phones list the player in Spotify Connect.
    fn spotify_device_name(&self) -> String {
        "Y2Reborn".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> SpotifyRow {
        SpotifyRow {
            title: "Song".into(),
            detail: "Artist".into(),
            uri: "spotify:track:t".into(),
            id: id.into(),
        }
    }

    #[test]
    fn songs_play_in_the_list_they_were_chosen_from() {
        let list = SpotifyList::default();
        assert_eq!(
            context_for("playlist-tracks:p1", &list, &row("a")).as_deref(),
            Some("spotify:playlist:p1")
        );
        assert_eq!(
            context_for("album-tracks:a1", &list, &row("a")).as_deref(),
            Some("spotify:album:a1")
        );
        let liked = SpotifyList {
            context: Some("spotify:user:me:collection".into()),
            ..Default::default()
        };
        assert_eq!(
            context_for("liked-songs", &liked, &row("a")).as_deref(),
            Some("spotify:user:me:collection")
        );
        // Top songs play on within their own album; no album, no context.
        assert_eq!(
            context_for("artist-top-tracks:x", &list, &row("alb")).as_deref(),
            Some("spotify:album:alb")
        );
        assert_eq!(context_for("artist-top-tracks:x", &list, &row("")), None);
    }

    #[test]
    fn list_keys_map_to_helper_commands() {
        assert_eq!(command_of("playlists"), Some(("playlists", "")));
        assert_eq!(
            command_of("playlist-tracks:abc"),
            Some(("playlist-tracks", "abc"))
        );
        assert_eq!(command_of("artist:abc"), None);
        assert_eq!(command_of("playlists:x"), None);
        assert_eq!(command_of("rm"), None);
    }
}
