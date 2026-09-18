#![forbid(unsafe_code)]
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{sync_channel, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const SUBSYSTEMS: &[&str] = &[
    "core",
    "startup",
    "ui",
    "graphics",
    "input",
    "media",
    "ffmpeg",
    "playback",
    "audio",
    "alsa",
    "bluetooth",
    "bluealsa",
    "wifi",
    "library",
    "scanner",
    "database",
    "storage",
    "artwork",
    "power",
    "suspend",
    "metrics",
    "control",
    "diagnostics",
];
pub const METRICS: &[&str] = &[
    "reborn_uptime_seconds",
    "playback_tracks_started",
    "playback_tracks_completed",
    "playback_errors",
    "ffmpeg_packets_decoded",
    "ffmpeg_frames_decoded",
    "ffmpeg_decode_errors",
    "audio_buffer_frames",
    "audio_buffer_ms",
    "audio_xruns",
    "audio_recoveries",
    "decoder_stalls",
    "library_tracks",
    "library_scan_files_per_sec",
    "library_scan_errors",
    "database_query_latency_ms",
    "graphics_frames",
    "graphics_frame_time_ms",
    "graphics_missed_frames",
    "graphics_context_losses",
    "wifi_connects",
    "wifi_disconnects",
    "wifi_errors",
    "bluetooth_connects",
    "bluetooth_disconnects",
    "bluetooth_errors",
    "sd_insertions",
    "sd_removals",
    "logs_dropped",
    "log_write_errors",
];
pub const RING_LIMIT: usize = 512;
pub const EVENT_LIMIT: usize = 4096;
pub const LOG_BYTES: u64 = 1024 * 1024;
pub const LOG_FILES: usize = 3;
pub const BUNDLE_BYTES: usize = 2 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}
impl Level {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Ok,
    Degraded,
    Failed,
    Unavailable,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct HealthEntry {
    pub state: HealthState,
    pub detail: String,
    pub required: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub timestamp_wall: u64,
    pub timestamp_monotonic: u64,
    pub boot_id: String,
    pub reborn_session_id: String,
    pub sequence: u64,
    pub level: Level,
    pub subsystem: String,
    pub event: String,
    pub message: String,
    pub correlation_id: Option<u64>,
    pub fields: Value,
}
struct Heartbeat {
    last: Instant,
    deadline: Duration,
}
struct State {
    context: Value,
    ring: VecDeque<LogEvent>,
    levels: BTreeMap<String, (Level, Option<Instant>)>,
    metrics: BTreeMap<String, f64>,
    health: BTreeMap<String, HealthEntry>,
    heartbeats: BTreeMap<String, Heartbeat>,
    storms: BTreeMap<String, (Instant, u32)>,
    last_bundle: Option<Instant>,
}
struct Inner {
    bundle_lock: Mutex<()>,
    start: Instant,
    boot: String,
    session: String,
    seq: AtomicU64,
    operations: AtomicU64,
    state: Mutex<State>,
    writer: SyncSender<Vec<u8>>,
    dir: PathBuf,
}
#[derive(Clone)]
pub struct Observer(Arc<Inner>);
fn secret(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    [
        "password",
        "passphrase",
        "psk",
        "linkkey",
        "link_key",
        "ltk",
        "irk",
        "bond",
        "secret",
        "private_key",
        "calibration",
        "nvram",
        "protect_f",
        "protect_s",
        "pin_code",
        "passkey",
    ]
    .iter()
    .any(|s| k.contains(s))
}
fn safe_string(s: &str) -> String {
    if secret(s) && (s.contains('=') || s.contains(':') || s.contains("BEGIN ") || s.contains('/'))
    {
        return "[redacted]".into();
    }
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(512)
        .collect()
}
pub fn sanitize(v: &Value, private_media: bool) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| {
                    let blocked = secret(k)
                        || (private_media
                            && [
                                "path",
                                "root",
                                "pending",
                                "music_directory",
                                "filename",
                                "title",
                                "artist",
                                "album",
                                "queue",
                                "ssid",
                                "address",
                            ]
                            .contains(&k.as_str()));
                    (
                        k.clone(),
                        if blocked {
                            json!("[redacted]")
                        } else {
                            sanitize(v, private_media)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(
            a.iter()
                .take(512)
                .map(|v| sanitize(v, private_media))
                .collect(),
        ),
        Value::String(s) if private_media && (s.contains("/data/") || s.contains("/media/")) => {
            json!("[private path redacted]")
        }
        Value::String(s) => json!(safe_string(s)),
        v => v.clone(),
    }
}
impl Observer {
    pub fn new(dir: &Path) -> io::Result<Self> {
        let initial_error = fs::create_dir_all(dir).err();
        let (tx, rx) = sync_channel::<Vec<u8>>(256);
        let session = format!("{}-{}", wall_ms(), std::process::id());
        let o = Self(Arc::new(Inner {
            bundle_lock: Mutex::new(()),
            start: Instant::now(),
            boot: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .unwrap_or_default()
                .trim()
                .into(),
            session,
            seq: AtomicU64::new(0),
            operations: AtomicU64::new(1),
            state: Mutex::new(State {
                context: json!({"startup":true}),
                ring: VecDeque::new(),
                levels: BTreeMap::new(),
                metrics: METRICS.iter().map(|s| (s.to_string(), 0.)).collect(),
                health: BTreeMap::new(),
                heartbeats: BTreeMap::new(),
                storms: BTreeMap::new(),
                last_bundle: None,
            }),
            writer: tx,
            dir: dir.to_owned(),
        }));
        if initial_error.is_some() {
            o.health_set(
                "logging",
                HealthState::Degraded,
                false,
                "persistent log directory unavailable",
            );
        }
        let weak = Arc::downgrade(&o.0);
        let path = dir.to_owned();
        thread::Builder::new()
            .name("logger".into())
            .spawn(move || {
                let mut log = RotatingLog::new(path, LOG_BYTES, LOG_FILES);
                while let Ok(line) = rx.recv() {
                    let result = log.append(&line);
                    if let Some(inner) = weak.upgrade() {
                        if result.is_err() {
                            if let Ok(mut s) = inner.state.lock() {
                                *s.metrics.entry("log_write_errors".into()).or_default() += 1.;
                                s.health.insert(
                                    "logging".into(),
                                    HealthEntry {
                                        state: HealthState::Degraded,
                                        detail: "persistent log write failed".into(),
                                        required: false,
                                    },
                                );
                            }
                        }
                    } else {
                        break;
                    }
                }
            })?;
        Ok(o)
    }
    pub fn context(&self, context: Value) {
        if let Ok(mut state) = self.0.state.lock() {
            state.context = sanitize(&context, true);
        }
    }
    pub fn session(&self) -> &str {
        &self.0.session
    }
    pub fn uptime(&self) -> u64 {
        self.0.start.elapsed().as_secs()
    }
    pub fn enabled(&self, subsystem: &str, level: Level) -> bool {
        let Ok(state) = self.0.state.lock() else {
            return false;
        };
        let threshold = state
            .levels
            .get(subsystem)
            .filter(|(_, until)| until.is_none_or(|t| Instant::now() < t))
            .map(|v| v.0)
            .unwrap_or(Level::Info);
        level <= threshold
    }
    pub fn correlation(&self) -> u64 {
        self.0.operations.fetch_add(1, Ordering::Relaxed)
    }
    pub fn emit(
        &self,
        level: Level,
        subsystem: &str,
        event: &str,
        message: &str,
        correlation: Option<u64>,
        fields: Value,
    ) {
        if !SUBSYSTEMS.contains(&subsystem) {
            return;
        }
        let Ok(mut state) = self.0.state.lock() else {
            return;
        };
        let threshold = state
            .levels
            .get(subsystem)
            .filter(|(_, until)| until.is_none_or(|t| Instant::now() < t))
            .map(|v| v.0)
            .unwrap_or(Level::Info);
        if level > threshold {
            return;
        }
        let key = format!("{subsystem}:{event}:{}", safe_string(message));
        if !state.storms.contains_key(&key) && state.storms.len() >= 128 {
            state.storms.clear()
        }
        let storm = state.storms.entry(key).or_insert((Instant::now(), 0));
        if storm.0.elapsed() > Duration::from_secs(10) {
            *storm = (Instant::now(), 0)
        }
        storm.1 += 1;
        if storm.1 > 10 {
            *state.metrics.get_mut("logs_dropped").unwrap() += 1.;
            return;
        }
        let mut e = LogEvent {
            timestamp_wall: wall_ms(),
            timestamp_monotonic: self.0.start.elapsed().as_millis() as u64,
            boot_id: self.0.boot.clone(),
            reborn_session_id: self.0.session.clone(),
            sequence: self.0.seq.fetch_add(1, Ordering::Relaxed),
            level,
            subsystem: subsystem.into(),
            event: safe_string(event),
            message: safe_string(message),
            correlation_id: correlation,
            fields: sanitize(&fields, false),
        };
        let Ok(mut bytes) = serde_json::to_vec(&e) else {
            return;
        };
        if bytes.len() > EVENT_LIMIT {
            e.fields = json!({"truncated":true});
            bytes = serde_json::to_vec(&e).unwrap_or_default()
        }
        state.ring.push_back(e);
        while state.ring.len() > RING_LIMIT {
            state.ring.pop_front();
        }
        bytes.push(b'\n');
        if let Err(TrySendError::Full(_)) = self.0.writer.try_send(bytes) {
            *state.metrics.get_mut("logs_dropped").unwrap() += 1.
        }
    }
    pub fn level(&self, subsystem: Option<&str>, level: Option<Level>) -> Result<Value, String> {
        let mut s = self.0.state.lock().map_err(|_| "logger poisoned")?;
        match (subsystem, level) {
            (Some("reset"), _) => s.levels.clear(),
            (Some(name), Some(level)) => {
                if !SUBSYSTEMS.contains(&name) {
                    return Err("unknown subsystem".into());
                }
                s.levels.insert(
                    name.into(),
                    (
                        level,
                        if level == Level::Trace {
                            Some(Instant::now() + Duration::from_secs(300))
                        } else {
                            None
                        },
                    ),
                );
            }
            (None, None) => {}
            _ => return Err("specify subsystem and level".into()),
        }
        Ok(
            json!({"default":"INFO","overrides":s.levels.iter().map(|(k,v)|(k,v.0)).collect::<BTreeMap<_,_>>(),"trace_ttl_seconds":300}),
        )
    }
    pub fn add(&self, name: &str, v: f64) {
        if let Ok(mut s) = self.0.state.lock() {
            if let Some(n) = s.metrics.get_mut(name) {
                *n += v
            }
        }
    }
    pub fn gauge(&self, name: &str, v: f64) {
        if v.is_finite() {
            if let Ok(mut s) = self.0.state.lock() {
                if let Some(n) = s.metrics.get_mut(name) {
                    *n = v
                }
            }
        }
    }
    pub fn metrics(&self) -> Value {
        self.gauge("reborn_uptime_seconds", self.uptime() as f64);
        self.0
            .state
            .lock()
            .map(|s| json!(s.metrics))
            .unwrap_or(json!({}))
    }
    pub fn health_set(&self, sub: &str, state: HealthState, required: bool, detail: &str) {
        if let Ok(mut s) = self.0.state.lock() {
            s.health.insert(
                sub.into(),
                HealthEntry {
                    state,
                    detail: safe_string(detail),
                    required,
                },
            );
        }
    }
    pub fn heartbeat(&self, worker: &str, deadline_seconds: u64) {
        if let Ok(mut s) = self.0.state.lock() {
            s.heartbeats.insert(
                worker.into(),
                Heartbeat {
                    last: Instant::now(),
                    deadline: Duration::from_secs(deadline_seconds),
                },
            );
        }
    }
    pub fn health(&self) -> Value {
        let Ok(s) = self.0.state.lock() else {
            return json!({"overall":"failed"});
        };
        let mut health = s.health.clone();
        for (k, h) in &s.heartbeats {
            if h.last.elapsed() > h.deadline {
                health.insert(
                    format!("worker_{k}"),
                    HealthEntry {
                        state: HealthState::Failed,
                        detail: "heartbeat deadline exceeded".into(),
                        required: true,
                    },
                );
            }
        }
        let overall = if health
            .values()
            .any(|h| h.required && h.state == HealthState::Failed)
        {
            "failed"
        } else if health.values().any(|h| {
            h.state == HealthState::Degraded || (h.required && h.state == HealthState::Unavailable)
        }) {
            "degraded"
        } else {
            "ok"
        };
        json!({"overall":overall,"subsystems":health,"recent_faults":s.ring.iter().rev().filter(|e|e.level<=Level::Warn).take(12).collect::<Vec<_>>()})
    }
    pub fn events(
        &self,
        last: usize,
        subsystem: Option<&str>,
        level: Option<Level>,
        since_ms: Option<u64>,
    ) -> Vec<LogEvent> {
        let Ok(s) = self.0.state.lock() else {
            return vec![];
        };
        let oldest = (self.0.start.elapsed().as_millis() as u64)
            .saturating_sub(since_ms.unwrap_or(u64::MAX));
        let mut v = s
            .ring
            .iter()
            .rev()
            .filter(|e| {
                subsystem.is_none_or(|s| e.subsystem == s)
                    && level.is_none_or(|l| e.level <= l)
                    && e.timestamp_monotonic >= oldest
            })
            .take(last.min(RING_LIMIT))
            .cloned()
            .collect::<Vec<_>>();
        v.reverse();
        v
    }
    pub fn diagnostic(&self, root: &Path, snapshot: Value, automatic: bool) -> io::Result<PathBuf> {
        if automatic {
            let mut s = self
                .0
                .state
                .lock()
                .map_err(|_| io::Error::other("observer lock"))?;
            if s.last_bundle
                .is_some_and(|t| t.elapsed() < Duration::from_secs(300))
            {
                return Err(io::Error::other("diagnostic rate limit"));
            }
            s.last_bundle = Some(Instant::now());
        }
        let _bundle_guard = self
            .0
            .bundle_lock
            .try_lock()
            .map_err(|_| io::Error::other("diagnostic writer busy"))?;
        fs::create_dir_all(root)?;
        let mut data = json!({"schema":1,"session":self.session(),"snapshot":snapshot,"health":self.health(),"metrics":self.metrics(),"events":self.events(128,None,None,None),"system":{}});
        for (name, path) in [
            ("meminfo", "/proc/meminfo"),
            ("process", "/proc/self/status"),
            ("kernel", "/proc/version"),
            ("boot_id", "/proc/sys/kernel/random/boot_id"),
            ("alsa", "/proc/asound/cards"),
        ] {
            if let Ok(v) = fs::read_to_string(path) {
                data["system"][name] = json!(v.chars().take(16384).collect::<String>())
            }
        }
        let mut frequencies = Vec::new();
        for entry in fs::read_dir("/sys/devices/system/cpu/cpufreq")
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .take(8)
        {
            let mut item = json!({"policy":entry.file_name().to_string_lossy()});
            for name in [
                "scaling_cur_freq",
                "scaling_min_freq",
                "scaling_max_freq",
                "scaling_governor",
            ] {
                if let Ok(v) = fs::read_to_string(entry.path().join(name)) {
                    item[name] = json!(v.trim());
                }
            }
            frequencies.push(item);
        }
        data["system"]["cpu_frequency"] = json!(frequencies);
        let mut drm = Vec::new();
        for entry in fs::read_dir("/sys/class/drm")
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .take(32)
        {
            let mut item = json!({"node":entry.file_name().to_string_lossy()});
            for name in ["status", "enabled", "modes"] {
                if let Ok(v) = fs::read_to_string(entry.path().join(name)) {
                    item[name] = json!(v.chars().take(512).collect::<String>());
                }
            }
            if let Ok(driver) = fs::canonicalize(entry.path().join("device/driver")) {
                item["driver"] =
                    json!(driver.file_name().map(|s| s.to_string_lossy().into_owned()));
            }
            drm.push(item);
        }
        data["system"]["drm_devices"] = json!(drm);
        let bytes = serde_json::to_vec(&sanitize(&data, true))?;
        if bytes.len() > BUNDLE_BYTES {
            return Err(io::Error::other("diagnostic size bound exceeded"));
        }
        let mut old = fs::read_dir(root)?
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("reborn-diagnostic-")
                    && e.path().extension().is_some_and(|s| s == "gz")
            })
            .collect::<Vec<_>>();
        old.sort_by_key(|e| e.file_name());
        let remove = old.len().saturating_sub(3);
        for f in old.into_iter().take(remove) {
            fs::remove_file(f.path())?;
        }
        let path = root.join(format!(
            "reborn-diagnostic-{}-{}.tar.gz",
            wall_ms(),
            self.correlation()
        ));
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        let gzip = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut tar = tar::Builder::new(gzip);
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_mtime(0);
        header.set_cksum();
        tar.append_data(&mut header, "diagnostic.json", bytes.as_slice())?;
        tar.into_inner()?.finish()?.sync_all()?;
        Ok(path)
    }
    pub fn install_panic_hook(&self, version: &'static str) {
        let weak = Arc::downgrade(&self.0);
        std::panic::set_hook(Box::new(move |info| {
            if let Some(i) = weak.upgrade() {
                let recent = i
                    .state
                    .try_lock()
                    .map(|s| {
                        s.ring
                            .iter()
                            .rev()
                            .take(16)
                            .map(|e| e.sequence)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let message = info
                    .payload()
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("non-string panic");
                let context = i
                    .state
                    .try_lock()
                    .map(|s| s.context.clone())
                    .unwrap_or(json!({"unavailable":"panic while observer locked"}));
                let record = json!({"version":version,"thread":thread::current().name(),"location":info.location().map(|l|format!("{}:{}",l.file(),l.line())),"message":safe_string(message),"recent_event_ids":recent,"session":i.session,"state_summary":context});
                if let Ok(mut file) = File::create(i.dir.join("panic-last.json")) {
                    let _ = serde_json::to_writer(&mut file, &record);
                    let _ = file.sync_all();
                }
            }
            std::process::exit(101);
        }));
    }
}
pub fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub struct RotatingLog {
    dir: PathBuf,
    max: u64,
    count: usize,
}
impl RotatingLog {
    pub fn new(dir: PathBuf, max: u64, count: usize) -> Self {
        Self {
            dir,
            max,
            count: count.clamp(1, 8),
        }
    }
    fn path(&self, n: usize) -> PathBuf {
        self.dir.join(if n == 0 {
            "reborn-current.jsonl".into()
        } else {
            format!("reborn-previous-{n}.jsonl")
        })
    }
    pub fn append(&mut self, line: &[u8]) -> io::Result<()> {
        if line.len() as u64 > self.max {
            return Err(io::Error::other("oversized log record"));
        }
        if fs::metadata(self.path(0)).map(|m| m.len()).unwrap_or(0) + line.len() as u64 > self.max {
            for n in (1..self.count).rev() {
                let old = self.path(n - 1);
                if old.exists() {
                    fs::rename(old, self.path(n))?;
                }
            }
            if self.count == 1 {
                fs::remove_file(self.path(0))?;
            }
        }
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.path(0))?;
        f.write_all(line)?;
        f.flush()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn temp(n: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("reborn-{}-{}-{n}", std::process::id(), wall_ms()));
        fs::create_dir_all(&p).unwrap();
        p
    }
    #[test]
    fn redaction_recursive() {
        let s=sanitize(&json!({"password":"one","nested":{"PSK":"two","link_key":"three","calibration":"four"},"message":"psk=secret","ok":2}),false).to_string();
        for v in ["one", "two", "three", "four", "psk=secret"] {
            assert!(!s.contains(v));
        }
    }
    #[test]
    fn rotation_bounded() {
        let p = temp("rotation");
        let mut l = RotatingLog::new(p.clone(), 100, 3);
        for _ in 0..100 {
            l.append(&[b'a'; 50]).unwrap()
        }
        assert!(
            fs::read_dir(&p)
                .unwrap()
                .map(|e| e.unwrap().metadata().unwrap().len())
                .sum::<u64>()
                <= 300
        );
        fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn log_failure_no_panic() {
        let mut l = RotatingLog::new("/dev/null/no".into(), 10, 3);
        assert!(l.append(b"abc").is_err());
        let mut l = RotatingLog::new(temp("size"), 1, 1);
        assert!(l.append(b"ab").is_err())
    }
    #[test]
    fn storm_and_correlation() {
        let p = temp("storm");
        let o = Observer::new(&p).unwrap();
        let id = o.correlation();
        assert_ne!(id, o.correlation());
        for _ in 0..100 {
            o.emit(
                Level::Warn,
                "audio",
                "failure",
                "error",
                Some(id),
                json!({}),
            );
        }
        assert_eq!(o.events(100, None, None, None).len(), 10);
        assert_eq!(o.metrics()["logs_dropped"], 90.);
    }
    #[test]
    fn runtime_level_and_cardinality() {
        let o = Observer::new(&temp("levels")).unwrap();
        assert!(o.level(Some("track-123"), Some(Level::Trace)).is_err());
        o.add("per_track_metric", 1.);
        assert!(o.metrics().get("per_track_metric").is_none());
        o.emit(Level::Debug, "media", "a", "", None, json!({}));
        assert!(o.events(1, None, None, None).is_empty());
        o.level(Some("media"), Some(Level::Debug)).unwrap();
        o.emit(Level::Debug, "media", "a", "", None, json!({}));
        assert_eq!(o.events(1, None, None, None).len(), 1);
    }
    #[test]
    fn heartbeat_failed() {
        let o = Observer::new(&temp("heart")).unwrap();
        o.heartbeat("database", 0);
        assert_eq!(o.health()["overall"], "failed");
    }
    #[test]
    fn bundle_sanitized_and_bounded() {
        let p = temp("bundle");
        let o = Observer::new(&p).unwrap();
        let path=o.diagnostic(&p,json!({"password":"CANARY_SECRET","path":"/music/PRIVATE","calibration":"PRIVATE_BYTES"}),false).unwrap();
        let g = flate2::read::GzDecoder::new(File::open(path).unwrap());
        let mut t = tar::Archive::new(g);
        use std::io::Read;
        let mut s = String::new();
        t.entries()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert!(!s.contains("CANARY_SECRET"));
        assert!(!s.contains("/music/PRIVATE"));
        assert!(s.len() < BUNDLE_BYTES);
    }
}

#[cfg(test)]
mod failure_tests {
    use super::*;
    #[test]
    fn unavailable_directory_retains_ring() {
        let o = Observer::new(Path::new("/dev/null/not-a-directory")).unwrap();
        o.emit(Level::Error, "audio", "fault", "test", None, json!({}));
        assert_eq!(o.events(1, None, None, None).len(), 1);
        assert_eq!(o.health()["overall"], "degraded");
    }
    #[test]
    fn disk_full_is_an_error_not_a_panic() {
        let p = std::env::temp_dir().join(format!("reborn-diskfull-{}", std::process::id()));
        fs::create_dir_all(&p).unwrap();
        let f = p.join("reborn-current.jsonl");
        let _ = fs::remove_file(&f);
        std::os::unix::fs::symlink("/dev/full", &f).unwrap();
        assert!(RotatingLog::new(p.clone(), 4096, 3)
            .append(b"{}\n")
            .is_err());
        fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn redaction_covers_protected_material() {
        let input = json!({"wifi":{"password":"PASSWORD_CANARY","PSK":"PSK_CANARY"},"bluetooth":{"link_key":"KEY_CANARY","IRK":"IRK_CANARY"},"calibration":"CALIBRATION_CANARY","ssh_private_key":"SSH_CANARY","root":"/data/private-music","pending":{"display":"PAIRING_CANARY"}});
        let s = sanitize(&input, true).to_string();
        for c in [
            "PASSWORD_CANARY",
            "PSK_CANARY",
            "KEY_CANARY",
            "IRK_CANARY",
            "CALIBRATION_CANARY",
            "SSH_CANARY",
            "PAIRING_CANARY",
            "/data/private-music",
        ] {
            assert!(!s.contains(c), "{c}");
        }
    }
    #[test]
    fn metrics_ignore_unbounded_labels() {
        let o = Observer::new(Path::new("/dev/null/no")).unwrap();
        for i in 0..1000 {
            o.add(&format!("track-{i}"), 1.);
        }
        assert_eq!(o.metrics().as_object().unwrap().len(), METRICS.len());
    }
    #[test]
    fn event_count_and_record_size_bounded() {
        let p = std::env::temp_dir().join(format!("reborn-ring-{}", std::process::id()));
        let o = Observer::new(&p).unwrap();
        for i in 0..1000 {
            o.emit(
                Level::Info,
                "core",
                &format!("e{i}"),
                "event",
                None,
                json!({"large":"x".repeat(20000)}),
            );
        }
        let events = o.events(usize::MAX, None, None, None);
        assert_eq!(events.len(), RING_LIMIT);
        assert!(events
            .iter()
            .all(|e| serde_json::to_vec(e).unwrap().len() <= EVENT_LIMIT));
    }
}
