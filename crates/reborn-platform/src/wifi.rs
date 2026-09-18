#![forbid(unsafe_code)]
use reborn_core::atomic_write;
use reborn_observability::{HealthState, Level, Observer};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs, io,
    os::unix::net::UnixDatagram,
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
}
static SEQ: AtomicU64 = AtomicU64::new(1);
impl Control {
    fn open() -> Result<Self, String> {
        let remote = fs::read_dir("/run/wpa_supplicant")
            .map_err(|_| "wpa control unavailable")?
            .filter_map(Result::ok)
            .find(|e| e.file_name() != "global")
            .ok_or("Wi-Fi interface is off or unavailable")?
            .path();
        let path = PathBuf::from(format!(
            "/run/reborn/wpa-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let socket = UnixDatagram::bind(&path).map_err(|e| e.to_string())?;
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|e| e.to_string())?;
        socket.connect(remote).map_err(|e| e.to_string())?;
        Ok(Self { socket, path })
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
        let s = String::from_utf8_lossy(&buf[..n]).trim().to_owned();
        if s.starts_with("FAIL") {
            return Err("wpa_supplicant rejected operation".into());
        }
        Ok(s)
    }
}
impl Drop for Control {
    fn drop(&mut self) {
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
    fs::read_to_string("/data/network/enabled").is_ok_and(|s| s.trim() == "1")
}
pub fn radio(on: bool, persist: bool) -> Result<(), String> {
    let mut found = false;
    for e in fs::read_dir("/sys/class/rfkill")
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
        return Err("Wi-Fi rfkill unavailable".into());
    }
    if persist {
        atomic_write(
            Path::new("/data/network/enabled"),
            if on { b"1\n" } else { b"0\n" },
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
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
fn status() -> Status {
    let mut s = Status {
        enabled: enabled(),
        ..Default::default()
    };
    match Control::open() {
        Err(e) => {
            s.state = "unavailable".into();
            s.error = Some(e)
        }
        Ok(c) => {
            s.available = true;
            if let Ok(text) = c.request("STATUS") {
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
                if let Ok(text) = c.request("SCAN_RESULTS") {
                    s.networks = text
                        .lines()
                        .skip(1)
                        .take(256)
                        .filter_map(|line| {
                            let p = line.splitn(5, '\t').collect::<Vec<_>>();
                            if p.len() < 5 {
                                return None;
                            }
                            Some(Network {
                                ssid: p[4].into(),
                                signal: p[2].parse().unwrap_or(-100),
                                security: p[3].into(),
                                saved_id: None,
                            })
                        })
                        .collect();
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
    }
    s
}
impl Wifi {
    pub fn spawn(log: Observer) -> io::Result<Self> {
        let (tx, rx) = sync_channel(8);
        let (et, er) = sync_channel(2);
        thread::Builder::new().name("wifi".into()).spawn(move || {
            let mut last = String::new();
            loop {
                let command = match rx.recv_timeout(Duration::from_secs(3)) {
                    Ok(c) => c,
                    Err(RecvTimeoutError::Timeout) => Command::Refresh,
                    Err(_) => break,
                };
                let id = log.correlation();
                let r: Result<(), String> = match command {
                    Command::Stop => break,
                    Command::Refresh => Ok(()),
                    Command::Power(on) => {
                        log.emit(
                            Level::Info,
                            "wifi",
                            "radio_power",
                            "Wi-Fi radio requested",
                            Some(id),
                            json!({"enabled":on}),
                        );
                        radio(on, true)
                    }
                    Command::Scan => Control::open().and_then(|c| c.request("SCAN").map(|_| ())),
                    Command::Connect { ssid, password } => {
                        log.emit(
                            Level::Info,
                            "wifi",
                            "connect",
                            "Wi-Fi connection requested",
                            Some(id),
                            json!({"ssid":ssid}),
                        );
                        Control::open().and_then(|c| connect(&c, &ssid, &password))
                    }
                    Command::Saved(id) => Control::open()
                        .and_then(|c| c.request(&format!("SELECT_NETWORK {id}")).map(|_| ())),
                    Command::Forget(id) => Control::open().and_then(|c| {
                        c.request(&format!("REMOVE_NETWORK {id}"))?;
                        c.request("SAVE_CONFIG").map(|_| ())
                    }),
                };
                let mut s = status();
                if let Err(e) = r {
                    log.add("wifi_errors", 1.);
                    log.emit(
                        Level::Warn,
                        "wifi",
                        "operation_failed",
                        &e,
                        Some(id),
                        json!({"interface":"wpa_supplicant control","recovery_attempted":false}),
                    );
                    s.error = Some(e)
                }
                if s.state != last {
                    log.emit(
                        Level::Info,
                        "wifi",
                        "state_changed",
                        "Wi-Fi state changed",
                        Some(id),
                        json!({"state":s.state,"ip":s.ip}),
                    );
                    if s.state == "COMPLETED" {
                        log.add("wifi_connects", 1.)
                    } else if last == "COMPLETED" {
                        log.add("wifi_disconnects", 1.)
                    }
                    last = s.state.clone();
                }
                log.health_set(
                    "wifi",
                    if s.available {
                        HealthState::Ok
                    } else {
                        HealthState::Unavailable
                    },
                    false,
                    &s.state,
                );
                let _ = et.try_send(s);
            }
        })?;
        Ok(Self {
            commands: tx,
            events: er,
        })
    }
}
pub fn scan_test() -> Result<serde_json::Value, String> {
    let before = enabled();
    let result = (|| {
        if !before {
            radio(true, false)?
        }
        let start = Instant::now();
        let c = loop {
            match Control::open() {
                Ok(c) => break c,
                Err(_) if start.elapsed() < Duration::from_secs(15) => {
                    thread::sleep(Duration::from_millis(200))
                }
                Err(e) => return Err(e),
            }
        };
        c.request("SCAN")?;
        thread::sleep(Duration::from_secs(3));
        let s = status();
        Ok(json!({"passed":true,"count":s.networks.len(),"available":s.available}))
    })();
    if !before {
        let _ = radio(false, false);
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
    #[test]
    fn password_encoding_cannot_inject_commands() {
        assert!(quote("x\nREMOVE_NETWORK all").is_err());
        assert_eq!(quote("a\"b\\c").unwrap(), "\"a\\\"b\\\\c\"");
    }
}
