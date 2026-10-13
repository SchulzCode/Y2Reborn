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
    AppModel, MediaSource, PlatformTask, Screen, Track,
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
        Screen::Home => Page::rows(vec![
            row("Now Playing", "now_playing", ""),
            row("Artists", "artists", ""),
            row("Albums", "albums", ""),
            row("Genres", "genres", ""),
            row("Songs", "tracks", ""),
            row("Folders", "folders", ""),
            row("Radio", "radio", ""),
            row("Settings", "settings", ""),
        ]),
        Screen::Music => Page::rows(vec![
            Item::new("Albums", "albums"),
            Item::new("Artists", "artists"),
            Item::new("Genres", "genres"),
            Item::new("Songs", "tracks"),
            Item::new("Playlists", "playlists"),
            Item::new("Folders", "folders"),
        ]),
        Screen::NowPlaying if m.current().is_none() => Page::hero(
            "Nothing Playing",
            "Choose an album, artist or song to start listening.",
        )
        .with_rows(vec![Item::new("Browse music", "home")]),
        Screen::NowPlaying => Page::default(),
        Screen::TrackInfo => track_info(m, tracks),
        Screen::Settings => settings(ui, m),
        Screen::Wifi => radio_firmware(m, "Wi-Fi").unwrap_or_else(|| wifi(ui)),
        Screen::Radio => radio_menu(ui),
        Screen::RadioStations => radio_stations(ui),
        Screen::RadioCategories => radio_categories(ui),
        Screen::Bluetooth => radio_firmware(m, "Bluetooth").unwrap_or_else(|| bluetooth(ui, m)),
        Screen::PcTransfer => pc_transfer(m),
        Screen::SettingsAudio => sound(ui, m),
        Screen::Equalizer => equalizer(m),
        Screen::Crossfade => crossfade(m),
        Screen::ScreenTimeout => screen_timeout(m),
        Screen::Theme => theme(ui, m),
        Screen::SettingsPlayback => Page::rows(vec![
            row("Shuffle", "shuffle", on_off(m.settings.shuffle)),
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
                    "Screen off after",
                    "screen_timeout",
                    crate::timeout_label(m.settings.screen_timeout_seconds),
                )
                .with_kind(RowKind::Chevron),
            );
            rows.push(row("Album view", "album_view", m.settings.album_view.label()));
            rows.push(
                row(
                    "Theme",
                    "theme",
                    ui.skin.as_deref().map_or("Reborn", |s| s.name.as_str()),
                )
                .with_kind(RowKind::Chevron),
            );
            Page::rows(rows)
        }
        Screen::SettingsSystem => system(ui, m),
        Screen::Battery => battery(m),
        Screen::Sleep => sleep(m),
        Screen::Storage => storage(m),
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
        reborn_core::AudioOutput::Wired => "Headphone jack".into(),
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
    let eq = if m.settings.eq_enabled {
        eq_preset_name(m)
    } else {
        "Equalizer off"
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
        row("Storage", "storage", storage).with_kind(RowKind::Chevron),
        row("About & Updates", "update", reborn_core::VERSION).with_kind(RowKind::Chevron),
    ])
}

/// Names of the equalizer presets and their gains (dB) per band
/// (60 Hz, 250 Hz, 1 kHz, 4 kHz, 12 kHz).
pub const EQ_PRESETS: [(&str, &str, [i8; 5]); 4] = [
    ("Flat", "flat", [0, 0, 0, 0, 0]),
    ("Bass boost", "bass", [6, 4, 0, 0, 0]),
    ("Vocal", "vocal", [-2, 0, 4, 3, 0]),
    ("Treble boost", "treble", [0, 0, 0, 4, 6]),
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

fn sound(ui: &Ui, m: &AppModel) -> Page {
    let normalise = m.settings.replay_gain != reborn_core::ReplayGainMode::Off;
    Page::rows(vec![
        row("Equalizer", "equalizer", eq_preset_name(m)).with_kind(RowKind::Chevron),
        row("Normalise volume", "replay_gain", "").with_kind(RowKind::Toggle(normalise)),
        row("Gapless playback", "gapless", "")
            .with_kind(RowKind::Toggle(m.settings.gapless_enabled)),
        row(
            "Crossfade",
            "crossfade",
            crate::crossfade_label(m.settings.crossfade_ms),
        )
        .with_kind(RowKind::Chevron),
        row("Output", "output", output_name(ui, m)).with_kind(RowKind::Chevron),
    ])
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
    let mut rows = vec![row("Reborn", "theme_set:", "The built-in look")
        .with_kind(RowKind::Check(m.settings.theme.is_empty()))];
    for skin in &ui.themes {
        let detail = match (&skin.problem, skin.author.is_empty()) {
            (Some(problem), _) => problem.clone(),
            (None, false) => format!("by {}", skin.author),
            (None, true) => skin.description.clone(),
        };
        rows.push(disabled(
            row(&skin.name, &format!("theme_set:{}", skin.id), detail)
                .with_kind(RowKind::Check(m.settings.theme == skin.id)),
            skin.problem.is_none(),
        ));
    }
    let mut page = Page::rows(rows);
    page.status = Some(if ui.themes.is_empty() {
        "Copy a skin folder into Themes on Internal Storage or the SD card.".into()
    } else {
        "Add skins by copying their folder into Themes on Internal Storage or the SD card.".into()
    });
    page
}

fn usb_summary(m: &AppModel) -> &'static str {
    match m.platform.snapshot.usb {
        UsbTransfer::Ready => "Ready",
        UsbTransfer::Copying => "Copying files…",
        UsbTransfer::Developer => "Advanced transfer",
        UsbTransfer::Starting => "Preparing…",
        UsbTransfer::Error => "Reconnect cable",
        UsbTransfer::Disconnected | UsbTransfer::Unknown => "",
    }
}

/// Wi-Fi and Bluetooth before the owner's one-time firmware copy (Y2Linux 49+):
/// what to do, while it installs, and when the file was not the right one.
fn radio_firmware(m: &AppModel, radio: &str) -> Option<Page> {
    use reborn_core::platform::RadioFirmware;
    const HOW: &str = "Connect the player to a computer and copy system.img from the Innioasis Y2 firmware package to the Firmware drive.";
    Some(match m.platform.snapshot.radio_firmware {
        RadioFirmware::Missing => Page::hero(format!("{radio} Needs the Radio Firmware"), HOW),
        RadioFirmware::Installing => Page::hero(
            "Installing the Radio Firmware…",
            "This takes a few seconds. Wi-Fi and Bluetooth start by themselves.",
        ),
        RadioFirmware::Failed => Page::hero(
            "That File Isn't the Y2 Firmware",
            "Use system.img from the Innioasis Y2 firmware package (v3.2.0). Delete the file on the Firmware drive and copy the right one.",
        ),
        RadioFirmware::Ready | RadioFirmware::Unknown => return None,
    })
}

/// Internet radio: the owner's lists first, then the community directory.
fn radio_menu(ui: &Ui) -> Page {
    let r = &ui.radio;
    let favorites = match r.favorites.len() {
        0 => String::new(),
        1 => "1 station".into(),
        n => format!("{n} stations"),
    };
    let rows = vec![
        row("Favorites", "radio_favorites", favorites),
        row("Popular", "radio_popular", ""),
        row("Countries", "radio_countries", ""),
        row("Genres", "radio_genres", ""),
        row("Search", "radio_search", ""),
        row("My Stations", "radio_own", "Radio folder"),
    ];
    if !r.online {
        return Page::hero(
            "Radio Needs Wi-Fi",
            "Connect to Wi-Fi to listen to internet radio. Favorites and your own lists stay here.",
        )
        .with_rows(
            std::iter::once(row("Wi-Fi", "radio_wifi", ""))
                .chain(rows)
                .collect(),
        );
    }
    Page::rows(rows)
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
                let mut detail = s.summary();
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
            "Wi-Fi is off",
            "Turn it on to sync the clock and get updates.",
        )
        .with_rows(vec![
            row("Wi-Fi", "wifi_power", "").with_kind(RowKind::Toggle(false))
        ]);
    }
    let mut rows = vec![row("Wi-Fi", "wifi_power", "").with_kind(RowKind::Toggle(true))];
    let connected = w.status.network();
    for n in &w.networks {
        let state = if connected == Some(n.ssid.as_str()) && w.problem.is_some() {
            "Couldn't connect".into()
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

fn network_detail(n: &NetworkView) -> String {
    let signal = match n.bars {
        0 | 1 => "Weak signal",
        2 => "Good signal",
        _ => "Strong signal",
    };
    match (n.saved_id.is_some(), n.secured) {
        (_, None) => "Unsupported security".into(),
        (true, _) if !n.visible => "Saved · Not in range".into(),
        (true, _) => format!("Saved · {signal}"),
        (false, Some(true)) => format!("Secured · {signal}"),
        (false, Some(false)) => format!("Open · {signal}"),
    }
}

pub fn wifi_status_line(ui: &Ui) -> Option<String> {
    let w = &ui.wifi;
    if let Some(problem) = w.problem {
        let name = w
            .status
            .network()
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
            "Bluetooth is off",
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
            d.description(&m.output),
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
    .with_rows(vec![Item::new("Scan for New Music", "scan_library")])
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
    let songs = |n: usize| match n {
        1 => "1 song".to_owned(),
        n => format!("{n} songs"),
    };
    let sd_online = m
        .sources
        .iter()
        .any(|s| matches!(s.kind, MediaSource::SdCard(_)) && s.online);
    let sd = if sd_online {
        songs(count(false))
    } else {
        match m.platform.snapshot.storage.sd {
            SdCard::Error => "Card can't be read".into(),
            _ => "No SD card".into(),
        }
    };
    let scan = if m.library.scanning {
        "Scanning… Your music stays available".into()
    } else if m.library.error.is_some() {
        "Last scan was interrupted. Existing music was kept".into()
    } else if let Some(scan) = &m.library.last_scan {
        format!("Last scan: {}", songs(scan.discovered as usize))
    } else {
        "Find music added from a computer".into()
    };
    Page::rows(vec![
        row("Internal Storage", "storage", songs(count(true))),
        row("SD Card", "storage", sd),
        disabled(
            row("Scan for New Music", "scan_library", scan),
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

fn equalizer(m: &AppModel) -> Page {
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
    let current = eq_preset_name(m);
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

fn storage(m: &AppModel) -> Page {
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
    page = page.with_rows(vec![
        disabled(
            row(
                "Scan for music",
                "scan_library",
                if m.library.scanning {
                    "Scanning…"
                } else {
                    ""
                },
            ),
            !m.library.scanning,
        ),
        row("From a computer", "pc_transfer", usb_summary(m)).with_kind(RowKind::Chevron),
    ]);
    page = page.fact(
        "Internal Storage",
        free_text(internal).unwrap_or_else(|| "Checking…".into()),
    );
    page = page.fact(
        "SD Card",
        match &s.storage.sd {
            SdCard::Ready(space) => free_text(space).unwrap_or_else(|| "Ready".into()),
            SdCard::Absent => "No SD card".into(),
            SdCard::Error => "SD card error · Reinsert the card".into(),
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

/// "1.0.0-platform-v1-power-final-candidate.35" reads as "1.0.0 (35)".
pub fn short_version(version: &str) -> String {
    // "1.0.0-beta.1" reads as "1.0.0 Beta 1".
    if let Some((base, beta)) = version.split_once("-beta.") {
        if !beta.is_empty() && beta.chars().all(|c| c.is_ascii_digit()) {
            return format!("{base} Beta {beta}");
        }
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
    page.rows
        .push(row("Version", "version", reborn_core::VERSION));
    page.rows
        .push(row("Reset settings", "confirm:reset_settings", "").with_kind(RowKind::Chevron));
    page
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
        UpdatePhase::Available { version } => Page::hero(
            "Update Available",
            match u.download_bytes {
                Some(bytes) => format!(
                    "Version {} is ready to download ({} MB).",
                    short_version(version),
                    bytes.div_ceil(1_000_000)
                ),
                None => format!("Version {} is ready to download.", short_version(version)),
            },
        )
        .with_rows(vec![disabled(
            Item::new("Download Update", "task:update_stage"),
            u.can_download && busy.is_none(),
        )]),
        UpdatePhase::ReadyToInstall => Page::hero(
            "Ready to Install",
            "The player restarts to install. It takes about a minute.",
        )
        .with_rows(vec![
            disabled(
                Item::new("Restart and Install", "confirm:update_apply"),
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
            UpdateProblem::NeedsNetwork => Page::hero(
                "Connect to Wi-Fi to Check for Updates",
                "Updates download over Wi-Fi.",
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
        .fact("Artist", crate::display_or_unknown(&t.artist))
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
