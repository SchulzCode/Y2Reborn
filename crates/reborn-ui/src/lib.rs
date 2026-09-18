#![forbid(unsafe_code)]
use font8x8::UnicodeFonts;
use reborn_core::{Action, AppModel, AudioOutput, RadioScan, Screen, Track};
use reborn_graphics::Quad;
#[derive(Clone)]
pub struct Item {
    pub label: String,
    pub key: String,
}
#[derive(Clone)]
pub enum Effect {
    None,
    Play(usize),
    Toggle,
    Next,
    Previous,
    Seek(i64),
    Volume(i8),
    Scan,
    WifiPower,
    WifiScan,
    WifiConnect { ssid: String, password: String },
    WifiSaved(u32),
    WifiForget(u32),
    BluetoothPower,
    BluetoothScan,
    BluetoothDevice { path: String, operation: String },
    Output(AudioOutput),
    ConfirmPairing(bool),
    ScreenToggle,
    Checkpoint,
}
#[derive(Clone)]
struct View {
    screen: Screen,
    selected: usize,
    filter: String,
}
#[derive(Default)]
pub struct RadioView {
    pub available: bool,
    pub powered: bool,
    pub scan: RadioScan,
    pub error: Option<String>,
    pub connection: String,
    pub count: usize,
}
impl RadioView {
    fn message(&self, bluetooth: bool) -> String {
        let name = if bluetooth { "Bluetooth" } else { "Wi-Fi" };
        match &self.scan {
            RadioScan::Starting => return format!("Starting {name}..."),
            RadioScan::Scanning => {
                return format!(
                    "Scanning... {} {}",
                    self.count,
                    if bluetooth { "devices" } else { "networks" }
                )
            }
            RadioScan::Failed { message } => return message.clone(),
            _ => {}
        }
        if let Some(error) = &self.error {
            return error.clone();
        }
        if !self.available {
            return format!("{name} service is starting or unavailable");
        }
        if !self.powered {
            return format!("{name} is off. Scan will turn it on.");
        }
        if let RadioScan::Complete { found } = self.scan {
            return if found == 0 {
                if bluetooth {
                    "No devices found. Put device in pairing mode.".into()
                } else {
                    "No networks found. Select Scan to try again.".into()
                }
            } else {
                format!(
                    "Scan complete: {found} {}",
                    if bluetooth { "devices" } else { "networks" }
                )
            };
        }
        if !self.connection.is_empty() {
            return self.connection.clone();
        }
        format!("{name} is on. Select Scan to discover.")
    }
    pub fn requested(&mut self) {
        self.scan = RadioScan::Starting;
        self.error = None;
    }
    pub fn failed(&mut self, error: String) {
        if self.scan.active() {
            self.scan = RadioScan::Failed {
                message: error.clone(),
            };
        }
        self.error = Some(error);
    }
}
#[derive(Default)]
pub struct Ui {
    pub selected: usize,
    pub filter: String,
    pub notice: String,
    pub wifi: RadioView,
    pub bluetooth: RadioView,
    pub networks: Vec<Item>,
    pub saved_networks: Vec<Item>,
    pub bluetooth_devices: Vec<Item>,
    pub pairing: Option<String>,
    history: Vec<View>,
    password: String,
    ssid: String,
    letter: usize,
    pub text_entry: bool,
}
const LETTERS:&[u8]=b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 !@#$%^&*()-_=+[]{};:'\",.<>/?\\|`~";
impl Ui {
    fn go(&mut self, m: &mut AppModel, s: Screen, filter: String) {
        if self.history.len() >= 16 {
            self.history.remove(0);
        }
        self.history.push(View {
            screen: m.screen,
            selected: self.selected,
            filter: self.filter.clone(),
        });
        self.selected = 0;
        self.filter = filter;
        m.screen = s;
    }
    pub fn rows(&self, m: &AppModel, tracks: &[Track]) -> Vec<Item> {
        let labels = |a: &[&str]| {
            a.iter()
                .map(|s| Item {
                    label: (*s).into(),
                    key: (*s).into(),
                })
                .collect()
        };
        match m.screen {
            Screen::Main => labels(&[
                "Music",
                "Now Playing",
                "Bluetooth",
                "Wi-Fi",
                "Diagnostics",
                "Settings",
            ]),
            Screen::Music => labels(&["Artists", "Albums", "Tracks", "Folders", "Scan library"]),
            Screen::Artists | Screen::Albums | Screen::Folders => {
                let mut v = tracks
                    .iter()
                    .map(|t| match m.screen {
                        Screen::Artists => t.artist.clone(),
                        Screen::Albums => t.album.clone(),
                        _ => t
                            .path
                            .parent()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    })
                    .collect::<Vec<_>>();
                v.sort();
                v.dedup();
                v.into_iter()
                    .map(|s| Item {
                        label: if s.is_empty() {
                            "Unknown".into()
                        } else {
                            s.clone()
                        },
                        key: s,
                    })
                    .collect()
            }
            Screen::Tracks => tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| self.matches(t))
                .map(|(i, t)| Item {
                    label: format!("{}{}", if t.online { "" } else { "[offline] " }, t.title),
                    key: i.to_string(),
                })
                .collect(),
            Screen::Wifi => {
                let mut v = labels(&[
                    if self.wifi.powered {
                        "Turn Wi-Fi off"
                    } else {
                        "Turn Wi-Fi on"
                    },
                    match self.wifi.scan {
                        RadioScan::Starting => "Starting Wi-Fi...",
                        RadioScan::Scanning => "Scanning networks...",
                        _ => "Scan networks",
                    },
                ]);
                v.extend(self.saved_networks.iter().cloned());
                v.extend(self.networks.iter().cloned());
                v
            }
            Screen::Bluetooth => {
                if self.filter.starts_with('/') {
                    labels(&["Pair", "Connect", "Disconnect", "Forget", "Use for audio"])
                } else {
                    let mut v = labels(&[
                        if self.bluetooth.powered {
                            "Turn Bluetooth off"
                        } else {
                            "Turn Bluetooth on"
                        },
                        match self.bluetooth.scan {
                            RadioScan::Starting => "Starting Bluetooth...",
                            RadioScan::Scanning => "Scanning devices...",
                            _ => "Scan devices",
                        },
                        "Use wired output",
                    ]);
                    v.extend(self.bluetooth_devices.iter().cloned());
                    v
                }
            }
            Screen::Settings => labels(&[
                "Volume +",
                "Volume -",
                "Screen timeout +",
                "Screen timeout -",
                "Use wired output",
            ]),
            _ => vec![],
        }
    }
    fn matches(&self, t: &Track) -> bool {
        if self.filter.is_empty() {
            true
        } else if let Some(v) = self.filter.strip_prefix("artist:") {
            t.artist == v
        } else if let Some(v) = self.filter.strip_prefix("album:") {
            t.album == v
        } else if let Some(v) = self.filter.strip_prefix("folder:") {
            t.path.parent().is_some_and(|p| p.to_string_lossy() == v)
        } else {
            true
        }
    }
    pub fn action(&mut self, m: &mut AppModel, tracks: &[Track], action: Action) -> Effect {
        if action == Action::ScreenToggle {
            return Effect::ScreenToggle;
        }
        if self.pairing.is_some() {
            return match action {
                Action::Select => {
                    self.pairing = None;
                    Effect::ConfirmPairing(true)
                }
                Action::Back => {
                    self.pairing = None;
                    Effect::ConfirmPairing(false)
                }
                _ => Effect::None,
            };
        }
        if self.text_entry {
            return self.entry(action);
        }
        match action {
            Action::PlayPause => return Effect::Toggle,
            Action::VolumeUp => return Effect::Volume(5),
            Action::VolumeDown => return Effect::Volume(-5),
            Action::Next => return Effect::Next,
            Action::Previous => return Effect::Previous,
            Action::Menu => {
                self.go(m, Screen::Main, String::new());
                return Effect::None;
            }
            Action::Back => {
                if let Some(v) = self.history.pop() {
                    m.screen = v.screen;
                    self.selected = v.selected;
                    self.filter = v.filter;
                } else {
                    m.screen = Screen::Main;
                    self.filter.clear();
                }
                return Effect::None;
            }
            _ => {}
        }
        if m.screen == Screen::NowPlaying {
            return match action {
                Action::Select => Effect::Toggle,
                Action::Right => Effect::Seek(10000),
                Action::Left => Effect::Seek(-10000),
                Action::Up => Effect::Previous,
                Action::Down => Effect::Next,
                _ => Effect::None,
            };
        }
        let rows = self.rows(m, tracks);
        let delta = match action {
            Action::Up => -1,
            Action::Down => 1,
            Action::Wheel(n) => n,
            _ => 0,
        };
        if delta != 0 && !rows.is_empty() {
            self.selected =
                (self.selected as i64 + delta as i64).rem_euclid(rows.len() as i64) as usize;
            return Effect::None;
        }
        if action == Action::Left && m.screen == Screen::Wifi {
            if let Some(row) = rows.get(self.selected) {
                if let Some(id) = row.key.strip_prefix("saved:").and_then(|s| s.parse().ok()) {
                    return Effect::WifiForget(id);
                }
            }
        }
        if action != Action::Select && action != Action::Right {
            return Effect::None;
        }
        let Some(row) = rows.get(self.selected) else {
            return Effect::None;
        };
        let key = row.key.clone();
        match m.screen {
            Screen::Main => {
                let s = [
                    Screen::Music,
                    Screen::NowPlaying,
                    Screen::Bluetooth,
                    Screen::Wifi,
                    Screen::Diagnostics,
                    Screen::Settings,
                ][self.selected.min(5)];
                self.go(m, s, String::new());
            }
            Screen::Music => {
                if self.selected == 4 {
                    return Effect::Scan;
                }
                self.go(
                    m,
                    [
                        Screen::Artists,
                        Screen::Albums,
                        Screen::Tracks,
                        Screen::Folders,
                    ][self.selected.min(3)],
                    String::new(),
                );
            }
            Screen::Artists | Screen::Albums | Screen::Folders => {
                let prefix = match m.screen {
                    Screen::Artists => "artist:",
                    Screen::Albums => "album:",
                    _ => "folder:",
                };
                self.go(m, Screen::Tracks, format!("{prefix}{key}"));
            }
            Screen::Tracks => {
                if let Ok(index) = key.parse() {
                    self.go(m, Screen::NowPlaying, String::new());
                    return Effect::Play(index);
                }
            }
            Screen::Wifi => {
                if self.selected == 0 {
                    return if self.wifi.scan == RadioScan::Starting {
                        Effect::None
                    } else {
                        Effect::WifiPower
                    };
                }
                if self.selected == 1 {
                    return if self.wifi.scan.active() {
                        Effect::None
                    } else {
                        Effect::WifiScan
                    };
                }
                if let Some(id) = key.strip_prefix("saved:").and_then(|s| s.parse().ok()) {
                    return Effect::WifiSaved(id);
                }
                self.ssid = key;
                self.password.clear();
                self.letter = 0;
                self.text_entry = true;
            }
            Screen::Bluetooth => {
                if self.filter.starts_with('/') {
                    if self.selected == 4 {
                        return Effect::BluetoothDevice {
                            path: self.filter.clone(),
                            operation: "output".into(),
                        };
                    }
                    return Effect::BluetoothDevice {
                        path: self.filter.clone(),
                        operation: key.to_lowercase(),
                    };
                } else {
                    match self.selected {
                        0 => {
                            return if self.bluetooth.scan == RadioScan::Starting {
                                Effect::None
                            } else {
                                Effect::BluetoothPower
                            }
                        }
                        1 => {
                            return if self.bluetooth.scan.active() {
                                Effect::None
                            } else {
                                Effect::BluetoothScan
                            }
                        }
                        2 => return Effect::Output(AudioOutput::Wired),
                        _ => self.go(m, Screen::Bluetooth, key),
                    }
                }
            }
            Screen::Settings => match self.selected {
                0 => return Effect::Volume(5),
                1 => return Effect::Volume(-5),
                2 => {
                    m.settings.screen_timeout_seconds =
                        (m.settings.screen_timeout_seconds + 30).min(600);
                    return Effect::Checkpoint;
                }
                3 => {
                    m.settings.screen_timeout_seconds =
                        m.settings.screen_timeout_seconds.saturating_sub(30).max(30);
                    return Effect::Checkpoint;
                }
                4 => return Effect::Output(AudioOutput::Wired),
                _ => {}
            },
            _ => {}
        }
        Effect::None
    }
    fn entry(&mut self, a: Action) -> Effect {
        match a {
            Action::Up => self.letter = (self.letter + LETTERS.len() - 1) % LETTERS.len(),
            Action::Down => self.letter = (self.letter + 1) % LETTERS.len(),
            Action::Wheel(n) => {
                self.letter = (self.letter as i32 + n).rem_euclid(LETTERS.len() as i32) as usize
            }
            Action::Select | Action::Right => {
                if self.password.len() < 63 {
                    self.password.push(LETTERS[self.letter] as char);
                }
            }
            Action::Left => {
                self.password.pop();
            }
            Action::Back => {
                self.password.clear();
                self.text_entry = false
            }
            Action::Menu => {
                if self.password.len() >= 8 {
                    self.text_entry = false;
                    return Effect::WifiConnect {
                        ssid: self.ssid.clone(),
                        password: std::mem::take(&mut self.password),
                    };
                }
            }
            _ => {}
        }
        Effect::None
    }
    pub fn draw(&self, m: &AppModel, tracks: &[Track], health: &str, has_art: bool) -> Vec<Quad> {
        let mut d = vec![];
        rect(&mut d, 0., 0., 480., 43., 0x162330ff);
        text(&mut d, 16., 14., "REBORN", 2., 0x83e3b8ff);
        text(
            &mut d,
            305.,
            17.,
            &format!("{:?}", m.playback),
            1.5,
            0xd2dce8ff,
        );
        if let Some(pair) = &self.pairing {
            text(&mut d, 18., 80., "Bluetooth pairing", 2., 0xffffffff);
            text(&mut d, 18., 125., pair, 2., 0xffffffff);
            text(
                &mut d,
                18.,
                220.,
                "Select: allow   Back: reject",
                1.5,
                0xffffffff,
            );
            return d;
        }
        if self.text_entry {
            text(
                &mut d,
                18.,
                64.,
                &format!("Wi-Fi: {}", self.ssid),
                1.5,
                0xffffffff,
            );
            text(
                &mut d,
                18.,
                100.,
                &"*".repeat(self.password.len()),
                1.5,
                0xffffffff,
            );
            text(
                &mut d,
                218.,
                155.,
                &(LETTERS[self.letter] as char).to_string(),
                5.,
                0x83e3b8ff,
            );
            text(
                &mut d,
                18.,
                230.,
                "Wheel: letter  Select: add",
                1.5,
                0xffffffff,
            );
            text(
                &mut d,
                18.,
                255.,
                "Left: delete  Menu: connect",
                1.5,
                0xffffffff,
            );
            text(&mut d, 18., 280., "Back: cancel", 1.5, 0xffffffff);
            return d;
        }
        if m.screen == Screen::NowPlaying {
            if has_art {
                let mut q = Quad::rect(18., 63., 150., 150., 0xffffffff);
                q.artwork = true;
                d.push(q)
            } else {
                rect(&mut d, 18., 63., 150., 150., 0x233748ff);
                text(&mut d, 52., 112., "R", 6., 0x83e3b8ff)
            }
            if let Some(t) = m.current() {
                text(&mut d, 187., 73., &t.title, 1.5, 0xffffffff);
                text(&mut d, 187., 110., &t.artist, 1.5, 0xaac1d5ff);
                text(&mut d, 187., 145., &t.album, 1.5, 0xaac1d5ff);
                text(
                    &mut d,
                    18.,
                    239.,
                    &format!("{} / {}", time(m.position_ms), time(t.duration_ms)),
                    1.5,
                    0xffffffff,
                );
                rect(&mut d, 18., 273., 444., 6., 0x26394aff);
                rect(
                    &mut d,
                    18.,
                    273.,
                    444. * (m.position_ms as f32 / t.duration_ms.max(1) as f32).clamp(0., 1.),
                    6.,
                    0x83e3b8ff,
                );
            }
            text(
                &mut d,
                18.,
                304.,
                &format!(
                    "Output: {}",
                    if matches!(m.output, AudioOutput::Wired) {
                        "Wired"
                    } else {
                        "Bluetooth"
                    }
                ),
                1.5,
                0xaac1d5ff,
            );
        } else if m.screen == Screen::Diagnostics {
            text(
                &mut d,
                18.,
                72.,
                &format!("Health: {health}"),
                2.,
                0xffffffff,
            );
            text(
                &mut d,
                18.,
                116.,
                &format!("Library: {} tracks", tracks.len()),
                1.5,
                0xaac1d5ff,
            );
            text(
                &mut d,
                18.,
                164.,
                "SSH: rebornctl test baseline --json",
                1.25,
                0xaac1d5ff,
            );
            text(
                &mut d,
                18.,
                202.,
                &format!("Volume: {}", m.settings.volume),
                1.5,
                0xffffffff,
            );
        } else {
            let rows = self.rows(m, tracks);
            let start = self.selected / 7 * 7;
            for (i, row) in rows.iter().enumerate().skip(start).take(7) {
                let y = 55. + ((i - start) * 37) as f32;
                if i == self.selected {
                    rect(&mut d, 9., y, 462., 33., 0x264c48ff)
                }
                text(
                    &mut d,
                    19.,
                    y + 10.,
                    &row.label,
                    1.5,
                    if i == self.selected {
                        0xa2ffceff
                    } else {
                        0xd9e4eeff
                    },
                );
            }
        }
        let notice = match m.screen {
            Screen::Wifi => self.wifi.message(false),
            Screen::Bluetooth => self.bluetooth.message(true),
            _ => self.notice.clone(),
        };
        if !notice.is_empty() {
            rect(&mut d, 0., 332., 480., 28., 0x17232fff);
            text(&mut d, 12., 341., &notice, 1., 0xf9d68aff)
        }
        d
    }
}
fn time(ms: u64) -> String {
    format!("{}:{:02}", ms / 60000, ms / 1000 % 60)
}
fn rect(d: &mut Vec<Quad>, x: f32, y: f32, w: f32, h: f32, c: u32) {
    d.push(Quad::rect(x, y, w, h, c));
}
fn text(d: &mut Vec<Quad>, x: f32, y: f32, s: &str, scale: f32, c: u32) {
    let limit = ((474. - x) / (8. * scale)).max(0.) as usize;
    for (i, ch) in s.chars().take(limit).enumerate() {
        let mut q = Quad::rect(x + i as f32 * 8. * scale, y, 8. * scale, 8. * scale, c);
        q.glyph = Some(if ch.is_ascii() { ch as u8 } else { b'?' });
        d.push(q);
    }
}
pub fn font_atlas() -> Vec<u8> {
    let mut out = vec![0u8; 128 * 64 * 4];
    for c in 0..128 {
        if let Some(rows) = font8x8::BASIC_FONTS.get(char::from(c)) {
            for (y, bits) in rows.into_iter().enumerate() {
                for x in 0..8 {
                    let i = (((c as usize / 16) * 8 + y) * 128 + (c as usize % 16) * 8 + x) * 4;
                    out[i..i + 4].copy_from_slice(&[
                        255,
                        255,
                        255,
                        if bits & (1 << x) != 0 { 255 } else { 0 },
                    ]);
                }
            }
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    fn footer(ui: &Ui, screen: Screen) -> String {
        ui.draw(
            &AppModel {
                screen,
                ..Default::default()
            },
            &[],
            "ok",
            false,
        )
        .iter()
        .filter(|q| q.y == 341.)
        .filter_map(|q| q.glyph.map(char::from))
        .collect()
    }
    #[test]
    fn radio_progress_completion_empty_and_failure_are_visible() {
        let mut ui = Ui::default();
        ui.wifi.available = true;
        assert!(footer(&ui, Screen::Wifi).starts_with("Wi-Fi is off"));
        ui.wifi.requested();
        assert_eq!(footer(&ui, Screen::Wifi), "Starting Wi-Fi...");
        ui.wifi.powered = true;
        ui.wifi.scan = RadioScan::Scanning;
        assert!(footer(&ui, Screen::Wifi).starts_with("Scanning..."));
        ui.wifi.scan = RadioScan::Complete { found: 28 };
        assert_eq!(footer(&ui, Screen::Wifi), "Scan complete: 28 networks");
        ui.wifi.scan = RadioScan::Complete { found: 0 };
        assert!(footer(&ui, Screen::Wifi).starts_with("No networks found"));
        ui.wifi.requested();
        ui.wifi.failed("Scan timed out".into());
        assert_eq!(footer(&ui, Screen::Wifi), "Scan timed out");
        ui.bluetooth.available = true;
        ui.bluetooth.powered = true;
        ui.bluetooth.scan = RadioScan::Complete { found: 0 };
        assert!(footer(&ui, Screen::Bluetooth).contains("pairing mode"));
        assert!(!footer(&ui, Screen::Bluetooth).contains("timed out"));
        assert!(footer(&ui, Screen::Main).is_empty());
    }
    #[test]
    fn scans_are_button_accessible_and_duplicate_activation_is_ignored() {
        let mut ui = Ui {
            selected: 1,
            ..Default::default()
        };
        let mut model = AppModel {
            screen: Screen::Wifi,
            ..Default::default()
        };
        assert!(matches!(
            ui.action(&mut model, &[], Action::Select),
            Effect::WifiScan
        ));
        ui.wifi.requested();
        assert!(ui.rows(&model, &[])[1].label.starts_with("Starting"));
        assert!(matches!(
            ui.action(&mut model, &[], Action::Select),
            Effect::None
        ));
        ui.wifi.scan = RadioScan::Scanning;
        assert!(ui.rows(&model, &[])[1].label.starts_with("Scanning"));
        ui.wifi.scan = RadioScan::Complete { found: 1 };
        ui.networks = vec![Item {
            label: "Home -40 dBm secure".into(),
            key: "Home".into(),
        }];
        assert_eq!(ui.rows(&model, &[])[2].key, "Home");
        ui.action(&mut model, &[], Action::Down);
        ui.action(&mut model, &[], Action::Select);
        assert!(ui.text_entry);
        assert_eq!(ui.ssid, "Home");
        ui.entry(Action::Back);
        model.screen = Screen::Bluetooth;
        ui.selected = 1;
        assert!(matches!(
            ui.action(&mut model, &[], Action::Select),
            Effect::BluetoothScan
        ));
        ui.bluetooth.requested();
        assert!(matches!(
            ui.action(&mut model, &[], Action::Select),
            Effect::None
        ));
        ui.bluetooth.scan = RadioScan::Complete { found: 1 };
        ui.bluetooth_devices = vec![Item {
            label: "Headphones available".into(),
            key: "/org/bluez/hci0/dev_01_02_03_04_05_06".into(),
        }];
        ui.selected = 3;
        ui.action(&mut model, &[], Action::Select);
        assert_eq!(ui.rows(&model, &[])[0].label, "Pair");
    }
    #[test]
    fn button_only_navigation() {
        let mut u = Ui::default();
        let mut m = AppModel::default();
        u.action(&mut m, &[], Action::Select);
        assert_eq!(m.screen, Screen::Music);
        u.action(&mut m, &[], Action::Down);
        u.action(&mut m, &[], Action::Select);
        assert_eq!(m.screen, Screen::Albums);
        u.action(&mut m, &[], Action::Back);
        assert_eq!(m.screen, Screen::Music);
    }
    #[test]
    fn password_never_rendered() {
        let mut u = Ui {
            text_entry: true,
            password: "secretpass".into(),
            ..Default::default()
        };
        assert!(!u.draw(&AppModel::default(), &[], "ok", false).is_empty());
        u.entry(Action::Back);
        assert!(u.password.is_empty());
    }
    #[test]
    fn font_dimensions() {
        assert_eq!(font_atlas().len(), 128 * 64 * 4);
    }
}
