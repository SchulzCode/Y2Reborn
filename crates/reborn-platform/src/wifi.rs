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
use y2p_contract::network::Published;
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Network {
    pub ssid: String,
    pub signal: i32,
    pub security: String,
    pub saved_id: Option<u32>,
}
impl Network {
    /// The connector supports open and WPA-PSK networks. Enterprise, WEP and
    /// SAE-only networks must never be accidentally treated as open networks.
    pub fn password_required(&self) -> Option<bool> {
        if self.security.contains("PSK") {
            Some(true)
        } else if self.security.is_empty()
            || self.security == "[ESS]"
            || self.security == "[WPS][ESS]"
        {
            Some(false)
        } else {
            None
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub available: bool,
    pub enabled: bool,
    pub state: String,
    #[serde(default)]
    pub readiness: String,
    #[serde(default)]
    pub readiness_reason: Option<String>,
    pub ssid: String,
    #[serde(default)]
    pub association_id: Option<String>,
    pub ip: String,
    pub networks: Vec<Network>,
    pub saved: Vec<Network>,
    pub error: Option<String>,
    #[serde(default)]
    pub scan: RadioScan,
    /// The owner's last join (a typed password or a saved network) and how it
    /// ended; kept until the next one.
    #[serde(default)]
    pub join: Option<Join>,
    /// The saved network whose password failed (the platform's record).
    #[serde(default)]
    pub failed_network_id: Option<u32>,
}
/// A join is tried before it is saved (Y2Reborn #20: a wrong password was
/// saved at once, the list said "Saved" and nothing ever said why).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Join {
    pub attempt: u64,
    pub ssid: String,
    pub outcome: JoinOutcome,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinOutcome {
    Checking,
    Joined,
    WrongPassword,
    NotFound,
    Failed,
}
pub enum Command {
    Power(bool),
    Scan,
    Connect { ssid: String, password: String },
    Saved(u32),
    Forget(u32),
    Disconnect,
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
        // STATUS may queue behind a ~2.1 s vendor station-statistics query.
        // Keep mutations at their existing bound; do not turn that delay into
        // a false ERROR observation.
        self.socket
            .set_read_timeout(Some(Duration::from_secs(
                if matches!(cmd, "PING" | "STATUS") {
                    3
                } else {
                    2
                },
            )))
            .map_err(|e| e.to_string())?;
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
/// wpa_supplicant reads a `"..."` value literally up to its LAST quote and
/// knows no escapes, so a password is sent as it is. Escaping it stored `\`
/// and `"` doubled and such a password could never connect.
fn quote(s: &str) -> Result<String, String> {
    if s.len() > 128 || s.chars().any(|c| c.is_control()) {
        return Err("invalid text length or control character".into());
    }
    Ok(format!("\"{s}\""))
}
/// A network name goes as hex: every byte, quotes included, survives
/// SET_NETWORK and SAVE_CONFIG unchanged.
fn hex_ssid(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}
pub fn enabled() -> bool {
    Paths::default().power().unwrap_or(false)
}

/// Whether the supplicant control interface exists (diagnostic probe).
pub fn service_available() -> bool {
    Paths::default().controls.join("global").exists()
}

/// The user-facing problem, if the current Wi-Fi observation has one.
/// Supplicant and readiness vocabulary never leaves this crate.
pub fn problem(s: &Status) -> Option<reborn_core::platform::WifiProblem> {
    use reborn_core::platform::WifiProblem;
    let reason = s
        .readiness_reason
        .as_deref()
        .or(s.error.as_deref())
        .unwrap_or("")
        .to_ascii_lowercase();
    // A join being checked says its own outcome; a failed network that is no
    // longer saved (a removed join, a forgotten network) is not a problem.
    if s.join
        .as_ref()
        .is_some_and(|j| j.outcome == JoinOutcome::Checking)
        || s.failed_network_id
            .is_some_and(|id| !s.saved.iter().any(|n| n.saved_id == Some(id)))
    {
        return None;
    }
    let dns = reason.contains("dns");
    if s.readiness == "Authenticated" && dns {
        return Some(WifiProblem::NoInternetNames);
    }
    if s.readiness != "Failed" && s.error.is_none() {
        return None;
    }
    // Only the platform's password verdict or our own password check names
    // the password. Any `auth` (F52) also caught `not_authenticated`, the
    // platform's plain "not connected", and showed "wrong password" after
    // every wake that did not reconnect at once.
    Some(
        if matches!(reason.as_str(), "wrong_credentials" | "wrong_key")
            || reason.contains("password")
        {
            WifiProblem::WrongPassword
        } else if reason.contains("dhcp") || reason.contains("address") {
            WifiProblem::NoAddress
        } else if dns {
            WifiProblem::NoInternetNames
        } else if reason.contains("not_found")
            || reason.contains("no_network")
            || reason == "ap_unavailable"
        {
            WifiProblem::NetworkNotFound
        } else {
            WifiProblem::Other
        },
    )
}
pub fn radio(on: bool, persist: bool) -> Result<(), String> {
    Paths::default().radio(on, persist)
}
/// A new network block for a join, not selected and not saved yet.
fn add_network(c: &Control, ssid: &str, password: &str) -> Result<u32, String> {
    if ssid.is_empty()
        || ssid.len() > 32
        || (!password.is_empty() && !(8..=63).contains(&password.len()))
    {
        return Err("WPA2 requires SSID 1..32 bytes and password 8..63 bytes".into());
    }
    let ssid = hex_ssid(ssid);
    let psk = quote(password)?;
    let id = c
        .request("ADD_NETWORK")?
        .parse::<u32>()
        .map_err(|_| "invalid network id")?;
    let result = (|| {
        c.request(&format!("SET_NETWORK {id} ssid {ssid}"))?;
        if password.is_empty() {
            c.request(&format!("SET_NETWORK {id} key_mgmt NONE"))?;
        } else {
            c.request(&format!("SET_NETWORK {id} psk {psk}"))?;
            c.request(&format!("SET_NETWORK {id} key_mgmt WPA-PSK"))?;
        }
        Ok(id)
    })();
    if result.is_err() {
        let _ = c.request(&format!("REMOVE_NETWORK {id}"));
    }
    result
}

/// How long a join may take before it counts as failed. A wrong password is
/// known after one 4-way handshake (≈10 s on the Y2, owner's player 8.27).
const JOIN_SECONDS: u64 = 30;

/// One join being checked: the supplicant's events say whether it connected,
/// the password was wrong or the network is not in range.
struct PendingJoin {
    attempt: u64,
    ssid: String,
    id: u32,
    /// A new block (a typed password): removed again unless it connected.
    added: bool,
    deadline: Instant,
    monitor: Control,
    auth_failures: u32,
    not_found: u32,
    status_at: Instant,
    /// The supplicant's own handshake timeout came before the disconnect.
    timed_out: bool,
    /// The last disconnect was ours (a sleep's radio off, another join), not
    /// the access point's or a timeout.
    interrupted: bool,
}

/// `id=7` in a supplicant event (`[id=7 id_str=]`, `id=7 ssid=…`).
fn event_network(event: &str) -> Option<u32> {
    let at = event.find(" id=").or_else(|| event.find("[id="))? + 4;
    event[at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// A GET_NETWORK ssid answer (`"name"` or hex) as the hex `hex_ssid` sends.
fn ssid_hex(value: &str) -> String {
    match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(name) => hex_ssid(name),
        None => value.to_ascii_lowercase(),
    }
}

impl PendingJoin {
    fn start(
        paths: &Paths,
        attempt: u64,
        ssid: &str,
        password: Option<&str>,
        saved: Option<u32>,
    ) -> Result<Self, String> {
        let c = Control::at(paths)?;
        // Attach before selecting: no event of this join may be missed.
        let mut monitor = Control::at(paths)?;
        monitor.request("ATTACH")?;
        monitor.attached = true;
        monitor
            .socket
            .set_nonblocking(true)
            .map_err(|e| e.to_string())?;
        let (id, added) = match saved {
            Some(id) => (id, false),
            None => (add_network(&c, ssid, password.unwrap_or(""))?, true),
        };
        if let Err(e) = c.request(&format!("SELECT_NETWORK {id}")) {
            if added {
                let _ = c.request(&format!("REMOVE_NETWORK {id}"));
            }
            return Err(e);
        }
        let now = Instant::now();
        Ok(Self {
            attempt,
            ssid: ssid.to_owned(),
            id,
            added,
            deadline: now + Duration::from_secs(JOIN_SECONDS),
            monitor,
            auth_failures: 0,
            not_found: 0,
            status_at: now,
            timed_out: false,
            interrupted: false,
        })
    }

    /// The outcome once there is one.
    fn step(&mut self, paths: &Paths, now: Instant) -> Option<JoinOutcome> {
        for _ in 0..32 {
            let mut bytes = [0; 4096];
            let n = match self.monitor.socket.recv(&mut bytes) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            };
            let text = String::from_utf8_lossy(&bytes[..n]);
            let event = text.split_once('>').map(|(_, s)| s).unwrap_or(&text).trim();
            let ours = event_network(event) == Some(self.id);
            if event.starts_with("CTRL-EVENT-CONNECTED") && ours {
                return Some(JoinOutcome::Joined);
            }
            // wpa_supplicant says WRONG_KEY for any handshake that ended
            // early, also one cut short on this side (F52). Its timeout (a
            // wrong password on the Y2 ends so, ≈10 s) disconnects locally too.
            if event.starts_with("Authentication with ") && event.ends_with("timed out.") {
                self.timed_out = true;
            } else if event.starts_with("CTRL-EVENT-DISCONNECTED") {
                self.interrupted = event.contains("locally_generated=1") && !self.timed_out;
                self.timed_out = false;
            }
            if event.starts_with("CTRL-EVENT-SSID-TEMP-DISABLED") && ours {
                let interrupted = std::mem::take(&mut self.interrupted);
                if event.contains("reason=WRONG_KEY") {
                    if !interrupted {
                        return Some(JoinOutcome::WrongPassword);
                    }
                    continue;
                }
                self.auth_failures += 1;
            } else if event.starts_with("CTRL-EVENT-AUTH-REJECT") {
                self.auth_failures += 1;
            } else if event.starts_with("CTRL-EVENT-NETWORK-NOT-FOUND") {
                self.not_found += 1;
            }
            if self.auth_failures >= 2 {
                return Some(JoinOutcome::Failed);
            }
            if self.not_found >= 3 {
                return Some(JoinOutcome::NotFound);
            }
        }
        // A missed event: the supplicant's own state, once a second.
        if now.duration_since(self.status_at) >= Duration::from_secs(1) {
            self.status_at = now;
            if let Ok(text) = Control::at(paths).and_then(|c| c.request("STATUS")) {
                let field = |k: &str| {
                    text.lines()
                        .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
                        .map(str::to_owned)
                };
                if field("wpa_state").as_deref() == Some("COMPLETED")
                    && field("id").and_then(|v| v.parse().ok()) == Some(self.id)
                {
                    return Some(JoinOutcome::Joined);
                }
            }
        }
        if now >= self.deadline {
            return Some(if self.not_found > 0 && self.auth_failures == 0 {
                JoinOutcome::NotFound
            } else {
                JoinOutcome::Failed
            });
        }
        None
    }

    /// Keep a network that connected (and drop older blocks of the same name);
    /// remove one that did not. Every other saved network is enabled again
    /// (SELECT_NETWORK disabled them, and the old flow saved them disabled).
    fn finish(&self, paths: &Paths, outcome: JoinOutcome) -> Result<(), String> {
        let c = Control::at(paths)?;
        if outcome == JoinOutcome::Joined {
            if self.added {
                let name = hex_ssid(&self.ssid);
                let list = c.request("LIST_NETWORKS")?;
                for other in list
                    .lines()
                    .skip(1)
                    .filter_map(|l| l.split('\t').next()?.parse::<u32>().ok())
                    .filter(|other| *other != self.id)
                {
                    if c.request(&format!("GET_NETWORK {other} ssid"))
                        .is_ok_and(|v| ssid_hex(&v) == name)
                    {
                        c.request(&format!("REMOVE_NETWORK {other}"))?;
                    }
                }
            }
            c.request("ENABLE_NETWORK all")?;
            c.request("SAVE_CONFIG")?;
        } else {
            if self.added {
                c.request(&format!("REMOVE_NETWORK {}", self.id))?;
            }
            c.request("ENABLE_NETWORK all")?;
        }
        Ok(())
    }
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
    status_with_inventory(paths, None)
}
fn status_with_inventory(paths: &Paths, cached: Option<&Status>) -> Status {
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
                            "bssid" => s.association_id = Some(v.into()),
                            "ip_address" => s.ip = v.into(),
                            _ => {}
                        }
                    }
                }
                // Discovery is explicit. Reading the same scan/saved inventory
                // every status tick adds control traffic without finding new APs.
                // Refresh on user operations, association changes and every 30 s.
                if let Some(old) = cached.filter(|old| {
                    old.state == s.state
                        && old.ssid == s.ssid
                        && old.association_id == s.association_id
                }) {
                    s.networks = old.networks.clone();
                    s.saved = old.saved.clone();
                } else {
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
            }
        },
    }
    if paths.controls == Path::new("/run/wpa_supplicant")
        && Path::new("/etc/y2linux/platform-contract").exists()
    {
        let record = fs::read("/run/y2/network.json")
            .ok()
            .filter(|b| b.len() <= 65536)
            .and_then(|b| serde_json::from_slice::<Published>(&b).ok());
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
        let observed = record.and_then(|r| {
            let state =
                network_readiness(&r, boot.trim(), crate::native::monotonic_seconds()?, &s)?;
            Some((
                state,
                r.failed_network_id.and_then(|v| u32::try_from(v).ok()),
            ))
        });
        if let Some(((state, reason), failed)) = observed {
            s.readiness = state;
            s.readiness_reason = reason;
            s.failed_network_id = failed;
        } else {
            // No usable record: the supplicant's own state decides (an empty
            // readiness). "Unavailable" showed a working, associated player
            // as not connected while the platform record was late.
            s.readiness = String::new();
            s.readiness_reason = Some("platform_readiness_stale".into());
        }
    }
    s
}

const RECORD_S: f64 = 15.0;
const ONLINE_RECORD_S: f64 = 45.0;

fn network_readiness(
    record: &Published,
    boot: &str,
    now: f64,
    live: &Status,
) -> Option<(String, Option<String>)> {
    let age = record.age(now)?;
    if record.schema != Some(1)
        || !record.is_this_boot(Some(boot))
        || !(0.0..ONLINE_RECORD_S).contains(&age)
    {
        return None;
    }
    let state = record.state.as_deref()?;
    // With the screen dark the platform writes every 10 s and one pass can
    // take several seconds more (Beta 4 measured 12 s ages). An Online claim
    // is checked against the live association and address below, so it may
    // be older; any other state must be recent.
    if state != "Online" && age >= RECORD_S {
        return None;
    }
    if ![
        "Off",
        "Starting",
        "Scanning",
        "Associating",
        "Authenticated",
        "AcquiringIP",
        "Online",
        "Failed",
    ]
    .contains(&state)
    {
        return None;
    }
    if state == "Online"
        && (live.state != "COMPLETED"
            || live.association_id != record.association_id
            || live.association_id.is_none()
            || live.ip.is_empty()
            || !record.has_address(&live.ip))
    {
        return None;
    }
    Some((state.to_owned(), record.reason.clone()))
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
// Control socket failures are missing observations, not radio link events.
// Keep the last known association for counters only; the UI still receives the
// current ERROR/STARTING state and must not present stale connectivity as live.
fn association_transition(previous: &mut Option<bool>, state: &str) -> (bool, bool) {
    let connected = match state {
        "COMPLETED" => true,
        "OFF" | "DISCONNECTED" | "INACTIVE" | "INTERFACE_DISABLED" => false,
        _ => return (false, false),
    };
    let connects = connected && *previous != Some(true);
    let disconnects = !connected && *previous == Some(true);
    *previous = Some(connected);
    (connects, disconnects)
}

fn run(log: Observer, rx: Receiver<Command>, et: SyncSender<Status>, paths: Paths) {
    let mut current = Status::default();
    let mut scan: Option<Scan> = None;
    let mut last = String::new();
    let mut last_association = None;
    let mut operation_error = None;
    let mut refresh = Instant::now() - Duration::from_secs(4);
    let mut inventory_refresh = refresh;
    let mut publish = true;
    let mut join: Option<PendingJoin> = None;
    let mut attempts = 0u64;
    loop {
        let wait = if scan.is_some() || join.is_some() || publish {
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
        // Any other owner command ends a join still being checked (its block
        // is removed again unless it connected).
        if explicit && !matches!(command, Command::Scan) {
            if let Some(pending) = join.take() {
                let _ = pending.finish(&paths, JoinOutcome::Failed);
                current.join = None;
            }
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
                attempts += 1;
                PendingJoin::start(&paths, attempts, &ssid, Some(&password), None).map(|pending| {
                    current.join = Some(Join {
                        attempt: pending.attempt,
                        ssid: ssid.clone(),
                        outcome: JoinOutcome::Checking,
                    });
                    join = Some(pending);
                })
            }
            Command::Disconnect => {
                Control::at(&paths).and_then(|c| c.request("DISCONNECT").map(|_| ()))
            }
            Command::Saved(network) => {
                let ssid = current
                    .saved
                    .iter()
                    .find(|n| n.saved_id == Some(network))
                    .map(|n| n.ssid.clone())
                    .unwrap_or_default();
                attempts += 1;
                PendingJoin::start(&paths, attempts, &ssid, None, Some(network)).map(|pending| {
                    current.join = Some(Join {
                        attempt: pending.attempt,
                        ssid,
                        outcome: JoinOutcome::Checking,
                    });
                    join = Some(pending);
                })
            }
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
            let inventory_due = explicit || inventory_refresh.elapsed() >= Duration::from_secs(30);
            let mut s =
                status_with_inventory(&paths, if inventory_due { None } else { Some(&current) });
            if inventory_due {
                inventory_refresh = Instant::now();
            }
            s.scan = current.scan.clone();
            s.join = current.join.clone();
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
        if let Some(pending) = join.as_mut() {
            if let Some(outcome) = pending.step(&paths, Instant::now()) {
                let finished = pending.finish(&paths, outcome);
                log.emit(
                    if outcome == JoinOutcome::Joined {
                        Level::Info
                    } else {
                        Level::Warn
                    },
                    "wifi",
                    "join_result",
                    "Wi-Fi join checked",
                    Some(id),
                    json!({"outcome":format!("{outcome:?}"),"new_network":pending.added,
                        "cleanup_error":finished.err()}),
                );
                current.join = Some(Join {
                    attempt: pending.attempt,
                    ssid: pending.ssid.clone(),
                    outcome,
                });
                join = None;
                // The saved list changed: read it again at once.
                inventory_refresh = Instant::now() - Duration::from_secs(60);
                refresh = Instant::now() - Duration::from_secs(4);
                publish = true;
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
            let (connected, disconnected) =
                association_transition(&mut last_association, &current.state);
            if connected {
                log.add("wifi_connects", 1.);
            } else if disconnected {
                log.add("wifi_disconnects", 1.);
            }
            last = current.state.clone();
        }
        log.health_set(
            "wifi",
            if !current.available {
                HealthState::Unavailable
            } else if current.error.is_some()
                || (current.state == "COMPLETED" && current.readiness != "Online")
            {
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
        if s.readiness == "Online" {
            return Ok(
                json!({"passed":true,"state":s.state,"readiness":s.readiness,"dhcp":true,"dns":true}),
            );
        }
        if Instant::now() > until {
            return Err("saved network usable IP/route/DNS deadline exceeded".into());
        }
        thread::sleep(Duration::from_millis(250));
    }
}
/// Owner diagnostic: the Wi-Fi page's join on saved network `id`, or (wrong)
/// a new entry with its name and a wrong password. Nothing is left behind by
/// the wrong one; the saved network's own entry is never changed.
pub fn join_test(id: u32, wrong: bool) -> Result<serde_json::Value, String> {
    let paths = Paths::default();
    let c = Control::at(&paths)?;
    let count = |c: &Control| -> Result<usize, String> {
        Ok(c.request("LIST_NETWORKS")?.lines().skip(1).count())
    };
    let before = count(&c)?;
    let raw = c.request(&format!("GET_NETWORK {id} ssid"))?;
    let ssid = match raw.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(name) => name.to_owned(),
        None => {
            let bytes = (0..raw.len() / 2)
                .map(|i| u8::from_str_radix(raw.get(i * 2..i * 2 + 2).unwrap_or(""), 16))
                .collect::<Result<Vec<u8>, _>>()
                .map_err(|_| "unreadable network name")?;
            String::from_utf8(bytes).map_err(|_| "network name is not text")?
        }
    };
    let mut join = if wrong {
        PendingJoin::start(&paths, 0, &ssid, Some("y2-wrong-password-test"), None)?
    } else {
        PendingJoin::start(&paths, 0, &ssid, None, Some(id))?
    };
    let started = Instant::now();
    let outcome = loop {
        if let Some(outcome) = join.step(&paths, Instant::now()) {
            break outcome;
        }
        thread::sleep(Duration::from_millis(200));
    };
    join.finish(&paths, outcome)?;
    Ok(json!({
        "passed": outcome == if wrong { JoinOutcome::WrongPassword } else { JoinOutcome::Joined },
        "outcome": format!("{outcome:?}"),
        "seconds": started.elapsed().as_secs_f32(),
        "networks_before": before,
        "networks_after": count(&Control::at(&paths)?)?,
    }))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_failures_become_user_problems() {
        use reborn_core::platform::WifiProblem;
        let mut s = Status {
            readiness: "Failed".into(),
            readiness_reason: Some("wrong_key".into()),
            ..Default::default()
        };
        assert_eq!(problem(&s), Some(WifiProblem::WrongPassword));
        s.readiness_reason = Some("dhcp_timeout".into());
        assert_eq!(problem(&s), Some(WifiProblem::NoAddress));
        s.readiness = "Authenticated".into();
        s.readiness_reason = Some("dns_unavailable".into());
        assert_eq!(problem(&s), Some(WifiProblem::NoInternetNames));
        s.readiness = "Online".into();
        s.readiness_reason = None;
        assert_eq!(problem(&s), None);
    }
    #[test]
    fn only_a_password_verdict_says_wrong_password() {
        // F52 (aa96e00a): `not_authenticated` is the platform's plain "not
        // connected"; any reason containing `auth` showed "wrong password".
        use reborn_core::platform::WifiProblem;
        let failed = |reason: &str| {
            problem(&Status {
                readiness: "Failed".into(),
                readiness_reason: Some(reason.into()),
                ..Default::default()
            })
        };
        for reason in ["wrong_credentials", "wrong_key"] {
            assert_eq!(failed(reason), Some(WifiProblem::WrongPassword), "{reason}");
        }
        for reason in [
            "not_authenticated",
            "authentication_timeout",
            "authentication_failed",
            "authentication_rejected",
            "supplicant_unavailable",
        ] {
            assert_eq!(failed(reason), Some(WifiProblem::Other), "{reason}");
        }
        assert_eq!(failed("ap_unavailable"), Some(WifiProblem::NetworkNotFound));
        assert_eq!(failed("dhcp_nak"), Some(WifiProblem::NoAddress));
        // Our own check of a typed password still names it.
        let typed = Status {
            error: Some("WPA2 requires SSID 1..32 bytes and password 8..63 bytes".into()),
            ..Default::default()
        };
        assert_eq!(problem(&typed), Some(WifiProblem::WrongPassword));
    }
    #[test]
    fn missing_control_samples_and_scans_do_not_invent_link_cycles() {
        let mut previous = None;
        assert_eq!(
            association_transition(&mut previous, "STARTING"),
            (false, false)
        );
        assert_eq!(
            association_transition(&mut previous, "COMPLETED"),
            (true, false)
        );
        for state in [
            "ERROR",
            "STARTING",
            "SCANNING",
            "GROUP_HANDSHAKE",
            "unknown",
        ] {
            assert_eq!(association_transition(&mut previous, state), (false, false));
            assert_eq!(
                association_transition(&mut previous, "COMPLETED"),
                (false, false)
            );
        }
        assert_eq!(
            association_transition(&mut previous, "DISCONNECTED"),
            (false, true)
        );
        assert_eq!(
            association_transition(&mut previous, "ERROR"),
            (false, false)
        );
        assert_eq!(
            association_transition(&mut previous, "ASSOCIATING"),
            (false, false)
        );
        assert_eq!(
            association_transition(&mut previous, "COMPLETED"),
            (true, false)
        );
        assert_eq!(association_transition(&mut previous, "OFF"), (false, true));
        assert_eq!(association_transition(&mut previous, "OFF"), (false, false));
    }

    fn read(value: &serde_json::Value) -> Published {
        Published::deserialize(value).unwrap()
    }

    #[test]
    fn platform_readiness_rejects_old_boot_and_stale_online_claims() {
        let value = json!({"schema":1,"boot_id":"one","monotonic_s":100.0,"state":"Online","reason":null,
                   "association_id":"peer-one","ip_addresses":["192.0.2.2"]});
        let live = Status {
            state: "COMPLETED".into(),
            association_id: Some("peer-one".into()),
            ip: "192.0.2.2".into(),
            ..Default::default()
        };
        assert_eq!(
            network_readiness(&read(&value), "one", 101.0, &live)
                .unwrap()
                .0,
            "Online"
        );
        assert!(network_readiness(&read(&value), "two", 101.0, &live).is_none());
        // A late Online record still matches the live association.
        assert!(network_readiness(&read(&value), "one", 116.0, &live).is_some());
        assert!(network_readiness(&read(&value), "one", 146.0, &live).is_none());
        assert!(network_readiness(&read(&value), "one", 99.0, &live).is_none());
        let mut acquiring = value.clone();
        acquiring["state"] = json!("AcquiringIP");
        assert!(network_readiness(&read(&acquiring), "one", 114.0, &live).is_some());
        assert!(network_readiness(&read(&acquiring), "one", 116.0, &live).is_none());
    }
    #[test]
    fn online_readiness_must_match_current_association_and_address() {
        let value = json!({"schema":1,"boot_id":"one","monotonic_s":100.0,"state":"Online",
                           "association_id":"peer-one","ip_addresses":["192.0.2.2"]});
        let mut live = Status {
            state: "COMPLETED".into(),
            association_id: Some("peer-two".into()),
            ip: "192.0.2.2".into(),
            ..Default::default()
        };
        assert!(network_readiness(&read(&value), "one", 101.0, &live).is_none());
        live.association_id = Some("peer-one".into());
        assert!(network_readiness(&read(&value), "one", 101.0, &live).is_some());
        live.ip = "192.0.2.3".into();
        assert!(network_readiness(&read(&value), "one", 101.0, &live).is_none());
        live.ip = "192.0.2.2".into();
        live.state = "ERROR".into();
        assert!(network_readiness(&read(&value), "one", 101.0, &live).is_none());
    }
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
        requests: Arc<Mutex<Vec<String>>>,
        /// LIST_NETWORKS rows after the header (`id\tssid\tany\t`).
        saved: Arc<Mutex<String>>,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }
    impl Supplicant {
        fn new(results: String) -> Self {
            Self::with_status_delay(results, Duration::ZERO)
        }
        fn with_status_delay(results: String, status_delay: Duration) -> Self {
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
            let requests = Arc::new(Mutex::new(Vec::new()));
            let observed_requests = requests.clone();
            let saved = Arc::new(Mutex::new(String::new()));
            let listed = saved.clone();
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
                        observed_requests.lock().unwrap().push(command.to_owned());
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
                            "STATUS" => {
                                thread::sleep(status_delay);
                                "wpa_state=DISCONNECTED\n"
                            }
                            "SCAN_RESULTS" => &results,
                            "LIST_NETWORKS" => &format!(
                                "network id / ssid / bssid / flags\n{}",
                                listed.lock().unwrap()
                            ),
                            "ADD_NETWORK" => "3\n",
                            c if c.starts_with("GET_NETWORK ") && c.ends_with(" ssid") => {
                                let id = c.split(' ').nth(1).unwrap_or("");
                                &listed
                                    .lock()
                                    .unwrap()
                                    .lines()
                                    .find(|l| l.split('\t').next() == Some(id))
                                    .and_then(|l| l.split('\t').nth(1))
                                    .map_or("FAIL\n".to_owned(), |name| format!("\"{name}\"\n"))
                            }
                            c if [
                                "SET_NETWORK ",
                                "SELECT_NETWORK ",
                                "REMOVE_NETWORK ",
                                "ENABLE_NETWORK ",
                            ]
                            .iter()
                            .any(|p| c.starts_with(p))
                                || c == "SAVE_CONFIG" =>
                            {
                                "OK\n"
                            }
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
                requests,
                saved,
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
    fn outcome(join: &mut PendingJoin, paths: &Paths) -> JoinOutcome {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(outcome) = join.step(paths, Instant::now()) {
                return outcome;
            }
            assert!(Instant::now() < until, "no join outcome");
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn a_join_is_saved_only_when_it_connects() {
        // Y2Reborn #20: one wrong letter, 15 tries; each try was saved at once,
        // the list said "Saved" and nothing ever said the password was wrong.
        let fake = Supplicant::new(HEADER.into());
        fake.paths.radio(true, false).unwrap();
        *fake.saved.lock().unwrap() = "0\tHome\tany\t\n".into();
        let sent = |fake: &Supplicant| fake.requests.lock().unwrap().clone();
        let mut join =
            PendingJoin::start(&fake.paths, 1, "Home", Some("wrongpassword"), None).unwrap();
        assert_eq!((join.id, join.added), (3, true));
        let order = sent(&fake);
        let at = |c: &str| order.iter().position(|r| r.starts_with(c)).unwrap();
        assert!(
            at("ATTACH") < at("SELECT_NETWORK 3") && at("ADD_NETWORK") < at("SELECT_NETWORK 3")
        );
        // Another network's failure is not ours.
        fake.event("<3>CTRL-EVENT-SSID-TEMP-DISABLED id=13 ssid=\"Other\" auth_failures=1 reason=WRONG_KEY");
        fake.event("<3>CTRL-EVENT-SSID-TEMP-DISABLED id=3 ssid=\"Home\" auth_failures=1 duration=10 reason=WRONG_KEY");
        assert_eq!(outcome(&mut join, &fake.paths), JoinOutcome::WrongPassword);
        fake.requests.lock().unwrap().clear();
        join.finish(&fake.paths, JoinOutcome::WrongPassword)
            .unwrap();
        assert_eq!(sent(&fake), ["REMOVE_NETWORK 3", "ENABLE_NETWORK all"]);
        // The right password: kept, the older entry of the same name goes, and
        // the other saved networks are enabled again before saving.
        *fake.saved.lock().unwrap() =
            "0\tHome\tany\t\n1\tCafe\tany\t\n3\tHome\tany\t[CURRENT]\n".into();
        let mut join =
            PendingJoin::start(&fake.paths, 2, "Home", Some("rightpassword"), None).unwrap();
        fake.event(
            "<3>CTRL-EVENT-CONNECTED - Connection to aa:bb:cc:dd:ee:ff completed [id=3 id_str=]",
        );
        assert_eq!(outcome(&mut join, &fake.paths), JoinOutcome::Joined);
        fake.requests.lock().unwrap().clear();
        join.finish(&fake.paths, JoinOutcome::Joined).unwrap();
        let done = sent(&fake);
        assert!(done.contains(&"REMOVE_NETWORK 0".to_owned()));
        assert!(!done
            .iter()
            .any(|r| r == "REMOVE_NETWORK 1" || r == "REMOVE_NETWORK 3"));
        let at = |c: &str| done.iter().position(|r| r == c).unwrap();
        assert!(at("ENABLE_NETWORK all") < at("SAVE_CONFIG"));
        // Out of range: three "not found" in a row.
        let mut join =
            PendingJoin::start(&fake.paths, 3, "Home", Some("rightpassword"), None).unwrap();
        for _ in 0..3 {
            fake.event("<3>CTRL-EVENT-NETWORK-NOT-FOUND");
        }
        assert_eq!(outcome(&mut join, &fake.paths), JoinOutcome::NotFound);
        // F52: a handshake cut short on this side (a sleep's radio off, another
        // join) is not a password verdict; the supplicant's timeout is.
        let mut join =
            PendingJoin::start(&fake.paths, 5, "Home", Some("rightpassword"), None).unwrap();
        fake.event(
            "<3>CTRL-EVENT-DISCONNECTED bssid=aa:bb:cc:dd:ee:ff reason=3 locally_generated=1",
        );
        fake.event(
            "<3>CTRL-EVENT-SSID-TEMP-DISABLED id=3 ssid=\"Home\" auth_failures=1 reason=WRONG_KEY",
        );
        fake.event(
            "<3>CTRL-EVENT-CONNECTED - Connection to aa:bb:cc:dd:ee:ff completed [id=3 id_str=]",
        );
        assert_eq!(outcome(&mut join, &fake.paths), JoinOutcome::Joined);
        let mut join =
            PendingJoin::start(&fake.paths, 6, "Home", Some("wrongpassword"), None).unwrap();
        fake.event("<3>Authentication with aa:bb:cc:dd:ee:ff timed out.");
        fake.event(
            "<3>CTRL-EVENT-DISCONNECTED bssid=aa:bb:cc:dd:ee:ff reason=3 locally_generated=1",
        );
        fake.event(
            "<3>CTRL-EVENT-SSID-TEMP-DISABLED id=3 ssid=\"Home\" auth_failures=1 reason=WRONG_KEY",
        );
        assert_eq!(outcome(&mut join, &fake.paths), JoinOutcome::WrongPassword);
        // A saved network is selected, never removed when it fails.
        let mut join = PendingJoin::start(&fake.paths, 4, "Home", None, Some(0)).unwrap();
        assert!(!join.added);
        fake.event("<3>CTRL-EVENT-SSID-TEMP-DISABLED id=0 ssid=\"Home\" reason=WRONG_KEY");
        assert_eq!(outcome(&mut join, &fake.paths), JoinOutcome::WrongPassword);
        fake.requests.lock().unwrap().clear();
        join.finish(&fake.paths, JoinOutcome::WrongPassword)
            .unwrap();
        assert_eq!(sent(&fake), ["ENABLE_NETWORK all"]);
        assert_eq!(
            event_network("CTRL-EVENT-CONNECTED - Connection to x completed [id=17 id_str=]"),
            Some(17)
        );
        assert_eq!(ssid_hex("\"Ab\""), "4162");
    }
    #[test]
    fn a_failed_network_that_is_no_longer_saved_is_not_a_problem() {
        let mut s = Status {
            readiness: "Failed".into(),
            readiness_reason: Some("wrong_credentials".into()),
            failed_network_id: Some(3),
            saved: vec![Network {
                ssid: "Home".into(),
                saved_id: Some(0),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(problem(&s), None);
        s.failed_network_id = Some(0);
        assert_eq!(
            problem(&s),
            Some(reborn_core::platform::WifiProblem::WrongPassword)
        );
        s.join = Some(Join {
            attempt: 1,
            ssid: "Home".into(),
            outcome: JoinOutcome::Checking,
        });
        assert_eq!(problem(&s), None);
    }
    #[test]
    fn status_survives_measured_vendor_statistics_delay() {
        let fake = Supplicant::with_status_delay(HEADER.into(), Duration::from_millis(2100));
        fake.paths.radio(true, false).unwrap();
        let status = status_at(&fake.paths);
        assert_eq!(status.state, "DISCONNECTED");
        assert!(status.error.is_none());
    }
    #[test]
    fn status_refresh_reuses_inventory_and_keeps_live_association() {
        let fake = Supplicant::new(format!("{HEADER}a\t2412\t-50\t[ESS]\tHome\n"));
        fake.paths.radio(true, false).unwrap();
        let first = status_at(&fake.paths);
        assert_eq!(first.networks.len(), 1);
        fake.requests.lock().unwrap().clear();
        let next = status_with_inventory(&fake.paths, Some(&first));
        assert_eq!(next.state, "DISCONNECTED");
        assert_eq!(next.networks[0].ssid, "Home");
        assert_eq!(*fake.requests.lock().unwrap(), ["PING", "STATUS"]);
        // A known association change invalidates the cached discovery snapshot.
        let stale = Status {
            state: "COMPLETED".into(),
            ..first
        };
        fake.requests.lock().unwrap().clear();
        status_with_inventory(&fake.paths, Some(&stale));
        assert!(fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "SCAN_RESULTS"));
        assert!(fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "LIST_NETWORKS"));
        // Explicit operations and the bounded inventory tick request a fresh list.
        fake.requests.lock().unwrap().clear();
        status_at(&fake.paths);
        assert!(fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == "LIST_NETWORKS"));
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
        assert!(quote("tab\there").is_err());
    }
    #[test]
    fn passwords_and_names_reach_the_supplicant_unchanged() {
        // wpa_config_parse_string: after the first quote, everything up to the
        // last quote, no escapes.
        fn supplicant_reads(value: &str) -> &str {
            let inner = value.strip_prefix('"').unwrap();
            &inner[..inner.rfind('"').unwrap()]
        }
        for password in [
            "plain-password",
            "a\"b\\c",
            "ends with quote\"",
            "\"\"\"\"\"\"\"\"",
            "back\\slash",
        ] {
            assert_eq!(supplicant_reads(&quote(password).unwrap()), password);
        }
        assert_eq!(hex_ssid("Café \"5G\""), "436166c3a92022354722");
        assert_eq!(hex_ssid("Zyxel_7941"), "5a7978656c5f37393431");
    }
}

#[cfg(test)]
mod ui_security_tests {
    use super::Network;
    #[test]
    fn only_supported_security_can_be_connected_from_the_wheel_ui() {
        for (security, expected) in [
            ("[ESS]", Some(false)),
            ("[WPA2-PSK-CCMP][ESS]", Some(true)),
            ("[WPA2-EAP-CCMP][ESS]", None),
            ("[WEP][ESS]", None),
            ("[WPA3-SAE-CCMP][ESS]", None),
        ] {
            let network = Network {
                security: security.into(),
                ..Default::default()
            };
            assert_eq!(network.password_required(), expected, "{security}");
        }
    }
}
