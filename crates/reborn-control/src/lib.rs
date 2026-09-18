#![forbid(unsafe_code)]
use reborn_observability::{Level, Observer};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::mpsc::{sync_channel, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};
pub const MAX_REQUEST: usize = 8192;
pub const MAX_RESPONSE: usize = 2 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u8,
    pub id: u64,
    pub command: Command,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Status,
    Health,
    Metrics,
    Snapshot,
    Events {
        last: usize,
    },
    Logs {
        last: usize,
        subsystem: Option<String>,
        level: Option<Level>,
        since_ms: Option<u64>,
    },
    LogLevel {
        subsystem: Option<String>,
        level: Option<Level>,
    },
    Diagnose,
    Tests,
    Test {
        name: Test,
        seconds: Option<u64>,
        saved: Option<u32>,
    },
    Scan,
    Radio {
        radio: Radio,
        action: RadioAction,
    },
    InputMonitor {
        seconds: u64,
    },
    Playback {
        action: PlaybackAction,
    },
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Radio {
    Wifi,
    Bluetooth,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioAction {
    Scan,
    On,
    Off,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", content = "value", rename_all = "snake_case")]
pub enum PlaybackAction {
    Play(i64),
    Pause,
    Resume,
    Stop,
    Next,
    Previous,
    Seek(u64),
    Wired,
    Bluetooth(String),
    Volume(u8),
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Test {
    Baseline,
    Decoder,
    Playback,
    Database,
    Library,
    Storage,
    Graphics,
    Input,
    WifiScan,
    WifiConnect,
    Bluetooth,
    BluetoothScan,
    AudioWired,
    AudioBluetooth,
}
pub const TESTS: &[(&str, bool)] = &[
    ("baseline", false),
    ("decoder", false),
    ("playback", false),
    ("database", false),
    ("library", false),
    ("storage", false),
    ("graphics", false),
    ("input", false),
    ("wifi-scan", false),
    ("wifi-connect", false),
    ("bluetooth", false),
    ("bluetooth-scan", false),
    ("audio-wired", true),
    ("audio-bluetooth", true),
];
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub version: u8,
    pub id: u64,
    pub ok: bool,
    pub result: Value,
    pub error: Option<String>,
}
impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            version: 1,
            id,
            ok: true,
            result,
            error: None,
        }
    }
    pub fn error(id: u64, e: impl Into<String>) -> Self {
        Self {
            version: 1,
            id,
            ok: false,
            result: Value::Null,
            error: Some(e.into()),
        }
    }
}
pub struct Envelope {
    pub request: Request,
    pub reply: SyncSender<Response>,
}
pub fn parse(bytes: &[u8]) -> Result<Request, String> {
    if bytes.len() > MAX_REQUEST {
        return Err("request size exceeded".into());
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid control request")?;
    let r: Request =
        serde_json::from_value(value.clone()).map_err(|_| "invalid control request")?;
    fn keys_known(input: &Value, canonical: &Value) -> bool {
        match (input, canonical) {
            (Value::Object(a), Value::Object(b)) => a
                .iter()
                .all(|(k, v)| b.get(k).is_some_and(|c| keys_known(v, c))),
            _ => true,
        }
    }
    if !keys_known(
        &value,
        &serde_json::to_value(&r).map_err(|_| "serialization error")?,
    ) {
        return Err("unknown request field".into());
    }
    if r.version != 1 {
        return Err("unsupported protocol version".into());
    }
    match &r.command {
        Command::InputMonitor { seconds } if !(1..=30).contains(seconds) => {
            return Err("input duration 1..30 seconds".into())
        }
        Command::Test {
            seconds: Some(s), ..
        } if !(1..=15).contains(s) => return Err("test duration 1..15 seconds".into()),
        Command::Logs { last, .. } | Command::Events { last } if *last > 512 => {
            return Err("event limit 512".into())
        }
        _ => {}
    }
    Ok(r)
}
struct Client {
    stream: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    written: usize,
    pending: Option<Receiver<Response>>,
    deadline: Instant,
}
pub struct Server {
    pub requests: Receiver<Envelope>,
    _path: PathBuf,
}
impl Server {
    pub fn spawn(path: &Path, log: Observer) -> std::io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("socket parent"))?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        if let Ok(m) = fs::symlink_metadata(path) {
            if !m.file_type().is_socket() {
                return Err(std::io::Error::other("control path is not a socket"));
            }
            if UnixStream::connect(path).is_ok() {
                return Err(std::io::Error::other("another Reborn owns the socket"));
            }
            fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let (tx, rx) = sync_channel(8);
        let path = path.to_owned();
        thread::Builder::new()
            .name("control".into())
            .spawn(move || {
                let mut clients: Vec<Client> = vec![];
                loop {
                    if let Ok((stream, _)) = listener.accept() {
                        if clients.len() < 8 {
                            let _ = stream.set_nonblocking(true);
                            clients.push(Client {
                                stream,
                                input: Vec::new(),
                                output: Vec::new(),
                                written: 0,
                                pending: None,
                                deadline: Instant::now() + Duration::from_secs(2),
                            });
                        }
                    }
                    clients.retain_mut(|client| {
                        if Instant::now() > client.deadline {
                            return false;
                        }
                        if let Some(rx) = &client.pending {
                            if let Ok(response) = rx.try_recv() {
                                client.output = serde_json::to_vec(&response).unwrap_or_default();
                                if client.output.len() > MAX_RESPONSE {
                                    client.output = serde_json::to_vec(&Response::error(
                                        response.id,
                                        "response size exceeded",
                                    ))
                                    .unwrap_or_default();
                                }
                                client.output.push(b'\n');
                                client.pending = None;
                                client.deadline = Instant::now() + Duration::from_secs(2);
                            }
                        }
                        if !client.output.is_empty() {
                            match client.stream.write(&client.output[client.written..]) {
                                Ok(0) => return false,
                                Ok(n) => {
                                    client.written += n;
                                    return client.written < client.output.len();
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    return true
                                }
                                Err(_) => return false,
                            }
                        }
                        if client.pending.is_some() {
                            return true;
                        }
                        let mut bytes = [0; 1024];
                        match client.stream.read(&mut bytes) {
                            Ok(0) => return false,
                            Ok(n) => client.input.extend_from_slice(&bytes[..n]),
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return true,
                            Err(_) => return false,
                        }
                        if client.input.len() > MAX_REQUEST {
                            log.emit(
                                Level::Warn,
                                "control",
                                "oversized_request",
                                "Rejected oversized local request",
                                None,
                                json!({}),
                            );
                            return false;
                        }
                        if let Some(end) = client.input.iter().position(|&b| b == b'\n') {
                            let response = match parse(&client.input[..end]) {
                                Ok(request) => {
                                    let id = request.id;
                                    let (rt, rr) = sync_channel(1);
                                    if tx.try_send(Envelope { request, reply: rt }).is_err() {
                                        Some(Response::error(id, "control queue busy"))
                                    } else {
                                        client.pending = Some(rr);
                                        client.deadline = Instant::now() + Duration::from_secs(90);
                                        None
                                    }
                                }
                                Err(e) => Some(Response::error(0, e)),
                            };
                            if let Some(r) = response {
                                client.output = serde_json::to_vec(&r).unwrap_or_default();
                                client.output.push(b'\n');
                            }
                        }
                        true
                    });
                    thread::sleep(Duration::from_millis(10));
                }
            })?;
        Ok(Self {
            requests: rx,
            _path: path,
        })
    }
}
pub fn call(path: &Path, request: &Request) -> Result<Response, String> {
    let mut stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(90)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let mut bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    stream.write_all(&bytes).map_err(|e| e.to_string())?;
    let mut data = vec![];
    stream
        .take(MAX_RESPONSE as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > MAX_RESPONSE {
        return Err("oversized response".into());
    }
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn radio_protocol_is_enumerated_and_rejects_extra_payloads() {
        assert!(parse(
            br#"{"version":1,"id":3,"command":{"op":"radio","radio":"wifi","action":"scan"}}"#
        )
        .is_ok());
        for command in [
            json!({"op":"radio","radio":"bluetooth","action":"pair"}),
            json!({"op":"radio","radio":"wifi","action":"exec"}),
            json!({"op":"radio","radio":"wifi","action":"scan","path":"/bin/sh"}),
            json!({"op":"radio","radio":"arbitrary","action":"scan"}),
        ] {
            assert!(parse(
                &serde_json::to_vec(&json!({"version":1,"id":3,"command":command})).unwrap()
            )
            .is_err());
        }
    }
    #[test]
    fn malformed_and_no_exec() {
        for s in [
            br#"{"version":1,"id":1,"command":{"op":"exec","path":"/bin/sh"}}"#.as_slice(),
            br#"{"version":1,"id":1,"command":{"op":"status","extra":1}}"#,
            b"not json",
        ] {
            assert!(parse(s).is_err())
        }
        assert!(parse(&vec![b' '; MAX_REQUEST + 1]).is_err())
    }
    #[test]
    fn bounded_hooks() {
        assert!(
            parse(br#"{"version":1,"id":1,"command":{"op":"input_monitor","seconds":999}}"#)
                .is_err()
        )
    }
    #[test]
    fn json_roundtrip() {
        let r = Request {
            version: 1,
            id: 42,
            command: Command::Status,
        };
        assert_eq!(parse(&serde_json::to_vec(&r).unwrap()).unwrap().id, 42);
    }
    #[test]
    fn safe_test_discovery() {
        assert_eq!(TESTS.iter().filter(|(_, audible)| *audible).count(), 2);
        assert!(!TESTS
            .iter()
            .any(|(s, _)| s.contains("exec") || s.contains("reboot")));
    }
}
