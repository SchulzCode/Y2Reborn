#![forbid(unsafe_code)]
use reborn_core::{atomic_write, RadioScan};
use reborn_observability::{HealthState, Level, Observer};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs, io,
    os::unix::{fs::FileTypeExt, net::UnixDatagram},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Network {
    pub ssid: String,
    pub signal: i32,
    pub security: String,
    pub saved_id: Option<u32>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub available: bool,
    pub enabled: bool,
    pub state: String,
    pub ssid: String,
    pub ip: String,
    pub networks: Vec<Network>,
    pub saved: Vec<Network>,
    pub error: Option<String>,
    #[serde(default)]
    pub scan: RadioScan,
}
pub enum Command {
    Power(bool),
    Scan,
    Connect { ssid: String, password: String },
    Saved(u32),
    Forget(u32),
    Refresh,
    Stop,
}
pub struct Wifi {
    pub commands: SyncSender<Command>,
    pub events: Receiver<Status>,
}
struct Control {
    socket: UnixDatagram,
    path: PathBuf,
    attached: bool,
}
static SEQ: AtomicU64 = AtomicU64::new(1);
#[derive(Clone)]
struct Paths {
    controls: PathBuf,
    clients: PathBuf,
    rfkill: PathBuf,
    preference: PathBuf,
}
impl Default for Paths {
    fn default() -> Self {
        Self {
            controls: "/run/wpa_supplicant".into(),
            clients: "/run/reborn".into(),
            rfkill: "/sys/class/rfkill".into(),
            preference: "/data/network/enabled".into(),
        }
    }
}
impl Paths {
    fn interface(&self) -> Option<PathBuf> {
        fs::read_dir(&self.controls)
            .ok()?
            .filter_map(Result::ok)
            .find(|e| e.file_name() != "global" && e.file_type().is_ok_and(|t| t.is_socket()))
            .map(|e| e.path())
    }
    // Read actual runtime power, including temporary diagnostic activation.
    fn power(&self) -> Option<bool> {
        fs::read_dir(&self.rfkill)
            .ok()?
            .filter_map(Result::ok)
            .find_map(|e| {
                (fs::read_to_string(e.path().join("type")).ok()?.trim() == "wlan").then(|| {
                    fs::read_to_string(e.path().join("soft")).is_ok_and(|s| s.trim() == "0")
                        && fs::read_to_string(e.path().join("hard")).is_ok_and(|s| s.trim() == "0")
                })
            })
    }
    fn radio(&self, on: bool, persist: bool) -> Result<(), String> {
        let mut found = false;
        for e in fs::read_dir(&self.rfkill)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
        {
            if fs::read_to_string(e.path().join("type")).is_ok_and(|s| s.trim() == "wlan") {
                fs::write(e.path().join("soft"), if on { "0" } else { "1" })
                    .map_err(|e| e.to_string())?;
                found = true;
            }
        }
        if !found {
            return Err("Wi-Fi radio unavailable".into());
        }
        if persist {
            atomic_write(&self.preference, if on { b"1\n" } else { b"0\n" })
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
impl Control {
    fn open() -> Result<Self, String> {
        Self::at(&Paths::default())
    }
    fn at(paths: &Paths) -> Result<Self, String> {
        let remote = paths.interface().ok_or("Wi-Fi interface is not ready")?;
        Self::connect_to(&remote, &paths.clients)
    }
    fn connect_to(remote: &Path, clients: &Path) -> Result<Self, String> {
        let path = clients.join(format!(
            "wpa-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let socket = UnixDatagram::bind(&path).map_err(|e| e.to_string())?;
        // Own the pathname before any fallible setup so failures also unlink it.
        let c = Self {
            socket,
            path,
            attached: false,
        };
        c.socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        c.socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        c.socket.connect(remote).map_err(|e| e.to_string())?;
        Ok(c)
    }
    fn request(&self, cmd: &str) -> Result<String, String> {
        if cmd.len() > 4096 || cmd.contains('\n') || cmd.contains('\r') {
            return Err("invalid control request".into());
        }
        self.socket
            .send(cmd.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut buf = [0; 32768];
        let n = self.socket.recv(&mut buf).map_err(|e| e.to_string())?;
        let s = String::from_utf8_lossy(&buf[..n])
            .trim_end_matches(['\n', '\r'])
            .to_owned();
        if s.starts_with("FAIL") {
            return Err("wpa_supplicant rejected operation".into());
        }
        Ok(s)
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        if self.attached {
            let _ = self.socket.send(b"DETACH");
        }
        let _ = fs::remove_file(&self.path);
    }
}
fn quote(s: &str) -> Result<String, String> {
    if s.len() > 128 || s.chars().any(|c| c.is_control()) {
        return Err("invalid text length or control character".into());
    }
    Ok(format!(
        "\"{}\"",
        s.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
pub fn enabled() -> bool {
    Paths::default().power().unwrap_or(false)
}
pub fn radio(on: bool, persist: bool) -> Result<(), String> {
    Paths::default().radio(on, persist)
}
fn connect(c: &Control, ssid: &str, password: &str) -> Result<(), String> {
    if ssid.is_empty() || ssid.len() > 32 || !(8..=63).contains(&password.len()) {
        return Err("WPA2 requires SSID 1..32 bytes and password 8..63 bytes".into());
    }
    let ssid = quote(ssid)?;
    let psk = quote(password)?;
    let id = c
        .request("ADD_NETWORK")?
        .parse::<u32>()
        .map_err(|_| "invalid network id")?;
    let result = (|| {
        c.request(&format!("SET_NETWORK {id} ssid {ssid}"))?;
        c.request(&format!("SET_NETWORK {id} psk {psk}"))?;
        c.request(&format!("SET_NETWORK {id} key_mgmt WPA-PSK"))?;
        c.request(&format!("SELECT_NETWORK {id}"))?;
        c.request("SAVE_CONFIG")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = c.request(&format!("REMOVE_NETWORK {id}"));
    }
    result
}
fn networks(text: &str) -> Result<Vec<Network>, String> {
    if !text.starts_with("bssid / frequency / signal level / flags / ssid") {
        return Err("Invalid Wi-Fi scan response".into());
    }
    let mut out: Vec<Network> = text
        .lines()
        .skip(1)
        .take(256)
        .filter_map(|line| {
            let p = line.splitn(5, '\t').collect::<Vec<_>>();
            if p.len() != 5 || p[4].is_empty() {
                return None;
            }
            Some(Network {
                ssid: p[4].into(),
                signal: p[2].parse().ok()?,
                security: p[3].into(),
                saved_id: None,
            })
        })
        .collect();
    out.sort_by(|a, b| b.signal.cmp(&a.signal).then(a.ssid.cmp(&b.ssid)));
    let mut seen = std::collections::HashSet::new();
    out.retain(|n| seen.insert((n.ssid.clone(), n.security.clone())));
    Ok(out)
}
fn status() -> Status {
    status_at(&Paths::default())
}
fn status_at(paths: &Paths) -> Status {
    let mut s = Status {
        enabled: paths.power().unwrap_or(false),
        ..Default::default()
    };
    let service = Control::connect_to(&paths.controls.join("global"), &paths.clients)
        .and_then(|c| c.request("PING"))
        .is_ok_and(|s| s == "PONG");
    s.available = paths.power().is_some() && service;
    if !s.available {
        s.state = "unavailable".into();
        s.error = Some(
            if service {
                "Wi-Fi radio unavailable"
            } else {
                "Wi-Fi service unavailable"
            }
            .into(),
        );
        return s;
    }
    if !s.enabled {
        s.state = "OFF".into();
        return s;
    }
    match Control::at(paths) {
        Err(_) if paths.interface().is_none() => {
            s.state = "STARTING".into();
        }
        Err(e) => {
            s.state = "ERROR".into();
            s.error = Some(e);
        }
        Ok(c) => match c.request("STATUS") {
            Err(e) => {
                s.state = "ERROR".into();
                s.error = Some(e);
            }
            Ok(text) => {
                for line in text.lines() {
                    if let Some((k, v)) = line.split_once('=') {
                        match k {
                            "wpa_state" => s.state = v.into(),
                            "ssid" => s.ssid = v.into(),
                            "ip_address" => s.ip = v.into(),
                            _ => {}
                        }
                    }
                }
                if let Ok(found) = c.request("SCAN_RESULTS").and_then(|s| networks(&s)) {
                    s.networks = found;
                }
                if let Ok(text) = c.request("LIST_NETWORKS") {
                    s.saved = text
                        .lines()
                        .skip(1)
                        .take(128)
                        .filter_map(|l| {
                            let p = l.split('\t').collect::<Vec<_>>();
                            Some(Network {
                                saved_id: Some(p.first()?.parse().ok()?),
                                ssid: p.get(1)?.to_string(),
                                ..Default::default()
                            })
                        })
                        .collect();
                }
            }
        },
    }
    s
}
struct Scan {
    id: u64,
    deadline: Instant,
    monitor: Option<Control>,
}
impl Scan {
    fn new(id: u64, now: Instant) -> Self {
        Self {
            id,
            deadline: now + Duration::from_secs(15),
            monitor: None,
        }
    }
    fn step(&mut self, paths: &Paths, state: &mut Status, now: Instant) -> Result<(), String> {
        if now >= self.deadline {
            return Err(if self.monitor.is_some() {
                "Wi-Fi scan timed out"
            } else {
                "Wi-Fi startup timed out"
            }
            .into());
        }
        if !paths.power().unwrap_or(false) {
            return Err("Wi-Fi was turned off".into());
        }
        if self.monitor.is_none() {
            if paths.interface().is_none() {
                return Ok(());
            }
            // Monitor events on a separate socket: replies and unsolicited events
            // must never be mistaken for each other. Attach before starting scan.
            let mut monitor = Control::at(paths)?;
            monitor.request("ATTACH")?;
            monitor.attached = true;
            monitor
                .socket
                .set_nonblocking(true)
                .map_err(|e| e.to_string())?;
            Control::at(paths)?.request("SCAN")?;
            self.monitor = Some(monitor);
            self.deadline = now + Duration::from_secs(15);
            state.scan = RadioScan::Scanning;
            return Ok(());
        }
        let monitor = self.monitor.as_ref().unwrap();
        for _ in 0..32 {
            let mut bytes = [0; 4096];
            let n = match monitor.socket.recv(&mut bytes) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.to_string()),
            };
            let text = String::from_utf8_lossy(&bytes[..n]);
            let event = text.split_once('>').map(|(_, s)| s).unwrap_or(&text).trim();
            if event.starts_with("CTRL-EVENT-SCAN-FAILED") {
                return Err("Wi-Fi scan failed; try again".into());
            }
            if event == "CTRL-EVENT-SCAN-RESULTS" {
                state.networks = networks(&Control::at(paths)?.request("SCAN_RESULTS")?)?;
                state.scan = RadioScan::Complete {
                    found: state.networks.len(),
                };
                break;
            }
        }
        Ok(())
    }
}
impl Wifi {
    pub fn spawn(log: Observer) -> io::Result<Self> {
        let (tx, rx) = sync_channel(8);
        let (et, er) = sync_channel(2);
        thread::Builder::new()
            .name("wifi".into())
            .spawn(move || run(log, rx, et, Paths::default()))?;
        Ok(Self {
            commands: tx,
            events: er,
        })
    }
}
fn run(log: Observer, rx: Receiver<Command>, et: SyncSender<Status>, paths: Paths) {
    let mut current = Status::default();
    let mut scan: Option<Scan> = None;
    let mut last = String::new();
    let mut operation_error = None;
    let mut refresh = Instant::now() - Duration::from_secs(4);
    let mut publish = true;
    loop {
        let wait = if scan.is_some() || publish {
            Duration::from_millis(200)
        } else {
            Duration::from_secs(3)
        };
        let command = match rx.recv_timeout(wait) {
            Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => Command::Refresh,
        };
        let id = log.correlation();
        let explicit = !matches!(command, Command::Refresh);
        let scanning = matches!(command, Command::Scan);
        let duplicate_scan = scanning && scan.is_some();
        if explicit && !duplicate_scan {
            operation_error = None;
            current.error = None;
        }
        let result = match command {
            Command::Stop | Command::Refresh => Ok(()),
            Command::Power(on) => {
                if !on || scan.is_none() {
                    scan = None;
                    current.scan = RadioScan::Idle;
                }
                log.emit(
                    Level::Info,
                    "wifi",
                    "radio_power",
                    "Wi-Fi radio requested",
                    Some(id),
                    json!({"enabled":on}),
                );
                paths.radio(on, true)
            }
            Command::Scan if duplicate_scan => Ok(()),
            Command::Scan => {
                current.scan = RadioScan::Starting;
                let _ = et.try_send(current.clone());
                log.emit(
                    Level::Info,
                    "wifi",
                    "scan_start",
                    "Wi-Fi scan requested",
                    Some(id),
                    json!({"enable_if_off":true}),
                );
                scan = Some(Scan::new(id, Instant::now()));
                paths.radio(true, true)
            }
            Command::Connect { ssid, password } => {
                log.emit(
                    Level::Info,
                    "wifi",
                    "connect",
                    "Wi-Fi connection requested",
                    Some(id),
                    json!({"ssid":ssid}),
                );
                Control::at(&paths).and_then(|c| connect(&c, &ssid, &password))
            }
            Command::Saved(id) => Control::at(&paths)
                .and_then(|c| c.request(&format!("SELECT_NETWORK {id}")).map(|_| ())),
            Command::Forget(id) => Control::at(&paths).and_then(|c| {
                c.request(&format!("REMOVE_NETWORK {id}"))?;
                c.request("SAVE_CONFIG").map(|_| ())
            }),
        };
        if let Err(e) = result {
            log.add("wifi_errors", 1.);
            log.emit(
                Level::Warn,
                "wifi",
                "operation_failed",
                &e,
                Some(id),
                json!({"interface":"wpa_supplicant control","recovery_attempted":false}),
            );
            if scanning {
                current.scan = RadioScan::Failed { message: e.clone() };
                scan = None;
            }
            operation_error = Some(e);
        }
        if explicit || refresh.elapsed() >= Duration::from_secs(3) {
            let mut s = status_at(&paths);
            s.scan = current.scan.clone();
            current = s;
            refresh = Instant::now();
            publish = true;
        }
        if let Some(job) = scan.as_mut() {
            let previous = current.scan.clone();
            match job.step(&paths, &mut current, Instant::now()) {
                Ok(()) => {
                    if current.scan == RadioScan::Scanning && current.scan != previous {
                        log.emit(
                            Level::Info,
                            "wifi",
                            "scan_started",
                            "Wi-Fi discovery active",
                            Some(job.id),
                            json!({}),
                        );
                    }
                    if let RadioScan::Complete { found } = current.scan {
                        log.emit(
                            Level::Info,
                            "wifi",
                            "scan_complete",
                            "Wi-Fi scan completed",
                            Some(job.id),
                            json!({"count":found}),
                        );
                    }
                }
                Err(e) => {
                    log.add("wifi_errors", 1.);
                    log.emit(
                        Level::Warn,
                        "wifi",
                        "scan_failed",
                        &e,
                        Some(job.id),
                        json!({"recovery_attempted":false}),
                    );
                    current.scan = RadioScan::Failed { message: e.clone() };
                    operation_error = Some(e);
                }
            }
            publish |= current.scan != previous;
            if !current.scan.active() {
                scan = None;
            }
        }
        if operation_error.is_some() {
            current.error = operation_error.clone();
        }
        if current.state != last {
            log.emit(
                Level::Info,
                "wifi",
                "state_changed",
                "Wi-Fi state changed",
                Some(id),
                json!({"state":current.state,"ip":current.ip}),
            );
            if current.state == "COMPLETED" {
                log.add("wifi_connects", 1.);
            } else if last == "COMPLETED" {
                log.add("wifi_disconnects", 1.);
            }
            last = current.state.clone();
        }
        log.health_set(
            "wifi",
            if !current.available {
                HealthState::Unavailable
            } else if current.error.is_some() {
                HealthState::Degraded
            } else {
                HealthState::Ok
            },
            false,
            current.error.as_deref().unwrap_or(&current.state),
        );
        if publish {
            publish = et.try_send(current.clone()).is_err();
        }
    }
}
pub fn scan_test() -> Result<serde_json::Value, String> {
    let paths = Paths::default();
    let before = paths.power().unwrap_or(false);
    let result = (|| {
        paths.radio(true, false)?;
        let mut scan = Scan::new(0, Instant::now());
        let mut s = Status {
            scan: RadioScan::Starting,
            ..Default::default()
        };
        loop {
            scan.step(&paths, &mut s, Instant::now())?;
            if let RadioScan::Complete { found } = s.scan {
                return Ok(
                    json!({"passed":true,"count":found,"available":true,"completion":"supplicant_event"}),
                );
            }
            thread::sleep(Duration::from_millis(200));
        }
    })();
    if !before {
        paths.radio(false, false)?;
    }
    result
}
pub fn saved_test(id: u32) -> Result<serde_json::Value, String> {
    let c = Control::open()?;
    let saved = c.request("LIST_NETWORKS")?;
    if !saved
        .lines()
        .skip(1)
        .any(|l| l.split('\t').next().and_then(|s| s.parse::<u32>().ok()) == Some(id))
    {
        return Err("saved network id not configured".into());
    }
    c.request(&format!("SELECT_NETWORK {id}"))?;
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let s = status();
        if s.state == "COMPLETED" && !s.ip.is_empty() {
            return Ok(json!({"passed":true,"state":s.state,"dhcp":true}));
        }
        if Instant::now() > until {
            return Err("saved network association/DHCP timed out".into());
        }
        thread::sleep(Duration::from_millis(250));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize},
        Arc, Mutex,
    };
    const HEADER: &str = "bssid / frequency / signal level / flags / ssid\n";
    struct Supplicant {
        root: PathBuf,
        paths: Paths,
        interface: UnixDatagram,
        monitor: Arc<Mutex<Option<PathBuf>>>,
        scans: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }
    impl Supplicant {
        fn new(results: String) -> Self {
            let root = std::env::temp_dir().join(format!(
                "rb-wifi-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let paths = Paths {
                controls: root.join("wpa"),
                clients: root.join("clients"),
                rfkill: root.join("rfkill"),
                preference: root.join("enabled"),
            };
            for d in [&paths.controls, &paths.clients, &paths.rfkill.join("radio")] {
                fs::create_dir_all(d).unwrap();
            }
            for (name, value) in [("type", "wlan\n"), ("soft", "1\n"), ("hard", "0\n")] {
                fs::write(paths.rfkill.join("radio").join(name), value).unwrap();
            }
            fs::write(&paths.preference, "0\n").unwrap();
            let global = UnixDatagram::bind(paths.controls.join("global")).unwrap();
            let interface = UnixDatagram::bind(paths.controls.join("test-radio")).unwrap();
            let iface = interface.try_clone().unwrap();
            global.set_nonblocking(true).unwrap();
            iface.set_nonblocking(true).unwrap();
            let monitor = Arc::new(Mutex::new(None));
            let m = monitor.clone();
            let scans = Arc::new(AtomicUsize::new(0));
            let count = scans.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let ending = stop.clone();
            let worker = thread::spawn(move || {
                while !ending.load(Ordering::Relaxed) {
                    for socket in [&global, &iface] {
                        let mut buf = [0; 4096];
                        let Ok((n, from)) = socket.recv_from(&mut buf) else {
                            continue;
                        };
                        let from = from.as_pathname().unwrap();
                        let command = std::str::from_utf8(&buf[..n]).unwrap();
                        let response = match command {
                            "PING" => "PONG\n",
                            "ATTACH" => {
                                *m.lock().unwrap() = Some(from.to_path_buf());
                                "OK\n"
                            }
                            "DETACH" => {
                                *m.lock().unwrap() = None;
                                "OK\n"
                            }
                            "SCAN" => {
                                count.fetch_add(1, Ordering::Relaxed);
                                "OK\n"
                            }
                            "STATUS" => "wpa_state=DISCONNECTED\n",
                            "SCAN_RESULTS" => &results,
                            "LIST_NETWORKS" => "network id / ssid / bssid / flags\n",
                            _ => "FAIL\n",
                        };
                        let _ = socket.send_to(response.as_bytes(), from);
                    }
                    thread::sleep(Duration::from_millis(2));
                }
            });
            Self {
                root,
                paths,
                interface,
                monitor,
                scans,
                stop,
                worker: Some(worker),
            }
        }
        fn event(&self, text: &str) {
            self.interface
                .send_to(
                    text.as_bytes(),
                    self.monitor.lock().unwrap().as_ref().unwrap(),
                )
                .unwrap();
        }
    }
    impl Drop for Supplicant {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap();
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn wait_for(rx: &Receiver<Status>, test: impl Fn(&Status) -> bool) -> Status {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let s = rx
                .recv_timeout(until.saturating_duration_since(Instant::now()))
                .unwrap();
            if test(&s) {
                return s;
            }
        }
    }
    #[test]
    fn disabled_radio_is_off_and_healthy_without_an_interface() {
        let fake = Supplicant::new(HEADER.into());
        fs::remove_file(fake.paths.controls.join("test-radio")).unwrap();
        let s = status_at(&fake.paths);
        assert!(s.available && !s.enabled && s.error.is_none());
        assert_eq!(s.state, "OFF");
        assert!(fs::read_dir(&fake.paths.clients).unwrap().next().is_none());
    }
    #[test]
    fn worker_scan_enables_radio_publishes_progress_and_keeps_results() {
        let fake = Supplicant::new(format!("{HEADER}a\t2412\t-70\t[WPA2]\tHome\nb\t2412\t-30\t[WPA2]\tHome\nc\t2412\t-45\t[ESS]\tGuest\n"));
        let log = Observer::new(&fake.root.join("logs")).unwrap();
        let observer = log.clone();
        let (tx, rx) = sync_channel(8);
        let (et, er) = sync_channel(2);
        let paths = fake.paths.clone();
        let worker = thread::spawn(move || run(observer, rx, et, paths));
        tx.send(Command::Scan).unwrap();
        wait_for(&er, |s| s.scan == RadioScan::Starting);
        wait_for(&er, |s| s.scan == RadioScan::Scanning);
        assert_eq!(fake.paths.power(), Some(true));
        assert_eq!(fs::read_to_string(&fake.paths.preference).unwrap(), "1\n");
        tx.send(Command::Scan).unwrap(); // does not restart an active scan
        fake.event("<3>CTRL-EVENT-SCAN-RESULTS");
        let done = wait_for(&er, |s| matches!(s.scan, RadioScan::Complete { .. }));
        assert_eq!(done.scan, RadioScan::Complete { found: 2 });
        assert_eq!(done.networks[0].ssid, "Home");
        assert_eq!(done.networks[0].signal, -30);
        assert_eq!(fake.scans.load(Ordering::Relaxed), 1);
        tx.send(Command::Refresh).unwrap();
        wait_for(&er, |s| s.scan == RadioScan::Complete { found: 2 });
        tx.send(Command::Power(false)).unwrap();
        let off = wait_for(&er, |s| s.state == "OFF");
        assert_eq!(off.scan, RadioScan::Idle);
        assert!(off.networks.is_empty() && off.error.is_none());
        tx.send(Command::Stop).unwrap();
        worker.join().unwrap();
        let events = log.events(100, Some("wifi"), None, None);
        let scans: Vec<_> = events
            .iter()
            .filter(|e| e.event.starts_with("scan_"))
            .collect();
        assert_eq!(scans.len(), 3);
        assert!(scans
            .iter()
            .all(|e| e.correlation_id == scans[0].correlation_id));
    }
    #[test]
    fn empty_scan_completes_and_scan_timeout_is_bounded() {
        let fake = Supplicant::new(HEADER.into());
        fake.paths.radio(true, false).unwrap();
        let now = Instant::now();
        let mut scan = Scan::new(1, now);
        let mut s = Status {
            scan: RadioScan::Starting,
            ..Default::default()
        };
        scan.step(&fake.paths, &mut s, now).unwrap();
        fake.event("<3>CTRL-EVENT-SCAN-RESULTS");
        scan.step(&fake.paths, &mut s, now + Duration::from_secs(1))
            .unwrap();
        assert_eq!(s.scan, RadioScan::Complete { found: 0 });
        assert!(scan
            .step(&fake.paths, &mut s, now + Duration::from_secs(16))
            .unwrap_err()
            .contains("timed out"));
    }
    #[test]
    fn scan_waits_for_interface_then_times_out_without_false_success() {
        let fake = Supplicant::new(HEADER.into());
        fs::remove_file(fake.paths.controls.join("test-radio")).unwrap();
        fake.paths.radio(true, false).unwrap();
        let now = Instant::now();
        let mut scan = Scan::new(1, now);
        let mut s = Status {
            scan: RadioScan::Starting,
            ..Default::default()
        };
        scan.step(&fake.paths, &mut s, now).unwrap();
        assert_eq!(s.scan, RadioScan::Starting);
        assert_eq!(fake.scans.load(Ordering::Relaxed), 0);
        assert!(scan
            .step(&fake.paths, &mut s, now + Duration::from_secs(16))
            .unwrap_err()
            .contains("startup timed out"));
    }
    #[test]
    fn scan_failure_and_malformed_results_are_errors() {
        for (results, event, expected) in [
            (HEADER, "<3>CTRL-EVENT-SCAN-FAILED ret=-5", "scan failed"),
            ("garbage", "<3>CTRL-EVENT-SCAN-RESULTS", "Invalid Wi-Fi"),
        ] {
            let fake = Supplicant::new(results.into());
            fake.paths.radio(true, false).unwrap();
            let now = Instant::now();
            let mut scan = Scan::new(1, now);
            let mut s = Status {
                scan: RadioScan::Starting,
                ..Default::default()
            };
            scan.step(&fake.paths, &mut s, now).unwrap();
            fake.event(event);
            assert!(scan
                .step(&fake.paths, &mut s, now + Duration::from_secs(1))
                .unwrap_err()
                .contains(expected));
        }
    }
    #[test]
    fn failed_socket_setup_removes_client_path() {
        let fake = Supplicant::new(HEADER.into());
        assert!(Control::connect_to(&fake.root.join("missing"), &fake.paths.clients).is_err());
        assert!(fs::read_dir(&fake.paths.clients).unwrap().next().is_none());
    }
    #[test]
    fn password_encoding_cannot_inject_commands() {
        assert!(quote("x\nREMOVE_NETWORK all").is_err());
        assert_eq!(quote("a\"b\\c").unwrap(), "\"a\\\"b\\\\c\"");
    }
}
