//! Normal product pages, built from typed state only.
//!
//! A page is an optional hero message (empty, off, error or progress state),
//! a few static facts, and the interactive rows the wheel moves through.
//! Engineering detail is never placed here; it belongs to `diagnostics`.
use crate::{
    components::output_label, BluetoothDeviceView, Item, NetworkView, RowKind, Ui, WifiStatus,
};
use reborn_core::{
    platform::{
        human_bytes, ChargingState, HealthLevel, LowBattery, SdCard, SleepPhase, SleepProblem,
        UpdatePhase, UpdateProblem, UsbTransfer, VolumeSpace, VolumeState, WakeReason, WifiProblem,
    },
    AppModel, MediaSource, PlatformTask, Screen, Settings, Track, HOME_HIDEABLE, HOME_PINNABLE,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hero {
    pub title: String,
    pub body: String,
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub hero: Option<Hero>,
    pub facts: Vec<(String, String)>,
    pub rows: Vec<Item>,
    /// One calm status line above the footer (radio progress or problem).
    pub status: Option<String>,
}
impl Page {
    fn rows(rows: Vec<Item>) -> Self {
        Self {
            rows,
            ..Default::default()
        }
    }
    fn hero(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            hero: Some(Hero {
                title: title.into(),
                body: body.into(),
            }),
            ..Default::default()
        }
    }
    fn with_rows(mut self, rows: Vec<Item>) -> Self {
        self.rows = rows;
        self
    }
    fn fact(mut self, label: &str, value: impl Into<String>) -> Self {
        self.facts.push((label.into(), value.into()));
        self
    }
}

/// Home (UI v4): the six places a listener starts from, Audiobooks once
/// the library holds a book (Beta 8.19, can be switched off), and any Music
/// lists the owner pinned after Music. Any but Now Playing and Settings can
/// be taken off (Y2Reborn #52, #55). Spotify stands there only on a player
/// that has it installed.
pub(crate) fn home_rows(s: &Settings, tracks: &[Track], spotify: bool) -> Vec<Item> {
    let mut rows = vec![row("Now Playing", "now_playing", "")];
    if s.home_shows("music") {
        rows.push(row("Music", "music", ""));
    }
    if s.audiobooks_on_home && tracks.iter().any(|t| t.book && t.online) {
        rows.push(row("Audiobooks", "audiobooks", ""));
    }
    rows.extend(
        HOME_PINNABLE
            .iter()
            .filter(|(key, _)| s.home_pins.iter().any(|p| p == key))
            .map(|(key, label)| row(label, key, "")),
    );
    rows.extend(
        HOME_HIDEABLE[1..]
            .iter()
            .filter(|(key, _)| s.home_shows(key) && (spotify || *key != "spotify"))
            .map(|(key, label)| row(label, key, "")),
    );
    rows.push(row("Settings", "settings", ""));
    rows
}

/// Music: the library's lists, each with how much it holds.
fn music(ui: &Ui, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
    let [artists, albums, songs, genres] = ui.library_counts(m, tracks);
    let counted = |n: usize, one: &str, many: &str| {
        if n == 0 {
            String::new()
        } else {
            crate::components::count(n, one, many)
        }
    };
    vec![
        row("Artists", "artists", counted(artists, "artist", "artists")),
        row("Albums", "albums", counted(albums, "album", "albums")),
        row("Songs", "tracks", counted(songs, "song", "songs")),
        row("Genres", "genres", counted(genres, "genre", "genres")),
        row("Folders", "folders", ""),
    ]
    .into_iter()
    // Playlists and Search taken off Home stay at hand here.
    .chain(
        [("playlists", "Playlists"), ("search", "Search")]
            .into_iter()
            .filter(|(key, _)| !m.settings.home_shows(key))
            .map(|(key, label)| row(label, key, "")),
    )
    .collect()
}

/// Customize's Home Menu value: "Standard", the pinned lists, what is
/// hidden, or how many of each.
fn home_summary(m: &AppModel) -> String {
    let pinned: Vec<&str> = HOME_PINNABLE
        .iter()
        .filter(|(key, _)| m.settings.home_pins.iter().any(|p| p == key))
        .map(|(_, label)| *label)
        .collect();
    let mut hidden: Vec<&str> = HOME_HIDEABLE
        .iter()
        .filter(|(key, _)| !m.settings.home_shows(key))
        .map(|(_, label)| *label)
        .collect();
    if !m.settings.audiobooks_on_home {
        hidden.insert(usize::from(!m.settings.home_shows("music")), "Audiobooks");
    }
    match (pinned.len(), hidden.len()) {
        (0, 0) => "Standard".into(),
        (1 | 2, 0) => pinned.join(", "),
        (n, 0) => format!("{n} lists added"),
        (0, 1 | 2) => format!("{} hidden", hidden.join(", ")),
        (0, n) => format!("{n} hidden"),
        (p, h) => format!("{p} added, {h} hidden"),
    }
}

fn row(label: &str, key: &str, secondary: impl Into<String>) -> Item {
    Item::new(label, key).with_secondary(secondary)
}
fn on_off(on: bool) -> &'static str {
    if on {
        "On"
    } else {
        "Off"
    }
}
fn disabled(mut item: Item, enabled: bool) -> Item {
    item.enabled = enabled;
    item
}

pub fn page(ui: &Ui, m: &AppModel, tracks: &[Track]) -> Page {
    match m.screen {
        Screen::Home => Page::rows(home_rows(&m.settings, tracks, ui.spotify.available)),
        Screen::Music => Page::rows(music(ui, m, tracks)),
        // Every place Home can show, in Home's order (#52, #55).
        Screen::HomeMenu => {
            let shown = |(key, label): &(&str, &str)| {
                row(label, &format!("home_show:{key}"), "")
                    .with_kind(RowKind::Toggle(m.settings.home_shows(key)))
            };
            Page::rows(
                std::iter::once(shown(&HOME_HIDEABLE[0]))
                    .chain(std::iter::once(
                        row("Audiobooks", "home_audiobooks", "")
                            .with_kind(RowKind::Toggle(m.settings.audiobooks_on_home)),
                    ))
                    .chain(HOME_PINNABLE.iter().map(|(key, label)| {
                        let on = m.settings.home_pins.iter().any(|p| p == key);
                        row(label, &format!("home_pin:{key}"), "").with_kind(RowKind::Toggle(on))
                    }))
                    .chain(
                        HOME_HIDEABLE[1..]
                            .iter()
                            .filter(|(key, _)| ui.spotify.available || *key != "spotify")
                            .map(shown),
                    )
                    .collect(),
            )
        }
        Screen::NowPlaying if m.current().is_none() => Page::hero(
            "Nothing Playing",
            "Choose an album, artist or song to start listening.",
        )
        .with_rows(vec![Item::new("Browse Music", "home")]),
        Screen::NowPlaying => Page::default(),
        Screen::TrackInfo => track_info(m, tracks),
        Screen::Settings => settings(ui, m),
        Screen::Wifi => radio_firmware(m, "Wi-Fi").unwrap_or_else(|| wifi(ui)),
        Screen::Radio => radio_menu(ui),
        Screen::RadioStations => radio_stations(ui),
        Screen::RadioCategories => radio_categories(ui),
        Screen::RadioInfo => radio_info(ui),
        Screen::Bluetooth => radio_firmware(m, "Bluetooth").unwrap_or_else(|| bluetooth(ui, m)),
        Screen::PcTransfer => pc_transfer(m),
        Screen::SettingsAudio => sound(ui, m),
        Screen::LastFm => lastfm(ui, m),
        Screen::Spotify => spotify(ui),
        Screen::SpotifyList => spotify_list(ui, m),
        Screen::Equalizer => equalizer(ui, m),
        Screen::Crossfade => crossfade(m),
        Screen::Balance => balance(m),
        Screen::ScreenTimeout => screen_timeout(m),
        Screen::Output => output(ui, m),
        Screen::HeadphoneLevel => headphone_level(m),
        Screen::DacFilter => dac_filter(m),
        Screen::ErrorReports => error_reports(m),
        Screen::Theme => theme(ui, m),
        Screen::SettingsPlayback => Page::rows(vec![
            row("Shuffle", "shuffle", m.settings.shuffle_mode().label()),
            row("Repeat", "repeat", crate::repeat_label(m.settings.repeat)),
            row("Gapless Playback", "gapless", on_off(m.settings.gapless_enabled)),
            row("Crossfade", "crossfade", crate::crossfade_label(m.settings.crossfade_ms)),
        ]),
        Screen::SettingsLibrary => library(m, tracks),
        Screen::SettingsDisplay => {
            let mut rows = vec![];
            if m.platform.brightness_available {
                rows.push(row(
                    "Brightness",
                    "brightness",
                    format!("{}%", m.settings.brightness),
                ));
            }
            rows.push(
                row(
                    "Screen Off After",
                    "screen_timeout",
                    crate::timeout_label(m.settings.screen_timeout_seconds),
                )
                .with_kind(RowKind::Chevron),
            );
            rows.push(
                row("Time Zone", "time_zone", crate::time_zone::summary(m))
                    .with_kind(RowKind::Chevron),
            );
            // #53: a player without Wi-Fi keeps the time set here.
            rows.push(
                row("Date & Time", "date_time", crate::date_time::summary(ui, m))
                    .with_kind(RowKind::Chevron),
            );
            Page::rows(rows)
        }
        // Beta 8.18: how Reborn behaves, apart from how the screen looks.
        // Each row names what it changes; Select steps its value (8.36).
        Screen::Customize => Page::rows(vec![
            row("Home Menu", "home_menu", home_summary(m)).with_kind(RowKind::Chevron),
            row("Cover Flow", "album_view", m.settings.album_view.label()),
            // #56: by title (as before) or by artist, Cover Flow too.
            row("Album Order", "album_order", m.settings.album_order.label()),
            row("Artists", "artists_list", m.settings.artists_list.label()),
            row(
                "Now Playing Wheel",
                "now_playing_wheel",
                m.settings.now_playing_wheel.label(),
            ),
            row("Wrap Around", "wrap_around", m.settings.wrap_around.label()),
            row(
                "When Screen Off",
                "screen_off_buttons",
                m.settings.screen_off_buttons.label(),
            ),
            row("Haptics", "haptics", m.settings.haptics.label()),
        ]),
        Screen::TimeZone => Page::rows(crate::time_zone::rows(ui, m)),
        Screen::DateTime => {
            let mut page = Page::rows(crate::date_time::rows(ui, m));
            page.status = Some(crate::date_time::hint(m).into());
            page
        }
        Screen::SettingsSystem => system(ui, m),
        Screen::Battery => battery(m),
        Screen::Sleep => sleep(m),
        Screen::Storage => storage(ui, m),
        Screen::Update => update(m),
        Screen::About => Page::default()
            .fact("Reborn", reborn_core::VERSION)
            .fact(
                "System",
                m.platform
                    .snapshot
                    .info
                    .release_version
                    .as_deref()
                    .map(|v| format!("Y2Linux {}", reborn_core::display_release(v)))
                    .unwrap_or_else(|| "Y2Linux".into()),
            )
            .fact("Device", "Innioasis Y2")
            .with_rows(vec![Item::new("Licenses", "value:Licenses\u{1f}Reborn is free software under the MIT license. The system includes open-source components under their own licenses; their texts are installed with the system software.")]),
        Screen::Maintenance => Page::rows(vec![
            row("Reset Reborn Settings", "confirm:reset_settings", "Music and playlists stay"),
            row("Forget Wi-Fi Networks", "confirm:forget_wifi", "Saved passwords are removed"),
            row("Remove Bluetooth Pairings", "confirm:forget_bluetooth", "Pair your devices again"),
            row("Rebuild Music Library", "confirm:rebuild_library", "Re-read every music file"),
            row("Clear Cache", "confirm:clear_cache", "Removes saved artwork copies"),
        ]),
        _ => Page::default(),
    }
}

pub fn output_name(ui: &Ui, m: &AppModel) -> String {
    match &m.output {
        reborn_core::AudioOutput::Wired if m.speaker_active => "Speaker".into(),
        reborn_core::AudioOutput::Wired => "Headphone Jack".into(),
        reborn_core::AudioOutput::Bluetooth(address) => ui
            .bluetooth
            .devices
            .iter()
            .find(|d| &d.address == address)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| output_label(&m.output)),
    }
}

fn settings(ui: &Ui, m: &AppModel) -> Page {
    let eq = if m.settings.eq_enabled || m.settings.eq_profile_on {
        eq_summary(m)
    } else {
        "Equalizer off".to_owned()
    };
    let storage = free_text(&m.platform.snapshot.storage.internal)
        .map(|t| t.split(" free").next().unwrap_or("").to_owned() + " free")
        .unwrap_or_default();
    let brightness = if m.platform.brightness_available {
        format!("Brightness {}%", m.settings.brightness)
    } else {
        String::new()
    };
    Page::rows(vec![
        row("Bluetooth", "bluetooth", ui.bluetooth_summary()).with_kind(RowKind::Chevron),
        row("Wi-Fi", "wifi", ui.wifi_summary()).with_kind(RowKind::Chevron),
        row("Sound", "audio", eq).with_kind(RowKind::Chevron),
        row("Display", "display", brightness).with_kind(RowKind::Chevron),
        // Beta 7.5: the look is a place of its own, not a Display detail.
        row(
            "Skin",
            "theme",
            ui.skin.as_deref().map_or("Reborn", |s| s.name.as_str()),
        )
        .with_kind(RowKind::Chevron),
        row(
            "Customize",
            "customize",
            format!("Wrap: {}", m.settings.wrap_around.label()),
        )
        .with_kind(RowKind::Chevron),
        row("Storage", "storage", storage).with_kind(RowKind::Chevron),
        row(
            "Last.fm",
            "lastfm",
            match &ui.lastfm.user {
                Some(user) => user.clone(),
                None if m.settings.scrobble_log => "Log file".to_owned(),
                None => "Off".to_owned(),
            },
        )
        .with_kind(RowKind::Chevron),
        row("About & Updates", "update", reborn_core::VERSION).with_kind(RowKind::Chevron),
    ])
}

/// Settings > Last.fm: sign-in (a QR code while it runs), the account's
/// switches, what waits to be sent, and the scrobbler log.
fn lastfm(ui: &Ui, m: &AppModel) -> Page {
    let l = &ui.lastfm;
    let log = row("Also Write .scrobbler.log", "scrobble_log", "")
        .with_kind(RowKind::Toggle(m.settings.scrobble_log));
    if l.approval.is_some() {
        return Page::hero(
            "Scan to Sign In",
            "Open the code with your phone's camera and allow Y2Reborn. This page continues by itself.",
        )
        .with_rows(vec![row("Cancel", "lastfm_cancel", "")]);
    }
    if !l.available {
        return Page::hero(
            "Last.fm Isn't Available",
            "This build of Reborn has no Last.fm key. The scrobbler log still works.",
        )
        .with_rows(vec![log]);
    }
    let Some(user) = &l.user else {
        // Plays kept from a session Last.fm ended go out after signing in.
        let connect = if l.busy {
            "Starting…".to_owned()
        } else if l.waiting > 0 {
            crate::components::count(l.waiting, "play", "plays") + " waiting"
        } else {
            "Scan a code".to_owned()
        };
        return Page::rows(vec![
            row("Connect with Your Phone", "lastfm_connect", connect).with_kind(RowKind::Chevron),
            row("Sign In with Password", "lastfm_password", "").with_kind(RowKind::Chevron),
            log,
        ]);
    };
    let waiting = match l.waiting {
        0 => "Nothing waiting".to_owned(),
        n => crate::components::count(n, "play", "plays") + " waiting",
    };
    Page::rows(vec![
        row("Account", "lastfm_account", user.clone()),
        row("Scrobble", "lastfm_scrobble", "")
            .with_kind(RowKind::Toggle(m.settings.lastfm_scrobble)),
        row("Show Now Playing", "lastfm_now_playing", "")
            .with_kind(RowKind::Toggle(m.settings.lastfm_now_playing)),
        row(
            "Send Now",
            "lastfm_send",
            if l.busy {
                "Sending…".to_owned()
            } else {
                waiting
            },
        ),
        log,
        row("Disconnect", "lastfm_disconnect", ""),
    ])
}

/// Spotify: sign-in (a QR code while it runs), then the library.
fn spotify(ui: &Ui) -> Page {
    let s = &ui.spotify;
    if s.approval.is_some() {
        return Page::hero(
            "Scan to Connect Spotify",
            format!(
                "Open the code with your phone's camera, or go to spotify.com/pair and type {}. This page continues by itself.",
                s.user_code
            ),
        )
        .with_rows(vec![row("Cancel", "spotify_cancel", "")]);
    }
    if !s.available {
        return Page::hero(
            "Spotify Isn't Installed",
            "This build of Y2Linux has no Spotify player (spotifyd).",
        );
    }
    let online = ui.radio.online;
    let wifi = || {
        row(
            "Connect to Wi-Fi",
            "spotify_wifi",
            "Spotify needs the internet",
        )
    };
    if !s.signed_in {
        let mut page = Page::hero(
            "Listen to Spotify",
            "Connect your Spotify Premium account with your phone: no password is typed on the player.",
        );
        let mut rows = vec![];
        if !online {
            rows.push(wifi());
        }
        rows.push(
            row(
                "Connect with Your Phone",
                "spotify_connect",
                if s.busy { "Starting…" } else { "Scan a code" },
            )
            .with_kind(RowKind::Chevron),
        );
        page.rows = rows;
        return page;
    }
    let mut rows = vec![];
    if let Some((title, artist)) = &s.song {
        rows.push(row(title, "spotify_now", artist.clone()));
    }
    if !online {
        rows.push(wifi());
    }
    rows.extend([
        row("Playlists", "spotify_open:playlists", "").with_kind(RowKind::Chevron),
        row("Liked Songs", "spotify_open:liked-songs", "").with_kind(RowKind::Chevron),
        row("Albums", "spotify_open:saved-albums", "").with_kind(RowKind::Chevron),
        row("Artists", "spotify_open:followed-artists", "").with_kind(RowKind::Chevron),
        row("Disconnect", "spotify_disconnect", ""),
    ]);
    let mut page = Page::rows(rows);
    page.status = s.problem.clone();
    page
}

/// One Spotify list: loading, a problem, its rows and "More" while Spotify
/// has another page. An artist is a menu of three lists.
fn spotify_list(ui: &Ui, m: &AppModel) -> Page {
    let key = crate::spotify_filter(&m.navigation.filter).0;
    let (kind, id) = crate::spotify_list_kind(key);
    if kind == "artist" {
        return Page::rows(vec![
            row(
                "Top Songs",
                &format!("spotify_artist:artist-top-tracks:{id}"),
                "",
            )
            .with_kind(RowKind::Chevron),
            row("Albums", &format!("spotify_artist:artist-albums:{id}"), "")
                .with_kind(RowKind::Chevron),
            row(
                "All Songs",
                &format!("spotify_artist:artist-all-tracks:{id}"),
                "",
            )
            .with_kind(RowKind::Chevron),
        ]);
    }
    let Some(list) = ui.spotify.lists.get(key) else {
        return Page::hero("Loading…", "Asking Spotify.");
    };
    if list.loading && list.rows.is_empty() {
        return Page::hero("Loading…", "Asking Spotify.");
    }
    if let Some(error) = &list.error {
        if list.rows.is_empty() {
            return Page::hero("Couldn't Load This List", error.clone()).with_rows(vec![row(
                "Try Again",
                "spotify_retry",
                "",
            )]);
        }
    }
    if list.rows.is_empty() {
        let body = match kind {
            "playlists" => "Playlists you make or follow in Spotify appear here.",
            "liked-songs" => "Songs you like in Spotify appear here.",
            "saved-albums" => "Albums you save in Spotify appear here.",
            "followed-artists" => "Artists you follow in Spotify appear here.",
            _ => "Spotify has nothing to show here.",
        };
        return Page::hero("Nothing Here Yet", body);
    }
    let opens = matches!(
        kind,
        "playlists" | "saved-albums" | "artist-albums" | "followed-artists"
    );
    let mut rows = vec![];
    // A playlist or an album plays from its start; any song plays from it on.
    if matches!(kind, "playlist-tracks" | "album-tracks" | "liked-songs") {
        rows.push(row("Play All", "spotify_play_all", ""));
    }
    rows.extend(list.rows.iter().enumerate().map(|(i, r)| {
        let item = row(&r.title, &format!("spotify_row:{i}"), r.detail.clone());
        if opens {
            item.with_kind(RowKind::Chevron)
        } else {
            item
        }
    }));
    if list.next.is_some() {
        rows.push(row(
            if list.loading { "Loading…" } else { "More" },
            "spotify_more",
            "",
        ));
    }
    Page::rows(rows)
}

/// Names of the equalizer presets and their gains (dB) per band
/// (60 Hz, 250 Hz, 1 kHz, 4 kHz, 12 kHz).
pub const EQ_PRESETS: [(&str, &str, [i8; 5]); 4] = [
    ("Flat", "flat", [0, 0, 0, 0, 0]),
    ("Bass Boost", "bass", [6, 4, 0, 0, 0]),
    ("Vocal", "vocal", [-2, 0, 4, 3, 0]),
    ("Treble Boost", "treble", [0, 0, 0, 4, 6]),
];

pub(crate) fn eq_gains(m: &AppModel) -> [i8; 5] {
    let mut out = [0i8; 5];
    for (slot, band) in out.iter_mut().zip(&m.settings.eq_bands) {
        *slot = band.gain_db.round().clamp(-12., 12.) as i8;
    }
    out
}

/// The preset the equalizer currently matches, `Custom` otherwise.
pub(crate) fn eq_preset_name(m: &AppModel) -> &'static str {
    if !m.settings.eq_enabled {
        return "Flat";
    }
    let gains = eq_gains(m);
    EQ_PRESETS
        .iter()
        .find(|(_, _, preset)| *preset == gains)
        .map_or("Custom", |(name, _, _)| name)
}

/// What the Equalizer row says: the headphone profile when it plays, else
/// the preset.
pub(crate) fn eq_summary(m: &AppModel) -> String {
    match &m.settings.eq_profile {
        Some(p) if m.settings.eq_profile_on => p.name.clone(),
        _ => eq_preset_name(m).into(),
    }
}

fn sound(ui: &Ui, m: &AppModel) -> Page {
    let normalise = m.settings.replay_gain != reborn_core::ReplayGainMode::Off;
    Page::rows(vec![
        row("Equalizer", "equalizer", eq_summary(m)).with_kind(RowKind::Chevron),
        row("Normalize Volume", "replay_gain", "").with_kind(RowKind::Toggle(normalise)),
        row("Gapless Playback", "gapless", "")
            .with_kind(RowKind::Toggle(m.settings.gapless_enabled)),
        row(
            "Crossfade",
            "crossfade",
            crate::crossfade_label(m.settings.crossfade_ms),
        )
        .with_kind(RowKind::Chevron),
        row("Balance", "balance", balance_label(m.settings.balance)).with_kind(RowKind::Chevron),
        row("Output", "output_page", output_name(ui, m)).with_kind(RowKind::Chevron),
    ])
}

/// The Output card's second line for the headphone jack.
pub(crate) fn jack_summary(m: &AppModel) -> String {
    format!(
        "Level {} · Filter {}",
        m.settings.hp_level.label(),
        match m.settings.dac_filter {
            reborn_core::DacFilter::Fast => "Fast",
            reborn_core::DacFilter::FastPhaseCompensated => "Fast PC",
            reborn_core::DacFilter::Slow => "Slow",
            reborn_core::DacFilter::SlowPhaseCompensated => "Slow PC",
            reborn_core::DacFilter::Nos => "NOS",
        }
    )
}

/// Where music plays (the current one checked), then the headphone jack's
/// own settings, which only matter for wired playback.
fn output(ui: &Ui, m: &AppModel) -> Page {
    let mut rows = vec![row("Headphone Jack", "output_set:wired", "")
        .with_kind(RowKind::Check(m.output == reborn_core::AudioOutput::Wired))];
    // #28: the jack output plays on the speaker while nothing is plugged in.
    rows.push(row("Speaker", "speaker", "").with_kind(RowKind::Toggle(m.settings.speaker)));
    rows.extend(ui.bluetooth.devices.iter().filter(|d| d.usable_audio()).map(|d| {
        let current = matches!(&m.output, reborn_core::AudioOutput::Bluetooth(a) if a.eq_ignore_ascii_case(&d.address));
        row(&d.name, &format!("output_set:{}", d.path), "Bluetooth")
            .with_kind(RowKind::Check(current))
    }));
    rows.push(
        row("Headphone Level", "hp_level", m.settings.hp_level.label()).with_kind(RowKind::Chevron),
    );
    rows.push(
        row("DAC Filter", "dac_filter", m.settings.dac_filter.label()).with_kind(RowKind::Chevron),
    );
    Page::rows(rows)
}

fn headphone_level(m: &AppModel) -> Page {
    let mut page = Page::rows(
        reborn_core::HpLevel::ALL
            .into_iter()
            .map(|level| {
                row(level.label(), &format!("hp_level_set:{}", level.key()), "")
                    .with_kind(RowKind::Check(m.settings.hp_level == level))
            })
            .collect(),
    );
    page.status = Some(
        "Low suits earphones and IEMs, Medium most headphones, High hard-to-drive \
         headphones. The volume changes with it, so the music stays as loud."
            .into(),
    );
    page
}

fn dac_filter(m: &AppModel) -> Page {
    let mut page = Page::rows(
        reborn_core::DacFilter::ALL
            .into_iter()
            .map(|filter| {
                row(
                    filter.label(),
                    &format!("dac_filter_set:{}", filter.key()),
                    "",
                )
                .with_kind(RowKind::Check(m.settings.dac_filter == filter))
            })
            .collect(),
    );
    page.status =
        Some("How the headphone DAC filters between samples. Fast is the usual choice.".into());
    page
}

fn crossfade(m: &AppModel) -> Page {
    Page::rows(
        [0u32, 2, 4, 6, 8, 12]
            .into_iter()
            .map(|s| {
                let label = if s == 0 {
                    "Off".to_owned()
                } else {
                    format!("{s} seconds")
                };
                row(&label, &format!("crossfade_set:{}", s * 1000), "")
                    .with_kind(RowKind::Check(m.settings.crossfade_ms == s * 1000))
            })
            .collect(),
    )
}

/// "Center", "Left +2 dB": the louder side and by how much.
pub(crate) fn balance_label(balance: i8) -> String {
    match balance.signum() {
        -1 => format!("Left +{} dB", balance.unsigned_abs()),
        1 => format!("Right +{balance} dB"),
        _ => "Center".into(),
    }
}

/// Left +10 dB at the top, Center in the middle, Right +10 dB at the bottom:
/// the louder side is raised by lowering the other (a hearing difference
/// between the ears; user request).
fn balance(m: &AppModel) -> Page {
    let max = reborn_core::MAX_BALANCE;
    Page::rows(
        (-max..=max)
            .map(|b| {
                row(&balance_label(b), &format!("balance_set:{b}"), "")
                    .with_kind(RowKind::Check(m.settings.balance == b))
            })
            .collect(),
    )
}

fn screen_timeout(m: &AppModel) -> Page {
    let labels = [
        (15u32, "15 seconds"),
        (30, "30 seconds"),
        (60, "1 minute"),
        (120, "2 minutes"),
        (0, "Never"),
    ];
    let mut page = Page::rows(
        labels
            .into_iter()
            .map(|(s, label)| {
                row(label, &format!("timeout_set:{s}"), "")
                    .with_kind(RowKind::Check(m.settings.screen_timeout_seconds == s))
            })
            .collect(),
    );
    page.status = Some(
        if m.settings.screen_timeout_seconds == 0 {
            "Never keeps the screen on while music plays and drains the battery faster."
        } else {
            "The screen turns off after this long without a button press."
        }
        .into(),
    );
    page
}

/// The skin picker: Reborn's own look first, then every installed skin.
fn theme(ui: &Ui, m: &AppModel) -> Page {
    let mut rows = vec![
        row("Reborn", "theme_set:", "").with_kind(RowKind::Check(m.settings.theme.is_empty()))
    ];
    for skin in &ui.themes {
        // A skin's swatch says more than its author; only a problem is told.
        let detail = skin.problem.clone().unwrap_or_default();
        rows.push(disabled(
            row(&skin.name, &format!("theme_set:{}", skin.id), detail)
                .with_kind(RowKind::Check(m.settings.theme == skin.id)),
            skin.problem.is_none(),
        ));
    }
    let mut page = Page::rows(rows);
    page.status = Some(if ui.themes.is_empty() {
        "Copy a skin folder into Skins on Internal Storage or the SD card.".into()
    } else {
        "Add skins by copying their folder into Skins on Internal Storage or the SD card.".into()
    });
    page
}

/// Wi-Fi and Bluetooth before the owner's one-time firmware copy (Y2Linux 49+):
/// what to do, while it installs, and when the file was not the right one.
fn radio_firmware(m: &AppModel, radio: &str) -> Option<Page> {
    use reborn_core::platform::RadioFirmware;
    let s = &m.platform.snapshot;
    let how = if s.firmware_in_internal {
        "Connect the player to a computer and copy system.img from the Innioasis Y2 firmware package into the Firmware folder on Internal Storage."
    } else {
        "Connect the player to a computer and copy system.img from the Innioasis Y2 firmware package to the Firmware drive."
    };
    Some(match s.radio_firmware {
        RadioFirmware::Missing if s.firmware_no_space => Page::hero(
            "Not Enough Space for That File",
            "Use system.img from the Innioasis zip (about 520 MB), or copy it into a folder named Firmware on the SD card.",
        ),
        RadioFirmware::Missing => Page::hero(format!("{radio} Needs the Radio Firmware"), how),
        RadioFirmware::Installing => Page::hero(
            "Installing the Radio Firmware…",
            "This takes a few seconds. Wi-Fi and Bluetooth start by themselves.",
        ),
        RadioFirmware::Failed => Page::hero(
            "That File Isn't the Y2 Firmware",
            "Use system.img from the Innioasis Y2 firmware package (v3.2.0). Delete the copied file and copy the right one.",
        ),
        RadioFirmware::Ready | RadioFirmware::Unknown => return None,
    })
}

/// Internet radio: the station on air, then the owner's lists, then the
/// community directory. Without Wi-Fi a first row leads to Wi-Fi; favorites
/// and own lists stay usable.
fn radio_menu(ui: &Ui) -> Page {
    let r = &ui.radio;
    let favorites = match r.favorites.len() {
        0 => "No stations yet".to_owned(),
        n => crate::components::count(n, "station", "stations"),
    };
    let mut rows = vec![];
    if let Some(now) = &r.now {
        rows.push(row(&now.name, "radio_now", "On Air"));
    }
    if !r.online {
        rows.push(row(
            "Connect to Wi-Fi",
            "radio_wifi",
            "Radio needs the internet",
        ));
    }
    rows.extend([
        row("Favorites", "radio_favorites", favorites),
        row("Popular", "radio_popular", "Most listened worldwide"),
        row(
            "Countries",
            "radio_countries",
            "Stations from every country",
        ),
        row("Genres", "radio_genres", "Jazz, rock, news and more"),
        row("Search", "radio_search", "Find a station by name"),
        row("My Stations", "radio_own", "Your .m3u and .pls lists"),
    ]);
    Page::rows(rows)
}

/// One station's details; its actions are the rows.
fn radio_info(ui: &Ui) -> Page {
    let Some(station) = &ui.radio.info else {
        return Page::hero("No Station", "Choose a station first.").with_rows(radio_back());
    };
    let playing = ui
        .radio
        .now
        .as_ref()
        .is_some_and(|now| now.key() == station.key());
    // Skins show the station as a page: its name, the directory's facts and
    // the two actions (Reborn's own Station Info draws these itself).
    let mut state = vec![];
    if playing {
        state.push("On Air");
    }
    if ui.radio.is_favorite(station) {
        state.push("Favorite");
    }
    let facts = crate::views_radio::station_facts(station)
        .into_iter()
        .map(|(label, value)| (label.to_owned(), value))
        .collect();
    Page {
        hero: Some(Hero {
            title: station.name.clone(),
            body: state.join(" · "),
        }),
        facts,
        ..Page::rows(vec![
            row(
                if playing { "Now Playing" } else { "Play" },
                if playing {
                    "radio_now"
                } else {
                    "radio_info_play"
                },
                "",
            ),
            row(
                if ui.radio.is_favorite(station) {
                    "Remove from Favorites"
                } else {
                    "Add to Favorites"
                },
                "radio_info_favorite",
                "",
            ),
        ])
    }
}

fn radio_back() -> Vec<Item> {
    vec![row("Back to Radio", "radio_back", "")]
}

fn radio_stations(ui: &Ui) -> Page {
    let r = &ui.radio;
    if r.loading {
        return Page::hero("Loading Stations…", "Asking the station directory.")
            .with_rows(radio_back());
    }
    if let Some(error) = &r.error {
        return Page::hero("Couldn't Load Stations", error).with_rows(vec![row(
            "Try Again",
            "radio_retry",
            "",
        )]);
    }
    if r.stations.is_empty() {
        return match r.last_request {
            Some(reborn_core::RadioRequest::Favorites) => Page::hero(
                "No Favorites Yet",
                "Hold Select on a station, or on Now Playing, and choose Add to Favorites.",
            ),
            Some(reborn_core::RadioRequest::Own) => Page::hero(
                "No Stations of Your Own",
                "Copy .m3u or .pls station lists into a folder named Radio on Internal Storage or the SD card.",
            ),
            _ => Page::hero("No Stations Found", "Try another name, country or genre."),
        }
        .with_rows(radio_back());
    }
    Page::rows(
        r.stations
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut detail = crate::views_radio::station_detail(s);
                if r.is_favorite(s) {
                    detail = if detail.is_empty() {
                        "Favorite".into()
                    } else {
                        format!("Favorite · {detail}")
                    };
                }
                row(&s.name, &format!("station:{i}"), detail)
            })
            .collect(),
    )
}

fn radio_categories(ui: &Ui) -> Page {
    let r = &ui.radio;
    if r.loading {
        return Page::hero("Loading…", "Asking the station directory.").with_rows(radio_back());
    }
    if r.categories.is_empty() && r.error.is_none() {
        return Page::hero("Nothing Here", "The directory sent an empty list.")
            .with_rows(radio_back());
    }
    if let Some(error) = &r.error {
        return Page::hero("Couldn't Load the List", error).with_rows(vec![row(
            "Try Again",
            "radio_retry",
            "",
        )]);
    }
    Page::rows(
        r.categories
            .iter()
            .enumerate()
            .map(|(i, c)| {
                row(
                    &c.name,
                    &format!("category:{i}"),
                    format!("{} stations", c.stations),
                )
            })
            .collect(),
    )
}

fn wifi(ui: &Ui) -> Page {
    let w = &ui.wifi;
    if !w.available {
        return Page::hero(
            "Wi-Fi Is Starting…",
            "Wi-Fi becomes available a few seconds after the player starts.",
        );
    }
    if !w.powered {
        return Page::hero(
            "Wi-Fi Is Off",
            "Turn it on to sync the clock and get updates.",
        )
        .with_rows(vec![
            row("Wi-Fi", "wifi_power", "").with_kind(RowKind::Toggle(false))
        ]);
    }
    let mut rows = vec![row("Wi-Fi", "wifi_power", "").with_kind(RowKind::Toggle(true))];
    let connected = w.status.network();
    let checking = w
        .join
        .as_ref()
        .filter(|j| j.outcome == crate::WifiJoinOutcome::Checking)
        .map(|j| j.ssid.as_str());
    for n in &w.networks {
        let state = if checking == Some(n.ssid.as_str()) {
            // Tried before it is saved (Y2Reborn #20).
            if n.saved_id.is_some() || n.secured != Some(true) {
                "Connecting…".into()
            } else {
                "Checking password…".into()
            }
        } else if w.problem == Some(WifiProblem::WrongPassword)
            && w.problem_network.as_deref() == Some(n.ssid.as_str())
        {
            "Wrong password".into()
        } else if connected == Some(n.ssid.as_str()) && w.problem.is_some() {
            "Couldn't Connect".into()
        } else if connected == Some(n.ssid.as_str()) {
            match &w.status {
                WifiStatus::Connecting(_) => "Connecting…".into(),
                WifiStatus::GettingAddress(_) => "Getting network address…".into(),
                WifiStatus::NoInternet(_) => "Connected · No internet".into(),
                _ => "Connected".into(),
            }
        } else {
            network_detail(n)
        };
        let key = match n.saved_id {
            Some(id) => format!("saved:{id}"),
            None => format!("ssid:{}", n.ssid),
        };
        rows.push(disabled(row(&n.ssid, &key, state), n.secured.is_some()));
    }
    let scanning = w.scan.active();
    rows.push(disabled(
        row(
            if scanning {
                "Searching…"
            } else {
                "Search Again"
            },
            "wifi_scan",
            "",
        ),
        !scanning,
    ));
    let mut page = Page::rows(rows);
    page.status = wifi_status_line(ui);
    page
}

/// Saved, Secured or Open; the bars beside it show the signal (UI v4).
fn network_detail(n: &NetworkView) -> String {
    match (n.saved_id.is_some(), n.secured) {
        (_, None) => "Unsupported security".into(),
        (true, _) if !n.visible => "Saved · Not in range".into(),
        (true, _) => "Saved".into(),
        (false, Some(true)) => "Secured".into(),
        (false, Some(false)) => "Open".into(),
    }
}

pub fn wifi_status_line(ui: &Ui) -> Option<String> {
    let w = &ui.wifi;
    if let Some(problem) = w.problem {
        let name = w
            .status
            .network()
            .or(w.problem_network.as_deref())
            .map(|n| format!(" to {n}"))
            .unwrap_or_default();
        return Some(match problem {
            WifiProblem::WrongPassword => {
                format!("Couldn't connect{name}. Check the password and try again.")
            }
            WifiProblem::NoAddress => {
                "Connected to Wi-Fi, but couldn't get a network address.".into()
            }
            WifiProblem::NoInternetNames => {
                "Network connected, but internet name lookup isn't working.".into()
            }
            WifiProblem::NetworkNotFound => "Network not found. Move closer and try again.".into(),
            WifiProblem::Other => format!("Couldn't connect{name}. Try again."),
        });
    }
    match w.scan {
        reborn_core::RadioScan::Starting | reborn_core::RadioScan::Scanning => {
            Some("Searching for networks…".into())
        }
        reborn_core::RadioScan::Complete { found: 0 } => {
            Some("No networks found. Move closer to your router and search again.".into())
        }
        reborn_core::RadioScan::Failed { .. } => Some("Couldn't search. Try again.".into()),
        _ => None,
    }
}

fn bluetooth(ui: &Ui, m: &AppModel) -> Page {
    let b = &ui.bluetooth;
    if !b.available {
        return Page::hero(
            "Bluetooth Is Starting…",
            "Bluetooth becomes available a few seconds after the player starts.",
        );
    }
    if !b.powered {
        return Page::hero(
            "Bluetooth Is Off",
            "Turn it on to listen with wireless headphones.",
        )
        .with_rows(vec![
            row("Bluetooth", "bt_power", "").with_kind(RowKind::Toggle(false))
        ]);
    }
    let mut rows = vec![row("Bluetooth", "bt_power", "").with_kind(RowKind::Toggle(true))];
    let mut devices: Vec<&BluetoothDeviceView> = b.devices.iter().collect();
    devices.sort_by_key(|d| (!d.connected, !d.saved_pairing()));
    for d in devices {
        rows.push(row(
            &d.name,
            &format!("bt_device:{}", d.path),
            d.description(),
        ));
    }
    let scanning = b.scan.active();
    rows.push(disabled(
        row(
            if scanning {
                "Searching…"
            } else {
                "Search for Devices"
            },
            "bt_scan",
            "",
        ),
        !scanning,
    ));
    let quality = m.platform.snapshot.bluetooth_quality;
    let mut page = Page::rows(rows);
    page.status = if quality.pending_restart {
        Some("Restart the player to apply the saved codec quality.".into())
    } else if let Some(problem) = &b.problem {
        Some(problem.clone())
    } else {
        match b.scan {
            reborn_core::RadioScan::Starting | reborn_core::RadioScan::Scanning => {
                Some("Searching… Put your headphones in pairing mode.".into())
            }
            reborn_core::RadioScan::Complete { found: 0 } => {
                Some("No devices found. Put your headphones in pairing mode.".into())
            }
            _ if b.devices.is_empty() => Some(
                "No Bluetooth audio device connected. Pair a device to listen wirelessly.".into(),
            ),
            _ => None,
        }
    };
    page
}

fn pc_transfer(m: &AppModel) -> Page {
    let free = free_text(&m.platform.snapshot.storage.internal);
    let page = if m.library.scanning && m.platform.snapshot.usb != UsbTransfer::Copying {
        Page::hero(
            "Refreshing Music…",
            "Your music will appear when the scan finishes.",
        )
    } else {
        match m.platform.snapshot.usb {
            UsbTransfer::Ready => Page::hero(
                "USB File Transfer",
                "Open Y2 on your computer to copy music.",
            ),
            UsbTransfer::Copying => Page::hero(
                "Copying Files…",
                "Keep the cable connected until your computer finishes.",
            ),
            UsbTransfer::Developer => Page::hero(
                "Advanced USB Transfer",
                "Use your configured file-transfer client to copy music.",
            ),
            UsbTransfer::Starting => {
                Page::hero("Preparing PC Transfer…", "Keep the USB cable connected.")
            }
            UsbTransfer::Error => Page::hero(
                "File Transfer Interrupted",
                "Check storage space. Reconnect the USB cable and retry the copy.",
            ),
            UsbTransfer::Disconnected => Page::hero(
                "Connect to a Computer",
                "Connect Y2 to a computer with a USB cable to transfer music.",
            ),
            UsbTransfer::Unknown => Page::hero(
                "Checking Connection…",
                "Connect Y2 to a computer with a USB cable to transfer music.",
            ),
        }
    };
    match free {
        Some(free) => page.fact("Internal Storage", free),
        None => page,
    }
    .with_rows(vec![Item::new("Scan for Music", "scan_library")])
}

fn library(m: &AppModel, tracks: &[Track]) -> Page {
    let count = |internal: bool| {
        tracks
            .iter()
            .filter(|t| t.online)
            .filter(|t| {
                m.sources
                    .iter()
                    .find(|s| s.id == t.source_id)
                    .is_some_and(|s| matches!(s.kind, MediaSource::Internal) == internal)
            })
            .count()
    };
    let songs = |n: usize| crate::components::count(n, "song", "songs");
    let sd_online = m
        .sources
        .iter()
        .any(|s| matches!(s.kind, MediaSource::SdCard(_)) && s.online);
    let sd = if sd_online {
        songs(count(false))
    } else {
        match m.platform.snapshot.storage.sd {
            SdCard::Error => "Card can't be read".into(),
            SdCard::NeedsRepair => "Card needs repair".into(),
            SdCard::CheckTimedOut => "Card check took too long".into(),
            _ => "No SD card".into(),
        }
    };
    let scan = if m.library.scanning {
        "Scanning… Your music stays available".into()
    } else if m.library.error.is_some() {
        "Last scan was interrupted. Existing music was kept".into()
    } else if let Some(scan) = &m.library.last_scan {
        match scan.unreadable {
            0 => format!("Last scan: {}", songs(scan.discovered as usize)),
            n => format!(
                "Last scan: {} · {n} can't be read",
                songs(scan.discovered.saturating_sub(n) as usize)
            ),
        }
    } else {
        "Find music added from a computer".into()
    };
    Page::rows(vec![
        row("Internal Storage", "storage", songs(count(true))),
        row("SD Card", "storage", sd),
        disabled(
            row("Scan for Music", "scan_library", scan),
            !m.library.scanning,
        ),
        disabled(
            row(
                "Rebuild Library",
                "confirm:rebuild_library",
                "Re-read every file from scratch",
            ),
            !m.library.scanning,
        ),
    ])
}

/// The hidden service menu: everything an owner or developer needs that a
/// listener does not. Reached by tapping the version in About & Updates.
fn system(ui: &Ui, m: &AppModel) -> Page {
    let s = &m.platform.snapshot;
    let b = &ui.bluetooth;
    let quality = m.platform.snapshot.bluetooth_quality;
    let mut rows: Vec<Item> = vec![];
    if !b.codec_choices.is_empty() {
        rows.push(row(
            "Codec Preference",
            "codec_preference",
            m.settings.codec_preference.label(),
        ));
    }
    if quality.sbc_supported && b.sbc_quality_available {
        if let Some(q) = quality.requested_sbc {
            rows.push(disabled(
                row("SBC Quality", "sbc_quality", q.label()),
                m.platform.busy.is_none(),
            ));
        }
    }
    if b.codec_choices.contains(&reborn_core::CodecPreference::Aac) {
        if quality.aac_supported {
            if let Some(q) = quality.requested_aac {
                rows.push(disabled(
                    row("AAC Quality", "aac_quality", q.label()),
                    m.platform.busy.is_none(),
                ));
            }
        }
        if quality.aac_abr_supported {
            if let Some(abr) = quality.requested_aac_abr {
                rows.push(disabled(
                    row("AAC Auto Rate", "aac_abr", on_off(abr)),
                    m.platform.busy.is_none(),
                ));
            }
        }
    }
    if b.codec_choices
        .contains(&reborn_core::CodecPreference::Ldac)
    {
        if quality.ldac_supported {
            if let Some(q) = quality.requested_quality {
                rows.push(disabled(
                    row("LDAC Quality", "ldac_quality", q.label()),
                    m.platform.busy.is_none(),
                ));
            }
        }
        if quality.abr_supported {
            if let Some(abr) = quality.requested_abr {
                rows.push(disabled(
                    row("LDAC Auto Rate", "ldac_abr", on_off(abr)),
                    m.platform.busy.is_none(),
                ));
            }
        }
    }
    rows.extend(vec![
        row("Battery", "battery", battery_summary(m)),
        row("Sleep", "sleep", "Pause and rest the player"),
        row(
            "Storage",
            "storage",
            free_text(&s.storage.internal).unwrap_or_default(),
        ),
        row("Software Update", "update", update_summary(m)),
        row("About", "about", reborn_core::VERSION),
        row("Reset & Maintenance", "maintenance", ""),
        row(
            "Diagnostics",
            "diagnostics",
            match s.health {
                HealthLevel::Degraded => "Needs attention",
                HealthLevel::Failed => "Problem detected",
                _ => "",
            },
        ),
        Item::new("Restart", "confirm:reboot"),
        Item::new("Power Off", "confirm:power_off"),
    ]);
    Page::rows(rows)
}

pub fn battery_summary(m: &AppModel) -> String {
    let b = m.platform.battery;
    let state = match b.charging {
        ChargingState::Charging => "Charging",
        ChargingState::Full => "Fully charged",
        ChargingState::OnBattery | ChargingState::Unknown => "",
    };
    match (b.percent, state) {
        (Some(p), "") => format!("{p}%"),
        (Some(p), s) => format!("{p}% · {s}"),
        (None, s) => s.into(),
    }
}

fn battery(m: &AppModel) -> Page {
    let b = m.platform.battery;
    let page = match b.level {
        LowBattery::Low => Page::hero("Low Battery", "Connect a charger soon."),
        LowBattery::Critical => Page::hero(
            "Battery Critically Low",
            "Connect a charger now. The player turns off to protect the battery.",
        ),
        LowBattery::ShuttingDown => Page::hero("Shutting Down", "The battery is empty."),
        LowBattery::Normal => Page::default(),
    };
    let page = match b.percent {
        Some(p) => page.fact(
            if b.estimated {
                "Estimated Charge"
            } else {
                "Charge"
            },
            format!("{p}%"),
        ),
        None => page,
    };
    page.fact(
        "Status",
        match b.charging {
            ChargingState::Charging => "Charging",
            ChargingState::Full => "Fully charged",
            ChargingState::OnBattery => "On battery",
            ChargingState::Unknown => "Checking…",
        },
    )
}

pub(crate) fn eq_frequency(hz: f32) -> String {
    if hz >= 1_000. {
        format!("{} kHz", hz / 1_000.)
    } else {
        format!("{hz:.0} Hz")
    }
}

/// The headphone profiles on the storage, the one playing checked; a saved
/// profile whose file is gone stays listed.
fn eq_profiles(ui: &Ui, m: &AppModel) -> Page {
    let playing = m
        .settings
        .eq_profile
        .as_ref()
        .filter(|_| m.settings.eq_profile_on)
        .map(|p| p.name.as_str());
    let mut rows: Vec<Item> = ui
        .eq_profiles
        .iter()
        .enumerate()
        .map(|(index, file)| {
            row(&file.name, &format!("eq_profile_file:{index}"), "")
                .with_kind(RowKind::Check(playing == Some(file.name.as_str())))
        })
        .collect();
    if let Some(name) = playing.filter(|n| !ui.eq_profiles.iter().any(|f| f.name == *n)) {
        rows.insert(
            0,
            row(name, "eq_profile_saved", "Saved").with_kind(RowKind::Check(true)),
        );
    }
    let mut page = Page::rows(rows);
    page.status = Some(
        "More: AutoEQ ParametricEQ.txt files in an EQ folder on Internal Storage or the SD card."
            .into(),
    );
    page
}

fn equalizer(ui: &Ui, m: &AppModel) -> Page {
    if m.navigation.filter == "profile" {
        return eq_profiles(ui, m);
    }
    if m.navigation.filter == "custom" {
        let defaults = reborn_core::flat_eq_bands();
        let bands = if m.settings.eq_bands.is_empty() {
            &defaults
        } else {
            &m.settings.eq_bands
        };
        let mut rows = vec![];
        rows.extend(bands.iter().take(8).enumerate().map(|(index, band)| {
            row(
                &eq_frequency(band.frequency_hz),
                &format!("eq_band:{index}"),
                format!("{:+.0} dB", band.gain_db),
            )
        }));
        rows.push(Item::new("Reset to Flat", "eq_reset"));
        return Page::rows(rows);
    }
    let profile = m.settings.eq_profile_on && m.settings.eq_profile.is_some();
    let current = if profile { "" } else { eq_preset_name(m) };
    let mut rows: Vec<Item> = EQ_PRESETS
        .iter()
        .map(|(name, key, _)| {
            row(name, &format!("eq_preset:{key}"), "").with_kind(RowKind::Check(current == *name))
        })
        .collect();
    rows.push(
        row("Custom", "eq_custom", "").with_kind(if current == "Custom" {
            RowKind::Check(true)
        } else {
            RowKind::Chevron
        }),
    );
    rows.push(
        row("Headphone Profile", "eq_profile", "").with_kind(if profile {
            RowKind::Check(true)
        } else {
            RowKind::Chevron
        }),
    );
    Page::rows(rows)
}

fn sleep(m: &AppModel) -> Page {
    let s = m.platform.snapshot.sleep;
    let (title, body) = match s.phase {
        SleepPhase::SleepRequested | SleepPhase::SleepPreparing => ("Preparing to Sleep…", "The player is checking that it can sleep safely."),
        SleepPhase::Sleeping => ("Sleeping", "Press Power to wake the player."),
        SleepPhase::Waking => ("Waking…", "Restoring your player."),
        SleepPhase::Restored => ("Ready", match s.wake {
            WakeReason::Power => "The Power button woke your player.",
            WakeReason::Rtc => "The wake timer woke your player.",
            WakeReason::Unknown => "Your player is awake again.",
        }),
        SleepPhase::RestoreFailed => ("Wake Needs Attention", "Save your place and restart the player. Diagnostics has the last sleep result."),
        SleepPhase::SleepRefused => ("Player Stayed Awake", match s.problem {
            Some(SleepProblem::ChargerActive) => "Sleep needs the charger disconnected, so Power turned off the screen instead.",
            Some(SleepProblem::PlaybackActive) => "Pause music before putting the player to sleep. Power can turn off the screen while music plays.",
            Some(SleepProblem::QualificationPending) => "Sleep is not available yet, so Power turned off the screen instead. You can also power off.",
            Some(SleepProblem::Busy) => "The player is finishing another task. Try again when it finishes.",
            _ => "The player could not sleep safely. Try again or power off.",
        }),
        SleepPhase::Awake => ("Sleep", "Power sleeps the player when music is paused. During playback, it turns off the screen."),
    };
    let busy = m.platform.busy.is_some()
        || matches!(
            s.phase,
            SleepPhase::SleepRequested
                | SleepPhase::SleepPreparing
                | SleepPhase::Sleeping
                | SleepPhase::Waking
        );
    Page::hero(title, body).with_rows(vec![
        disabled(Item::new("Sleep Now", "task:sleep"), !busy),
        Item::new("Turn Off Screen", "screen_off"),
        Item::new("Power Off", "confirm:power_off"),
    ])
}

pub fn free_text(v: &VolumeSpace) -> Option<String> {
    match (v.free_bytes, v.total_bytes) {
        (Some(free), Some(total)) if total > 0 => Some(format!(
            "{} free of {}",
            human_bytes(free),
            human_bytes(total)
        )),
        (Some(free), _) => Some(format!("{} free", human_bytes(free))),
        _ => None,
    }
}

/// What the computer connection is doing; empty without a cable.
fn usb_summary(m: &AppModel) -> &'static str {
    match m.platform.snapshot.usb {
        UsbTransfer::Ready => "Ready",
        UsbTransfer::Copying => "Copying Files…",
        UsbTransfer::Developer => "Advanced Transfer",
        UsbTransfer::Starting => "Preparing…",
        UsbTransfer::Error => "Reconnect cable",
        UsbTransfer::Disconnected | UsbTransfer::Unknown => "",
    }
}

fn storage(ui: &Ui, m: &AppModel) -> Page {
    let s = &m.platform.snapshot;
    if !s.observed {
        return Page::hero("Checking Storage…", "This takes a moment.");
    }
    let internal = &s.storage.internal;
    let mut page = match internal.state {
        VolumeState::AlmostFull => Page::hero(
            "Storage Almost Full",
            "Remove some music using a computer to keep the library up to date.",
        ),
        VolumeState::LowSpace => Page::hero(
            "Storage Running Low",
            "Remove some music using a computer when convenient.",
        ),
        VolumeState::ReadOnly | VolumeState::Error => Page::hero(
            "Storage Problem",
            "Internal storage can't be written. Restart the player.",
        ),
        _ => Page::default(),
    };
    // How much music there is (Beta 7.5: no longer a row of About & Updates
    // that led here).
    let songs = ui.library_counts(m, &m.library.tracks)[2];
    page = page.with_rows(vec![disabled(
        row(
            "Scan for Music",
            "scan_library",
            if m.library.scanning {
                "Scanning…".into()
            } else {
                crate::components::count(songs, "song", "songs")
            },
        ),
        !m.library.scanning,
    )]);
    // The computer page opens by itself when a cable is connected; Storage
    // leads back to it only while one is (UI v4: no idle "From a Computer").
    if !usb_summary(m).is_empty() {
        page.rows.push(
            row("From a Computer", "pc_transfer", usb_summary(m)).with_kind(RowKind::Chevron),
        );
    }
    // Y2Linux sets Internal Storage up by itself at start; a real library on
    // the old storage waits for the listener (it is erased).
    use reborn_core::platform::InternalSetup;
    if let InternalSetup::Waiting { files, bytes } = s.internal_setup {
        page.rows.push(
            row(
                "Set Up Internal Storage",
                "confirm:internal_setup",
                if files == 0 {
                    // The platform could not read the old storage (nothing to copy off).
                    "Old storage can't be read".to_owned()
                } else {
                    format!(
                        "{} old {} · {}",
                        files,
                        if files == 1 { "file" } else { "files" },
                        reborn_core::platform::human_bytes(bytes)
                    )
                },
            )
            .with_kind(RowKind::Chevron),
        );
    }
    // Y2Linux 8.27: a card whose check found errors plays read-only; the
    // owner may repair it here (it can shorten or remove damaged files).
    if s.storage.sd.repairable() {
        page.rows.push(
            row(
                "Repair SD Card",
                "confirm:sd_repair",
                match s.storage.sd {
                    SdCard::NeedsRepair => "Not playing · Has errors",
                    SdCard::Unchecked(_) => "Not checked yet · Playing only",
                    _ => "Has errors · Playing only",
                },
            )
            .with_kind(RowKind::Chevron),
        );
    }
    page = page.fact(
        "Internal Storage",
        match s.internal_setup {
            InternalSetup::Pending(wait) => crate::views_settings::pending_note(wait).into(),
            InternalSetup::Running => "Setting up…".into(),
            InternalSetup::Waiting { .. } => "Not set up · Old files kept".into(),
            _ => free_text(internal).unwrap_or_else(|| "Checking…".into()),
        },
    );
    page = page.fact(
        "SD Card",
        match &s.storage.sd {
            SdCard::Ready(space) => free_text(space).unwrap_or_else(|| "Ready".into()),
            SdCard::Absent => "No SD card".into(),
            SdCard::Error => "SD card error · Reinsert the card".into(),
            SdCard::NeedsRepair => "Needs repair · Check it on a computer".into(),
            SdCard::CheckTimedOut => "Check took too long · Insert it again".into(),
            SdCard::ReadOnly(_) => "Has errors · Playing only".into(),
            SdCard::Unchecked(_) => "Not checked · Playing only".into(),
            SdCard::Unknown => "Checking…".into(),
        },
    );
    page
}

pub fn update_summary(m: &AppModel) -> String {
    let s = &m.platform.snapshot;
    if m.platform.busy == Some(PlatformTask::UpdateStage) {
        return match m.platform.download {
            Some(l) if l.verifying => format!("Checking · {}%", (l.fraction() * 100.) as u32),
            Some(l) => format!("Downloading · {}%", (l.fraction() * 100.) as u32),
            None => "Downloading…".into(),
        };
    }
    if m.platform.busy == Some(PlatformTask::UpdateCheck) {
        return "Working…".into();
    }
    match &s.update.phase {
        UpdatePhase::UpToDate => "Up to date".into(),
        UpdatePhase::Available { .. } => "Update available".into(),
        UpdatePhase::ReadyToInstall => "Ready to install".into(),
        UpdatePhase::Installing => "Finishing…".into(),
        UpdatePhase::Restoring => "Restore pending".into(),
        UpdatePhase::Downloading => match m.platform.snapshot.update.download_percent {
            Some(p) => format!("Downloading · {p}%"),
            None => "Downloading…".into(),
        },
        _ => String::new(),
    }
}

/// "1.0.0-beta.6.3" reads as "1.0 Beta 6.3", "1.0.0" as "1.0", and a
/// development build "1.0.0-platform-v1-power-final-candidate.35" as "1.0.0 (35)".
pub fn short_version(version: &str) -> String {
    let release = |base: &str| {
        base.strip_suffix(".0")
            .filter(|b| b.contains('.'))
            .unwrap_or(base)
            .to_owned()
    };
    if let Some((base, beta)) = version.split_once("-beta.") {
        let numbered = beta
            .split('.')
            .all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if numbered {
            return format!("{} Beta {beta}", release(base));
        }
    }
    if !version.contains('-') && version.split('.').count() == 3 {
        return release(version);
    }
    match (version.split('-').next(), version.rsplit('.').next()) {
        (Some(base), Some(build))
            if version.contains('-') && build.chars().all(|c| c.is_ascii_digit()) =>
        {
            format!("{base} ({build})")
        }
        _ => version.to_owned(),
    }
}

/// About & Updates: the update status first, then the facts an owner may want.
fn update(m: &AppModel) -> Page {
    let mut page = update_status(m);
    if let Some(reports) = &m.platform.snapshot.reports {
        page.rows.push(
            row(
                "Error Reports",
                "error_reports",
                match reports.enabled {
                    _ if !reports.upload => "",
                    Some(true) => "On",
                    Some(false) => "Off",
                    None => "Not Set",
                },
            )
            .with_kind(RowKind::Chevron),
        );
    }
    page.rows.push(row(
        "Version",
        "version",
        short_version(reborn_core::VERSION),
    ));
    page.rows
        .push(row("Reset Settings", "confirm:reset_settings", "").with_kind(RowKind::Chevron));
    page
}

/// While a report is made: no rows (nothing to press); the view draws the
/// live progress (`views_settings::report_progress`), this hero is its text.
pub(crate) fn report_preparing(m: &AppModel) -> Option<Page> {
    matches!(
        m.platform.busy,
        Some(PlatformTask::ReportSend | PlatformTask::ReportSave(..))
    )
    .then(|| {
        Page::hero(
            "Preparing the Report…",
            "The player collects its logs and removes personal details. This takes about ten seconds.",
        )
    })
}

/// About & Updates > Error Reports (Y2Linux ERROR-REPORTS-PLAN): the switch,
/// what is sent, a report now, the install ID and the last report. Until the
/// platform uploads, only Save Report: a file on storage the owner sends.
fn error_reports(m: &AppModel) -> Page {
    let upload = m
        .platform
        .snapshot
        .reports
        .as_ref()
        .is_some_and(|r| r.upload);
    if m.navigation.filter == "info" {
        return if upload {
            Page::hero(
                "What Is Sent",
                "The player's logs, settings and hardware details. Never your music, names, \
                 Wi-Fi, Bluetooth or accounts. Sent encrypted over Wi-Fi, kept 180 days.",
            )
        } else {
            Page::hero(
                "What Is Saved",
                "The player's logs, settings and hardware details. Never your music, names, \
                 Wi-Fi, Bluetooth or accounts. Attach the file to an issue on GitHub.",
            )
        };
    }
    if let Some(page) = report_preparing(m) {
        return page;
    }
    let Some(r) = &m.platform.snapshot.reports else {
        let mut page = Page::rows(vec![
            row("What Is Sent", "report_info", "").with_kind(RowKind::Chevron)
        ]);
        page.status = Some("Checking…".into());
        return page;
    };
    let idle = m.platform.busy.is_none();
    if m.navigation.filter == "save" {
        if r.destinations.is_empty() {
            return Page::hero(
                "No Storage Available",
                "Internal Storage and the SD card can't be written right now.",
            );
        }
        return Page::rows(
            r.destinations
                .iter()
                .map(|d| {
                    disabled(
                        row(d.label(), &format!("report_dest:{}", d.argument()), ""),
                        idle,
                    )
                })
                .collect(),
        );
    }
    let save = disabled(
        row("Save Report", "report_save", "").with_kind(RowKind::Chevron),
        idle,
    );
    let saved = r.last.as_ref().filter(|l| l.state == "saved").map(|l| {
        format!(
            "Last report saved to {}, Error Reports folder.",
            l.destination.map_or("storage", |d| d.label())
        )
    });
    if !upload {
        let mut page = Page::rows(vec![
            save,
            row("What Is Saved", "report_info", "").with_kind(RowKind::Chevron),
        ]);
        page.status = Some(saved.unwrap_or_else(|| {
            "Saves the logs to Internal Storage or the SD card for you to send.".into()
        }));
        return page;
    }
    let on = r.enabled == Some(true);
    let mut rows = vec![
        disabled(
            row("Send Error Reports", "report_toggle", "").with_kind(RowKind::Toggle(on)),
            idle,
        ),
        row("What Is Sent", "report_info", "").with_kind(RowKind::Chevron),
        disabled(row("Send Report Now", "report_send", ""), on && idle),
        save,
    ];
    if let Some(id) = r.short_id().filter(|_| on) {
        rows.push(disabled(
            row("Install ID", "confirm:report_new_id", id),
            idle,
        ));
    }
    let mut page = Page::rows(rows);
    page.status = Some(match (&r.last, on) {
        _ if saved.is_some() => saved.unwrap_or_default(),
        (_, false) => "Off: nothing is collected or sent.".into(),
        (None, true) => "On: a report is made when something goes wrong.".into(),
        (Some(last), true) => {
            let what = report_trigger_label(&last.trigger);
            match last.state.as_str() {
                "sent" => format!("Last report ({what}) sent."),
                // Refused by the receiver, or still unsent after three days.
                "dropped" => format!("Last report ({what}) couldn't be sent."),
                "failed" => format!("Last report ({what}) couldn't be sent yet."),
                _ if r.queued > 1 => {
                    format!("Last report ({what}) waiting to send, {} in all.", r.queued)
                }
                _ => format!("Last report ({what}) waiting to send."),
            }
        }
    });
    page
}

/// What an automatic or manual report was about, in plain words.
fn report_trigger_label(trigger: &str) -> &'static str {
    match trigger {
        "manual" => "sent by you",
        "kernel_crash" => "system crash",
        "kernel_warning" => "system warning",
        "unclean_restart" => "unexpected restart",
        "reborn_crash" => "player app crash",
        "reborn_failure" => "player app problem",
        "update_failure" => "update problem",
        "sleep_failure" => "sleep problem",
        "radio_failure" => "Wi-Fi or Bluetooth problem",
        "display_failure" => "display problem",
        "storage_error" => "storage problem",
        _ => "problem",
    }
}

fn update_status(m: &AppModel) -> Page {
    let s = &m.platform.snapshot;
    let u = &s.update;
    let busy = m.platform.busy;
    if busy == Some(PlatformTask::UpdateCheck) {
        return Page::hero("Checking for Updates…", "This takes a moment.");
    }
    if busy == Some(PlatformTask::UpdateStage) || u.phase == UpdatePhase::Downloading {
        return match m.platform.download {
            Some(l) if l.verifying => Page::hero(
                format!("Checking the Update… {}%", (l.fraction() * 100.) as u32),
                "Making sure the download is complete and genuine.",
            ),
            Some(l) => Page::hero(
                format!("Downloading Update… {}%", (l.fraction() * 100.) as u32),
                "Keep Wi-Fi on.",
            ),
            None => Page::hero(
                match u.download_percent {
                    Some(p) => format!("Downloading Update… {p}%"),
                    None => "Downloading Update…".into(),
                },
                "Keep Wi-Fi on.",
            ),
        };
    }
    if !s.observed {
        return Page::hero("Software Update", "Checking the installed version…");
    }
    if !s.enabled("ota") {
        return Page::hero(
            "Updates Are Installed with a Computer",
            "Connect the player to a computer to install new software.",
        );
    }
    let check = disabled(
        Item::new("Check for Updates", "task:update_check"),
        u.can_check && busy.is_none(),
    );
    match &u.phase {
        UpdatePhase::UpToDate => Page::hero(
            "Up to Date",
            match &s.info.release_version {
                Some(v) => format!("{} is the latest version.", short_version(v)),
                None => format!("Reborn {} is the latest version.", reborn_core::VERSION),
            },
        )
        .with_rows(vec![check]),
        // One step (Beta 7.1): Update Now downloads, checks and restarts to
        // install without a second confirmation.
        UpdatePhase::Available { version } => Page::hero(
            "Update Available",
            match u.download_bytes {
                Some(bytes) => format!(
                    "Version {} ({} MB) downloads and installs in one go; the player restarts at the end.",
                    short_version(version),
                    bytes.div_ceil(1_000_000)
                ),
                None => format!(
                    "Version {} downloads and installs in one go; the player restarts at the end.",
                    short_version(version)
                ),
            },
        )
        .with_rows(vec![disabled(
            Item::new("Update Now", "update_now"),
            u.can_download && busy.is_none(),
        )]),
        UpdatePhase::ReadyToInstall => Page::hero(
            "Ready to Install",
            "The player restarts to install. It takes about a minute.",
        )
        .with_rows(vec![
            disabled(
                Item::new("Install Now", "update_install"),
                u.can_install && busy.is_none(),
            ),
            disabled(
                Item::new("Cancel Update", "confirm:update_cancel"),
                u.can_cancel && busy.is_none(),
            ),
        ]),
        UpdatePhase::Installing => Page::hero(
            "Finishing the Update…",
            "The new version is checking itself. This takes a minute.",
        ),
        UpdatePhase::Restoring => Page::hero(
            "Restoring the Previous Version",
            "The update didn't start properly. Turn the player off, then press Power.",
        ),
        UpdatePhase::Checking | UpdatePhase::Downloading => {
            Page::hero("Working…", "This takes a moment.")
        }
        UpdatePhase::Failed(problem) => match problem {
            // Also after a wake while Wi-Fi was still reconnecting: it may be on.
            UpdateProblem::NeedsNetwork => Page::hero(
                "Couldn't Reach the Update Server",
                "Make sure Wi-Fi is connected, then check again.",
            )
            .with_rows(vec![Item::new("Wi-Fi", "wifi"), check]),
            UpdateProblem::NoUpdateSource | UpdateProblem::NeedsComputer => Page::hero(
                "Updates Are Installed with a Computer",
                "Connect the player to a computer to install new software.",
            ),
            UpdateProblem::NotEnoughSpace => Page::hero(
                "Not Enough Space for the Update",
                match u.space_needed_mb {
                    Some(mb) => format!(
                        "The player cleared what it could, but the update needs {mb} MB of system space. Your music is not affected. Connect the player to a computer to update."
                    ),
                    None => "The player cleared what it could. Your music is not affected. Connect the player to a computer to update.".into(),
                },
            )
            .with_rows(vec![check]),
            UpdateProblem::NeedsCharge => Page::hero(
                "Connect the Charger",
                "Installing needs the charger or at least half a battery.",
            )
            .with_rows(vec![check]),
            UpdateProblem::VerificationFailed => Page::hero(
                "Update Couldn't Be Verified",
                "Nothing was changed on your player.",
            )
            .with_rows(vec![check]),
            UpdateProblem::Other => Page::hero(
                "Update Didn't Finish",
                "Nothing was changed on your player.",
            )
            .with_rows(vec![check]),
        },
        UpdatePhase::Unknown => Page::hero(
            "Software Update",
            "See whether a newer version is available.",
        )
        .with_rows(vec![check]),
    }
}

fn track_info(m: &AppModel, tracks: &[Track]) -> Page {
    let track = m
        .navigation
        .filter
        .parse::<usize>()
        .ok()
        .and_then(|i| tracks.get(i))
        .or_else(|| m.current());
    let Some(t) = track else {
        return Page::hero(
            "Song Not Found",
            "It may be on an SD card that was removed.",
        );
    };
    let format = match (t.codec.is_empty(), t.sample_rate) {
        (true, _) => String::new(),
        (false, 0) => t.codec.to_uppercase(),
        (false, rate) => format!("{} · {}", t.codec.to_uppercase(), khz(rate)),
    };
    let location = m
        .sources
        .iter()
        .find(|s| s.id == t.source_id)
        .map(|s| match s.kind {
            MediaSource::Internal => "Internal Storage",
            MediaSource::SdCard(_) => "SD Card",
        })
        .unwrap_or("");
    let mut page = Page::default()
        .fact("Title", crate::track_title(t))
        .fact("Artist", crate::names_or_unknown(&t.artist))
        .fact("Album", crate::display_or_unknown(&t.album))
        .fact("Length", crate::components::time(t.duration_ms));
    if !format.is_empty() {
        page = page.fact("Format", format);
    }
    page = page.fact("File", t.filename.clone());
    if !location.is_empty() {
        page = page.fact("Location", location);
    }
    page
}

pub fn khz(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{:.1} kHz", rate as f32 / 1000.)
    }
}
