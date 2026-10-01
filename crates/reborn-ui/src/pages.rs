//! Normal product pages, built from typed state only.
//!
//! A page is an optional hero message (empty, off, error or progress state),
//! a few static facts, and the interactive rows the wheel moves through.
//! Engineering detail is never placed here; it belongs to `diagnostics`.
use crate::{components::output_label, BluetoothDeviceView, Item, NetworkView, Ui, WifiStatus};
use reborn_core::{
    platform::{
        human_bytes, ChargingState, HealthLevel, LowBattery, SdCard, UpdatePhase, UpdateProblem,
        UsbTransfer, VolumeSpace, VolumeState, WifiProblem,
    },
    AppModel, MediaSource, PlatformTask, RepeatMode, Screen, Track,
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
            row("Music", "music", "Albums · Artists · Songs"),
            row(
                "Now Playing",
                "now_playing",
                m.current()
                    .map(|t| crate::track_title(t).to_owned())
                    .unwrap_or_else(|| "Nothing playing".into()),
            ),
            row(
                "Queue",
                "queue",
                match m.queue.len() {
                    0 => "Empty".into(),
                    1 => "1 song".into(),
                    n => format!("{n} songs"),
                },
            ),
            row("Settings", "settings", ""),
        ]),
        Screen::Music => Page::rows(vec![
            Item::new("Albums", "albums"),
            Item::new("Artists", "artists"),
            Item::new("Songs", "tracks"),
            Item::new("Folders", "folders"),
        ]),
        Screen::NowPlaying if m.current().is_none() => Page::hero(
            "Nothing Playing",
            "Choose an album, artist or song to start listening.",
        )
        .with_rows(vec![Item::new("Open Music", "music")]),
        Screen::NowPlaying => Page::default(),
        Screen::TrackInfo => track_info(m, tracks),
        Screen::Settings => settings(ui, m),
        Screen::Wifi => wifi(ui),
        Screen::Bluetooth => bluetooth(ui, m),
        Screen::PcTransfer => pc_transfer(m),
        Screen::SettingsAudio => Page::rows(vec![
            row("Output", "output", output_name(ui, m)),
            row(
                "ReplayGain",
                "replay_gain",
                crate::replay_gain_label(m.settings.replay_gain),
            ),
        ]),
        Screen::SettingsPlayback => Page::rows(vec![
            row("Shuffle", "shuffle", on_off(m.settings.shuffle)),
            row("Repeat", "repeat", crate::repeat_label(m.settings.repeat)),
            row(
                "Gapless Playback",
                "gapless",
                on_off(m.settings.gapless_enabled),
            ),
            row(
                "Crossfade",
                "crossfade",
                crate::crossfade_label(m.settings.crossfade_ms),
            ),
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
            rows.push(row(
                "Screen Timeout",
                "timeout",
                crate::timeout_label(m.settings.screen_timeout_seconds),
            ));
            Page::rows(rows)
        }
        Screen::SettingsSystem => system(m),
        Screen::Battery => battery(m),
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
    Page::rows(vec![
        row("Wi-Fi", "wifi", ui.wifi_summary()),
        row("Bluetooth", "bluetooth", ui.bluetooth_summary()),
        row("PC Transfer", "pc_transfer", usb_summary(m)),
        row("Audio", "audio", output_name(ui, m)),
        row("Playback", "playback", playback_summary(m)),
        row("Library", "library", ""),
        row("Display", "display", ""),
        row("System", "system", ""),
    ])
}

fn playback_summary(m: &AppModel) -> String {
    let mut parts = vec![];
    if m.settings.shuffle {
        parts.push("Shuffle");
    }
    match m.settings.repeat {
        RepeatMode::Off => {}
        RepeatMode::Track => parts.push("Repeat One"),
        RepeatMode::All => parts.push("Repeat All"),
    }
    if m.settings.crossfade_ms > 0 {
        parts.push("Crossfade");
    }
    parts.join(" · ")
}

fn usb_summary(m: &AppModel) -> &'static str {
    match m.platform.snapshot.usb {
        UsbTransfer::Ready => "Ready",
        UsbTransfer::Starting => "Preparing…",
        UsbTransfer::Error => "Reconnect cable",
        UsbTransfer::Disconnected | UsbTransfer::Unknown => "",
    }
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
        return Page::hero("Wi-Fi Is Off", "Turn on Wi-Fi to connect to a network.")
            .with_rows(vec![Item::new("Turn On Wi-Fi", "wifi_power")]);
    }
    let mut rows = vec![row("Wi-Fi", "wifi_power", "On")];
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
            "Bluetooth Is Off",
            "Turn on Bluetooth to listen with wireless headphones.",
        )
        .with_rows(vec![Item::new("Turn On Bluetooth", "bt_power")]);
    }
    let mut rows = vec![row("Bluetooth", "bt_power", "On")];
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
    let mut page = Page::rows(rows);
    page.status = if let Some(problem) = &b.problem {
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
    let page = match m.platform.snapshot.usb {
        UsbTransfer::Ready => Page::hero(
            "PC Transfer Ready",
            "Copy music to the Music folder from your computer.",
        ),
        UsbTransfer::Starting => {
            Page::hero("Preparing PC Transfer…", "Keep the USB cable connected.")
        }
        UsbTransfer::Error => Page::hero(
            "PC Transfer Disconnected",
            "Reconnect the USB cable and try again.",
        ),
        UsbTransfer::Disconnected => Page::hero(
            "Connect to a Computer",
            "Connect Y2 to a computer with a USB cable to transfer music.",
        ),
        UsbTransfer::Unknown => Page::hero(
            "Checking Connection…",
            "Connect Y2 to a computer with a USB cable to transfer music.",
        ),
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

fn system(m: &AppModel) -> Page {
    let s = &m.platform.snapshot;
    Page::rows(vec![
        row("Battery", "battery", battery_summary(m)),
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
    ])
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
        Some(p) => page.fact("Charge", format!("{p}%")),
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
    if matches!(
        m.platform.busy,
        Some(PlatformTask::UpdateCheck | PlatformTask::UpdateStage)
    ) {
        return "Working…".into();
    }
    match &s.update.phase {
        UpdatePhase::UpToDate => "Up to date".into(),
        UpdatePhase::Available { .. } => "Update available".into(),
        UpdatePhase::ReadyToInstall => "Ready to install".into(),
        UpdatePhase::Installing => "Installing…".into(),
        UpdatePhase::Downloading => "Downloading…".into(),
        _ => String::new(),
    }
}

fn update(m: &AppModel) -> Page {
    let s = &m.platform.snapshot;
    let u = &s.update;
    let busy = m.platform.busy;
    if busy == Some(PlatformTask::UpdateCheck) {
        return Page::hero("Checking for Updates…", "This takes a moment.");
    }
    if busy == Some(PlatformTask::UpdateStage) || u.phase == UpdatePhase::Downloading {
        return Page::hero(
            "Downloading Update…",
            "Keep Wi-Fi on and the player charging.",
        );
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
            format!("Reborn {} is the latest version.", reborn_core::VERSION),
        )
        .with_rows(vec![check]),
        UpdatePhase::Available { version } => Page::hero(
            "Update Available",
            format!("Version {version} is ready to download."),
        )
        .with_rows(vec![disabled(
            Item::new("Download Update", "task:update_stage"),
            u.can_download && busy.is_none(),
        )]),
        UpdatePhase::ReadyToInstall => Page::hero(
            "Ready to Install",
            "Restart to finish installing. Keep the player charging.",
        )
        .with_rows(vec![
            disabled(
                Item::new("Restart to Install", "confirm:update_apply"),
                u.can_install && busy.is_none(),
            ),
            disabled(
                Item::new("Cancel Update", "confirm:update_cancel"),
                u.can_cancel && busy.is_none(),
            ),
        ]),
        UpdatePhase::Installing => Page::hero(
            "Installing Update…",
            "The player restarts by itself. Keep it charging.",
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
                "Remove some music using a computer, then try again.",
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
        return Page::hero("Song Not Available", "It may be on a removed SD card.");
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
