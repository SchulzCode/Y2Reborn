//! Wi-Fi Share: a web page on the player's own address that works as a file browser for Internal
//! Storage and the SD card (docs/ui/REBORN-WIFI-SHARE.md).
//!
//! The server runs only while the listener keeps Settings → Library → Wi-Fi Share open, speaks
//! plain HTTP to the home network, and lets in whoever knows the six-digit code on the player's
//! screen. It touches the storages' music folders and nothing else.
mod files;
mod http;

use self::http::Request;
use serde_json::json;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PAGE: &str = include_str!("page.html");
const COOKIE: &str = "y2share";
const CONNECTIONS: usize = 8;
const WRONG_CODES: u32 = 5;
const LOCKED: Duration = Duration::from_secs(30);
/// An SD write window lasts 60 s (`client::sd_write_window`): renewed well before.
const RENEW: Duration = Duration::from_secs(20);

/// One storage as the page shows it.
#[derive(Clone)]
pub struct Storage {
    pub key: &'static str,
    pub name: &'static str,
    pub root: PathBuf,
    /// Writes need the platform's write window (the SD card).
    pub guarded: bool,
}

/// The storages that are there now: a mounted file system whose music folder exists (Internal
/// Storage keeps its music in a folder of its own, the card is music from its top).
pub fn storages() -> Vec<Storage> {
    let all = [
        (
            "internal",
            "Internal Storage",
            "/media/internal",
            "/media/internal/music",
            false,
        ),
        ("sd", "SD Card", "/media/sd", "/media/sd", true),
    ];
    let mounts = crate::storage::mounts();
    let there = all.into_iter().filter(|(_, _, mount, root, _)| {
        mounts.iter().any(|m| m.path == Path::new(mount)) && Path::new(root).is_dir()
    });
    there
        .map(|(key, name, _, root, guarded)| Storage {
            key,
            name,
            root: root.into(),
            guarded,
        })
        .collect()
}

struct Shared {
    storages: Vec<Storage>,
    code: String,
    sessions: Mutex<Vec<String>>,
    /// Wrong codes in a row, and until when the code is refused.
    wrong: Mutex<(u32, Option<Instant>)>,
    stop: AtomicBool,
    busy: AtomicUsize,
    changes: AtomicU64,
    received: AtomicU64,
    window: fn(bool) -> Result<(), String>,
}

/// A running share. Dropping it stops the server.
pub struct Share {
    shared: Arc<Shared>,
    pub address: String,
}

fn random_hex(bytes: usize) -> io::Result<String> {
    let mut data = vec![0u8; bytes];
    File::open("/dev/urandom")?.read_exact(&mut data)?;
    Ok(data.iter().map(|b| format!("{b:02x}")).collect())
}

/// The address other devices reach the player at: the one the default route leaves from. No
/// packet is sent; connecting a datagram socket only asks the kernel which address it would use.
pub fn own_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(address) if !address.is_loopback() => Some(address),
        _ => None,
    }
}

impl Share {
    /// Start sharing `storages` on `port` (80 on the player; 0 picks one, for tests).
    pub fn start(storages: Vec<Storage>, port: u16) -> io::Result<Share> {
        Self::start_with(storages, port, crate::client::sd_write_window)
    }

    pub fn start_with(
        storages: Vec<Storage>,
        port: u16,
        window: fn(bool) -> Result<(), String>,
    ) -> io::Result<Share> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let number = u32::from_str_radix(&random_hex(4)?, 16).unwrap_or(0) % 1_000_000;
        let shared = Arc::new(Shared {
            storages,
            code: format!("{number:06}"),
            sessions: Mutex::new(vec![]),
            wrong: Mutex::new((0, None)),
            stop: AtomicBool::new(false),
            busy: AtomicUsize::new(0),
            changes: AtomicU64::new(0),
            received: AtomicU64::new(0),
            window,
        });
        let host = own_address().unwrap_or(Ipv4Addr::LOCALHOST);
        let address = match port {
            80 => format!("http://{host}"),
            port => format!("http://{host}:{port}"),
        };
        let serving = shared.clone();
        std::thread::Builder::new()
            .name("share".into())
            .spawn(move || accept(&listener, &serving))?;
        Ok(Share { shared, address })
    }
    pub fn code(&self) -> &str {
        &self.shared.code
    }
    /// How often something on a storage changed since the start.
    pub fn changes(&self) -> u64 {
        self.shared.changes.load(Ordering::Relaxed)
    }
    /// Files received since the start.
    pub fn received(&self) -> u64 {
        self.shared.received.load(Ordering::Relaxed)
    }
    /// A transfer is running now.
    pub fn busy(&self) -> bool {
        self.shared.busy.load(Ordering::Relaxed) > 0
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

fn accept(listener: &TcpListener, shared: &Arc<Shared>) {
    let open = Arc::new(AtomicUsize::new(0));
    while !shared.stop.load(Ordering::Relaxed) {
        let Ok((stream, _)) = listener.accept() else {
            std::thread::sleep(Duration::from_millis(80));
            continue;
        };
        if open.load(Ordering::Relaxed) >= CONNECTIONS {
            continue; // dropped: the browser tries again
        }
        open.fetch_add(1, Ordering::Relaxed);
        let (shared, count) = (shared.clone(), open.clone());
        let client = move || {
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
            serve(stream, &shared);
            count.fetch_sub(1, Ordering::Relaxed);
        };
        let spawned = std::thread::Builder::new()
            .name("share-client".into())
            .spawn(client);
        if spawned.is_err() {
            open.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn text(out: &mut TcpStream, status: &str, message: &str) {
    http::answer(
        out,
        status,
        "text/plain; charset=utf-8",
        message.as_bytes(),
        &[],
    );
}

fn serve(stream: TcpStream, shared: &Shared) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut input = BufReader::new(stream);
    let Ok(request) = http::read(&mut input) else {
        return text(&mut out, "400 Bad Request", "Bad request");
    };
    if request.method == "GET" && request.path == "/" {
        return http::answer(
            &mut out,
            "200 OK",
            "text/html; charset=utf-8",
            PAGE.as_bytes(),
            &[],
        );
    }
    if request.path == "/api/enter" {
        http::drain(&mut input, request.length);
        return enter(&mut out, &request, shared);
    }
    let known = request.cookie(COOKIE).is_some_and(|token| {
        let sessions = shared.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.iter().any(|s| s == token)
    });
    if !known {
        http::drain(&mut input, request.length);
        return text(
            &mut out,
            "401 Unauthorized",
            "Enter the code shown on the player",
        );
    }
    if let Err(error) = route(&mut input, &mut out, &request, shared) {
        http::drain(&mut input, request.length);
        let (status, message) = match error.kind() {
            io::ErrorKind::NotFound => ("404 Not Found", "Not found".to_owned()),
            io::ErrorKind::AlreadyExists => ("409 Conflict", "That name is taken".to_owned()),
            io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput => {
                ("403 Forbidden", "That isn't allowed here".to_owned())
            }
            io::ErrorKind::StorageFull => ("507 Insufficient Storage", "No space left".to_owned()),
            io::ErrorKind::ReadOnlyFilesystem => (
                "503 Service Unavailable",
                "This storage can't be written right now".to_owned(),
            ),
            _ => (
                "500 Internal Server Error",
                format!("Couldn't do that: {error}"),
            ),
        };
        text(&mut out, status, &message);
    }
}

/// The code on the player's screen opens a session; a few wrong ones lock the door for a while.
fn enter(out: &mut TcpStream, request: &Request, shared: &Shared) {
    let mut wrong = shared.wrong.lock().unwrap_or_else(|e| e.into_inner());
    if wrong.1.is_some_and(|until| Instant::now() < until) {
        return text(
            out,
            "429 Too Many Requests",
            "Too many wrong codes. Wait half a minute.",
        );
    }
    if request.param("code") != shared.code {
        wrong.0 += 1;
        if wrong.0 >= WRONG_CODES {
            *wrong = (0, Some(Instant::now() + LOCKED));
        }
        return text(out, "403 Forbidden", "That code isn't right");
    }
    *wrong = (0, None);
    let Ok(token) = random_hex(16) else {
        return text(out, "500 Internal Server Error", "Couldn't start a session");
    };
    let cookie = format!("Set-Cookie: {COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/");
    shared
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(token);
    http::answer(out, "200 OK", "text/plain; charset=utf-8", b"ok", &[cookie]);
}

/// The SD card's write window, held while this lives and renewed during a long upload.
struct Window<'a> {
    shared: &'a Shared,
    held: bool,
    renewed: Instant,
}

impl<'a> Window<'a> {
    fn open(shared: &'a Shared, storage: &Storage) -> io::Result<Self> {
        if storage.guarded {
            (shared.window)(true).map_err(|_| io::ErrorKind::ReadOnlyFilesystem)?;
        }
        Ok(Self {
            shared,
            held: storage.guarded,
            renewed: Instant::now(),
        })
    }
    fn renew(&mut self) {
        if self.held && self.renewed.elapsed() >= RENEW {
            let _ = (self.shared.window)(true);
            self.renewed = Instant::now();
        }
    }
}

impl Drop for Window<'_> {
    fn drop(&mut self) {
        if self.held {
            let _ = (self.shared.window)(false);
        }
        self.shared.changes.fetch_add(1, Ordering::Relaxed);
    }
}

fn route(
    input: &mut BufReader<TcpStream>,
    out: &mut TcpStream,
    request: &Request,
    shared: &Shared,
) -> io::Result<()> {
    let key = request.param("root");
    if request.path == "/api/list" && key.is_empty() {
        let storages: Vec<_> = shared
            .storages
            .iter()
            .map(|s| {
                let (free, total) = files::space(&s.root);
                json!({"key": s.key, "name": s.name, "free": free, "total": total})
            })
            .collect();
        return json_answer(out, &json!({"storages": storages}));
    }
    let storage = shared.storages.iter().find(|s| s.key == key);
    let storage = storage.ok_or(io::ErrorKind::NotFound)?;
    let path = files::resolve(&storage.root, request.param("path"))?;
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/list") => {
            let (free, total) = files::space(&storage.root);
            let entries = files::list(&path)?;
            json_answer(
                out,
                &json!({"entries": entries, "free": free, "total": total}),
            )
        }
        ("GET", "/api/file") => download(out, &path),
        ("PUT", "/api/file") => {
            let _busy = Busy::new(shared);
            let mut window = Window::open(shared, storage)?;
            upload(input, &path, request, &mut window)?;
            shared.received.fetch_add(1, Ordering::Relaxed);
            done(out)
        }
        ("POST", "/api/folder") => {
            let _window = Window::open(shared, storage)?;
            fs::create_dir(&path)?;
            done(out)
        }
        ("POST", "/api/rename") => {
            let to = request.param("to");
            let parent = path.parent().filter(|_| path != storage.root);
            let (true, Some(parent)) = (files::name_ok(to), parent) else {
                return Err(io::ErrorKind::InvalidInput.into());
            };
            let target = parent.join(to);
            if target.symlink_metadata().is_ok() {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            let _window = Window::open(shared, storage)?;
            fs::rename(&path, target)?;
            done(out)
        }
        ("POST", "/api/delete") if path != storage.root => {
            let _window = Window::open(shared, storage)?;
            files::delete(&path)?;
            done(out)
        }
        _ => Err(io::ErrorKind::NotFound.into()),
    }
}

fn json_answer(out: &mut TcpStream, value: &serde_json::Value) -> io::Result<()> {
    let body = value.to_string();
    http::answer(out, "200 OK", "application/json", body.as_bytes(), &[]);
    Ok(())
}

fn done(out: &mut TcpStream) -> io::Result<()> {
    text(out, "200 OK", "ok");
    Ok(())
}

/// Counts a running transfer for the player's page.
struct Busy<'a>(&'a Shared);
impl<'a> Busy<'a> {
    fn new(shared: &'a Shared) -> Self {
        shared.busy.fetch_add(1, Ordering::Relaxed);
        Self(shared)
    }
}
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.busy.fetch_sub(1, Ordering::Relaxed);
    }
}

fn download(out: &mut TcpStream, path: &Path) -> io::Result<()> {
    let mut file = File::open(path)?;
    let info = file.metadata()?;
    if !info.is_file() {
        return Err(io::ErrorKind::NotFound.into());
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    // The name once plain for old browsers (quotes and controls out), once exact (RFC 5987).
    let plain: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let exact: String = name
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => (b as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect();
    let disposition =
        format!("Content-Disposition: attachment; filename=\"{plain}\"; filename*=UTF-8''{exact}");
    let head = http::head(
        "200 OK",
        "application/octet-stream",
        info.len(),
        &[disposition],
    );
    out.write_all(head.as_bytes())?;
    io::copy(&mut file, out)?;
    Ok(())
}

/// Receive a file beside its place as `.name.part` and move it there when it is whole, so a
/// broken transfer leaves no half song. An existing file stays unless the page asked to replace.
fn upload(
    input: &mut BufReader<TcpStream>,
    path: &Path,
    request: &Request,
    window: &mut Window,
) -> io::Result<()> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::ErrorKind::InvalidInput.into());
    };
    if path.symlink_metadata().is_ok() && request.param("replace") != "1" {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    if path.is_dir() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    // A folder dropped on the page arrives file by file with its folders in the path.
    fs::create_dir_all(parent)?;
    if files::space(parent).0 < request.length {
        return Err(io::ErrorKind::StorageFull.into());
    }
    let part = parent.join(format!(".{}.part", name.to_string_lossy()));
    let result = (|| {
        let mut file = File::create(&part)?;
        let mut left = request.length;
        let mut buffer = vec![0u8; 256 * 1024];
        while left > 0 {
            let want = buffer.len().min(left as usize);
            let got = input.read(&mut buffer[..want])?;
            if got == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            file.write_all(&buffer[..got])?;
            left -= got as u64;
            window.renew();
        }
        file.sync_all()?;
        fs::rename(&part, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

#[cfg(test)]
mod tests;
