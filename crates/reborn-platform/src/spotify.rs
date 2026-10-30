//! Spotify: the sign-in, the library and the player.
//!
//! Ported from PiPod-OS (a Rockbox fork, C) and kept to the same parts:
//!
//! - **Sign-in** is OAuth 2.0 Device Authorization (RFC 8628) with the
//!   client id librespot presents itself as: the player shows a QR code, the
//!   listener approves on a phone, the player polls until Spotify says so.
//!   No Developer Dashboard app, no redirect, no password on the player.
//! - **Playback** is `spotifyd` (librespot), started by Reborn as its own
//!   child with the `pipe` backend. Unlike PiPod-OS, where spotifyd wrote to
//!   ALSA itself, its PCM comes back to Reborn through a FIFO; `Audio` hands
//!   it to the decoder as a streamed WAV, so the equalizer, the volume,
//!   Bluetooth and the jack all apply as they do to any song.
//! - **Control** is spotifyd's MPRIS interface (plus its own
//!   `rs.spotifyd.Controls`) on the system bus, through the `dbus` crate
//!   rather than PiPod-OS's `busctl`.
//! - **Library** browsing (playlists, Liked Songs, albums, artists) runs the
//!   `reborn-spotify-metadata` helper (`tools/spotify-metadata`), which reads
//!   Spotify's own client protocol with the same session: the public Web API
//!   cannot read followed playlists for an app like this one.
//!
//! Nothing secret is ever logged; the account file and spotifyd's cache are
//! root-only (0600/0700) under `/data/reborn/state/spotify`.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as Process, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Identifies Spotify in the play queue (`Track::source_id`).
pub const SOURCE_ID: &str = reborn_core::SPOTIFY_SOURCE;
/// spotifyd, built with the `dbus_mpris` feature and `tools/spotify`'s patch.
pub const DAEMON: &str = "/usr/bin/spotifyd";
/// The library helper built from `tools/spotify-metadata`.
pub const METADATA: &str = "/usr/bin/reborn-spotify-metadata";
const CURL: &str = "/usr/bin/curl";
const USER_AGENT: &str = concat!("Y2Reborn/", env!("CARGO_PKG_VERSION"));

/// librespot's own client id (`KEYMASTER_CLIENT_ID`): one of the few Spotify
/// enables the device grant for, and the one its streaming servers accept.
const CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
/// spotifyd's own scope list (`src/oauth.rs`), all accepted for this client.
const SCOPES: &str = "app-remote-control playlist-modify playlist-modify-private \
playlist-modify-public playlist-read playlist-read-collaborative playlist-read-private \
streaming ugc-image-upload user-follow-modify user-follow-read user-library-modify \
user-library-read user-modify user-modify-playback-state user-modify-private \
user-personalized user-read-birthdate user-read-currently-playing user-read-email \
user-read-play-history user-read-playback-position user-read-playback-state \
user-read-private user-read-recently-played user-top-read";
const AUTHORIZE: &str = "https://accounts.spotify.com/oauth2/device/authorize";
const TOKEN: &str = "https://accounts.spotify.com/api/token";
/// librespot's `AUTHENTICATION_SPOTIFY_TOKEN` (authentication.proto).
const AUTH_SPOTIFY_TOKEN: u32 = 3;

const MPRIS_PREFIX: &str = "rs.spotifyd.instance";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const CONTROLS_PATH: &str = "/rs/spotifyd/Controls";
const CONTROLS: &str = "rs.spotifyd.Controls";

/// librespot's pipe backend: 44.1 kHz, 16 bit, stereo, native (little) endian.
pub const RATE: u32 = 44_100;
const FRAME: usize = 4;

fn tool(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.into())
}
fn daemon_path() -> String {
    tool("REBORN_SPOTIFYD", DAEMON)
}
fn metadata_path() -> String {
    tool("REBORN_SPOTIFY_METADATA", METADATA)
}
fn curl() -> String {
    tool("REBORN_CURL", CURL)
}

/// Both programs Spotify needs are installed (a build without them simply
/// has no Spotify, as one without the Last.fm key has no Last.fm).
pub fn installed() -> bool {
    let runnable = |p: String| {
        fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    runnable(daemon_path()) && runnable(metadata_path())
}

// ------------------------------------------------------------------ files

/// Where Spotify keeps its state: `<root>/state/spotify`.
#[derive(Clone, Debug)]
pub struct Paths {
    dir: PathBuf,
}
impl Paths {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: root.join("state/spotify"),
        }
    }
    /// The sign-in (refresh token, display name): 0600.
    pub fn account(&self) -> PathBuf {
        self.dir.join("account.json")
    }
    /// spotifyd's `cache_path` (its reusable credentials, no audio).
    pub fn cache(&self) -> PathBuf {
        self.dir.join("cache")
    }
    /// The OAuth credentials blob spotifyd and the helper both start from.
    pub fn oauth(&self) -> PathBuf {
        self.cache().join("oauth")
    }
    pub fn config(&self) -> PathBuf {
        self.dir.join("spotifyd.conf")
    }
    /// The FIFO spotifyd's pipe backend writes into.
    pub fn fifo(&self) -> PathBuf {
        self.dir.join("audio.pcm")
    }
    fn private_dir(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
    }
}

/// The stored sign-in. The access token only lives until spotifyd and the
/// helper have turned it into their own reusable credentials; the refresh
/// token gets a fresh one when they have not (yet).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Account {
    pub refresh_token: String,
    #[serde(default)]
    pub access_token: String,
    /// Unix seconds.
    #[serde(default)]
    pub expires_at: u64,
    /// The account's name as Spotify reports it once connected.
    #[serde(default)]
    pub name: String,
}
impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Account {{ name: {:?}, .. }}", self.name)
    }
}

pub fn load_account(paths: &Paths) -> Option<Account> {
    let a: Account = serde_json::from_slice(&fs::read(paths.account()).ok()?).ok()?;
    (!a.refresh_token.is_empty()).then_some(a)
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(tmp, path)
}

pub fn save_account(paths: &Paths, account: &Account) -> io::Result<()> {
    paths.private_dir(&paths.dir)?;
    write_private(
        &paths.account(),
        &serde_json::to_vec(account).map_err(io::Error::other)?,
    )
}

/// The credentials file spotifyd reads from `<cache_path>/oauth`: the access
/// token as librespot's `Credentials` (auth_type 3, standard base64).
pub fn credentials_blob(access_token: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "username": null,
        "auth_type": AUTH_SPOTIFY_TOKEN,
        "auth_data": base64(access_token.as_bytes()),
    }))
    .unwrap_or_default()
}

fn write_credentials(paths: &Paths, access_token: &str) -> io::Result<()> {
    paths.private_dir(&paths.cache())?;
    paths.private_dir(&paths.oauth())?;
    write_private(
        &paths.oauth().join("credentials.json"),
        &credentials_blob(access_token),
    )
}

/// spotifyd keeps reusable credentials of its own once it has connected.
fn has_reusable_credentials(paths: &Paths) -> bool {
    paths.cache().join("credentials.json").is_file()
}

/// Sign out: the account, spotifyd's cache and the OAuth blob.
pub fn forget(paths: &Paths) -> io::Result<()> {
    for file in [paths.account(), paths.config()] {
        match fs::remove_file(file) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    match fs::remove_dir_all(paths.cache()) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ----------------------------------------------------------------- sign-in

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No network, curl missing or a reply that never came.
    Offline(String),
    /// Spotify refused (denied, expired code, revoked sign-in).
    Refused(String),
}
impl Error {
    pub fn message(&self) -> String {
        match self {
            Self::Offline(_) => "Spotify can't be reached. Check Wi-Fi.".into(),
            Self::Refused(reason) => match reason.as_str() {
                "access_denied" => "The sign-in was declined.".into(),
                "expired_token" => "The code expired. Try again.".into(),
                "invalid_grant" => "Spotify ended the sign-in. Connect again.".into(),
                _ => "Spotify refused the sign-in.".into(),
            },
        }
    }
}

/// A sign-in waiting for approval on a phone.
#[derive(Clone)]
pub struct DeviceCode {
    device_code: String,
    /// The code to type at spotify.com/pair when the QR code can't be used.
    pub user_code: String,
    /// What the QR code holds: the approval page with the code filled in.
    pub url: String,
    pub interval: Duration,
    pub expires: Instant,
}
impl std::fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceCode {{ user_code: {:?}, .. }}", self.user_code)
    }
}

pub enum Poll {
    /// Not yet; ask again after `DeviceCode::interval` (which a slow_down
    /// reply has already lengthened).
    Pending,
    Approved(Account),
}

fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// POST a form over HTTPS; the body (codes, tokens) goes on curl's stdin,
/// never on its command line. Spotify answers errors as JSON with HTTP 400,
/// which curl still exits 0 for.
fn post(url: &str, body: &str) -> Result<serde_json::Value, Error> {
    let mut child = Process::new(curl())
        .args(["-sS", "--proto", "=https", "--tlsv1.2", "--max-time", "20"])
        .args(["--connect-timeout", "10", "-A", USER_AGENT])
        .args(["-H", "Content-Type: application/x-www-form-urlencoded"])
        .args(["--data-binary", "@-", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::Offline(e.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| Error::Offline(e.to_string()))?;
    }
    let mut reply = vec![];
    if let Some(mut out) = child.stdout.take() {
        let _ = out.by_ref().take(64 * 1024).read_to_end(&mut reply);
    }
    let status = child.wait().map_err(|e| Error::Offline(e.to_string()))?;
    if !status.success() && reply.is_empty() {
        return Err(Error::Offline(format!(
            "curl {}",
            status.code().unwrap_or(-1)
        )));
    }
    serde_json::from_slice(&reply).map_err(|_| Error::Offline("not JSON".into()))
}

fn text(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Ask Spotify for a code to show.
pub fn request_code() -> Result<DeviceCode, Error> {
    let body = format!("client_id={CLIENT_ID}&scope={}", form_encode(SCOPES));
    parse_code(&post(AUTHORIZE, &body)?, Instant::now())
}

pub fn parse_code(v: &serde_json::Value, now: Instant) -> Result<DeviceCode, Error> {
    if let Some(error) = text(v, "error") {
        return Err(Error::Refused(error));
    }
    let (Some(device_code), Some(user_code), Some(url)) = (
        text(v, "device_code"),
        text(v, "user_code"),
        text(v, "verification_uri_complete"),
    ) else {
        return Err(Error::Offline("incomplete reply".into()));
    };
    // RFC 8628's defaults when a field is missing.
    let interval = v.get("interval").and_then(|i| i.as_u64()).unwrap_or(5);
    let expires = v.get("expires_in").and_then(|e| e.as_u64()).unwrap_or(1800);
    Ok(DeviceCode {
        device_code,
        user_code,
        url,
        interval: Duration::from_secs(interval.clamp(1, 60)),
        expires: now + Duration::from_secs(expires),
    })
}

/// Ask once whether the code was approved. Call at most every `interval`.
pub fn poll(code: &mut DeviceCode, now_unix: u64) -> Result<Poll, Error> {
    if Instant::now() >= code.expires {
        return Err(Error::Refused("expired_token".into()));
    }
    let body = format!(
        "client_id={CLIENT_ID}&device_code={}&grant_type={}",
        form_encode(&code.device_code),
        form_encode("urn:ietf:params:oauth:grant-type:device_code")
    );
    parse_poll(&post(TOKEN, &body)?, code, now_unix)
}

pub fn parse_poll(
    v: &serde_json::Value,
    code: &mut DeviceCode,
    now_unix: u64,
) -> Result<Poll, Error> {
    match text(v, "error").as_deref() {
        Some("authorization_pending") => return Ok(Poll::Pending),
        Some("slow_down") => {
            code.interval += Duration::from_secs(5);
            return Ok(Poll::Pending);
        }
        Some(other) => return Err(Error::Refused(other.into())),
        None => {}
    }
    let (Some(access_token), Some(refresh_token)) =
        (text(v, "access_token"), text(v, "refresh_token"))
    else {
        return Err(Error::Offline("incomplete reply".into()));
    };
    let expires_in = v.get("expires_in").and_then(|e| e.as_u64()).unwrap_or(3600);
    Ok(Poll::Approved(Account {
        refresh_token,
        access_token,
        expires_at: now_unix + expires_in,
        name: String::new(),
    }))
}

/// Keep an approved sign-in: the account file and spotifyd's credentials.
pub fn sign_in(paths: &Paths, account: &Account) -> io::Result<()> {
    // A previous account's cached credentials must not outlive it.
    let _ = forget(paths);
    save_account(paths, account)?;
    write_credentials(paths, &account.access_token)
}

/// Before spotifyd starts: when it holds no reusable credentials yet and the
/// stored access token has run out, get a fresh one with the refresh token.
pub fn prepare(paths: &Paths, now_unix: u64) -> Result<(), Error> {
    if has_reusable_credentials(paths) {
        return Ok(());
    }
    let mut account = load_account(paths).ok_or_else(|| Error::Refused("invalid_grant".into()))?;
    if account.expires_at > now_unix + 120 && !account.access_token.is_empty() {
        return write_credentials(paths, &account.access_token)
            .map_err(|e| Error::Offline(e.to_string()));
    }
    refresh(paths, &mut account, now_unix)
}

/// A fresh access token from the refresh token, kept and written where
/// spotifyd and the helper start from.
fn refresh(paths: &Paths, account: &mut Account, now_unix: u64) -> Result<(), Error> {
    let body = format!(
        "client_id={CLIENT_ID}&grant_type=refresh_token&refresh_token={}",
        form_encode(&account.refresh_token)
    );
    let v = post(TOKEN, &body)?;
    if let Some(error) = text(&v, "error") {
        return Err(Error::Refused(error));
    }
    let access_token = text(&v, "access_token").ok_or(Error::Offline("no token".into()))?;
    account.access_token = access_token;
    account.expires_at = now_unix + v.get("expires_in").and_then(|e| e.as_u64()).unwrap_or(3600);
    // Spotify may rotate the refresh token.
    if let Some(refresh) = text(&v, "refresh_token") {
        account.refresh_token = refresh;
    }
    save_account(paths, account).map_err(|e| Error::Offline(e.to_string()))?;
    write_credentials(paths, &account.access_token).map_err(|e| Error::Offline(e.to_string()))
}

// ----------------------------------------------------------------- library

/// One row of a Spotify list as the helper prints it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub label: String,
    #[serde(default)]
    pub uri: Option<String>,
    /// Base62 id: the playlist/album/artist itself, or a track's album.
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub items: Vec<Row>,
    /// Passed back as the cursor for the next page.
    #[serde(default)]
    pub next: Option<String>,
    /// A context covering the whole list (Liked Songs).
    #[serde(default)]
    pub context: Option<String>,
}

pub fn parse_listing(bytes: &[u8]) -> Result<Listing, String> {
    let mut listing: Listing =
        serde_json::from_slice(bytes).map_err(|_| "Spotify sent something unreadable")?;
    listing.items.retain(|r| !r.label.trim().is_empty());
    listing.items.truncate(500);
    Ok(listing)
}

/// Commands the helper knows; the argument (an id, a cursor) only ever comes
/// from a previous listing, but is still checked to be a plain token.
pub const COMMANDS: [&str; 9] = [
    "playlists",
    "playlist-tracks",
    "liked-songs",
    "saved-albums",
    "followed-artists",
    "album-tracks",
    "artist-albums",
    "artist-top-tracks",
    "artist-all-tracks",
];

fn plain(value: &str) -> bool {
    value.len() <= 64 && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Run the helper for one list. The helper keeps reusable credentials once
/// it connected; until then it starts from the access token, which lasts an
/// hour: when that has run out, it gets a fresh one and tries once more.
pub fn browse(
    paths: &Paths,
    command: &str,
    arg: &str,
    cursor: &str,
    now_unix: u64,
) -> Result<Listing, String> {
    if !COMMANDS.contains(&command) || !plain(arg) || !plain(cursor) {
        return Err("unknown list".into());
    }
    match run_helper(paths, command, arg, cursor) {
        Err(Helper::Session) => {
            let mut account = load_account(paths).ok_or(Helper::Session.message())?;
            refresh(paths, &mut account, now_unix).map_err(|e| e.message())?;
            run_helper(paths, command, arg, cursor).map_err(|e| e.message())
        }
        result => result.map_err(|e| e.message()),
    }
}

enum Helper {
    Missing(String),
    /// The session did not connect (an expired token, or no network).
    Session,
    Slow,
    Failed,
    Unreadable,
}
impl Helper {
    fn message(&self) -> String {
        match self {
            Self::Missing(e) => format!("The Spotify library tool is missing ({e})."),
            Self::Session => "Spotify can't be reached. Check Wi-Fi.".into(),
            Self::Slow => "Spotify took too long to answer.".into(),
            Self::Failed => "Spotify didn't send this list.".into(),
            Self::Unreadable => "Spotify sent something unreadable.".into(),
        }
    }
}

fn run_helper(paths: &Paths, command: &str, arg: &str, cursor: &str) -> Result<Listing, Helper> {
    let child = Process::new(metadata_path())
        .args([command, arg, cursor])
        .env("REBORN_SPOTIFY_CACHE_DIR", paths.oauth())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Helper::Missing(e.to_string()))?;
    let output = wait_with_deadline(child, Duration::from_secs(60)).map_err(|_| Helper::Slow)?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let last = detail.lines().last().unwrap_or("").trim();
        return Err(
            if last.contains("no stored session") || last.contains("connect failed") {
                Helper::Session
            } else {
                Helper::Failed
            },
        );
    }
    parse_listing(&output.stdout).map_err(|_| Helper::Unreadable)
}

fn wait_with_deadline(mut child: Child, limit: Duration) -> Result<std::process::Output, String> {
    let deadline = Instant::now() + limit;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut buf = vec![];
        if let Some(e) = stderr.as_mut() {
            let _ = e.by_ref().take(64 * 1024).read_to_end(&mut buf);
        }
        buf
    });
    let out_reader = std::thread::spawn(move || {
        let mut buf = vec![];
        if let Some(o) = stdout.as_mut() {
            let _ = o.by_ref().take(4 << 20).read_to_end(&mut buf);
        }
        buf
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Spotify took too long to answer.".into());
            }
        }
    };
    Ok(std::process::Output {
        status,
        stdout: out_reader.join().unwrap_or_default(),
        stderr: err_reader.join().unwrap_or_default(),
    })
}

// ------------------------------------------------------------------ player

/// The song spotifyd plays, from its MPRIS metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Song {
    /// `mpris:trackid`: changes with every song.
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// The cover on Spotify's image server (https://i.scdn.co/...).
    pub art_url: String,
    pub length_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Starting,
    /// spotifyd answers on the bus.
    Ready,
    /// spotifyd stopped; the reason, briefly.
    Failed(String),
}

/// What the player shows, refreshed about once a second.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub phase: Phase,
    pub playing: bool,
    /// `PlaybackStatus` was read at all (the Player object exists only once
    /// something played).
    pub active: bool,
    pub song: Option<Song>,
    pub position_ms: u64,
    /// When `position_ms` was read: the position moves on from there.
    pub read_at: Option<Instant>,
    /// Commands spotifyd refused so far, and the last one with its error
    /// name (the app logs each new one).
    pub refused: u64,
    pub last_refusal: Option<(&'static str, String)>,
}
impl Status {
    pub fn position_now(&self) -> u64 {
        let elapsed = match (self.playing, self.read_at) {
            (true, Some(at)) => at.elapsed().as_millis() as u64,
            _ => 0,
        };
        let position = self.position_ms + elapsed;
        match &self.song {
            Some(s) if s.length_ms > 0 => position.min(s.length_ms),
            _ => position,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Play `track` within `context` (a playlist or album); without the
    /// patched spotifyd, the track alone.
    PlayInContext {
        context: String,
        track: String,
    },
    /// Play a track, album, playlist or artist from its start.
    OpenUri(String),
    Play,
    Pause,
    Next,
    Previous,
    /// Move by this many milliseconds.
    Seek(i64),
}

/// spotifyd running for Reborn. Dropping it stops spotifyd.
pub struct Daemon {
    commands: SyncSender<Command>,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    pump: Option<JoinHandle<()>>,
}

struct Shared {
    stop: AtomicBool,
    status: Mutex<Status>,
    child: Mutex<Option<Child>>,
    pump: Arc<Pump>,
}

impl Daemon {
    /// Write spotifyd's configuration and start it (credentials must be in
    /// place: `prepare`). `name` is how phones list the player.
    pub fn start(paths: &Paths, name: &str) -> io::Result<Self> {
        paths.private_dir(&paths.dir)?;
        paths.private_dir(&paths.cache())?;
        write_private(&paths.config(), config(paths, name).as_bytes())?;
        let fifo = paths.fifo();
        make_fifo(&fifo)?;
        // Read and write: the read end never sees EOF when spotifyd closes
        // and reopens its end, and opening never waits for a writer.
        let audio = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(&fifo)?;
        let mut command = Process::new(daemon_path());
        command
            .args(["--no-daemon", "--config-path"])
            .arg(paths.config())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // spotifyd ends with Reborn, however Reborn ends.
        // SAFETY: prctl only, between fork and exec.
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        let pid = child.id();
        let pump = Arc::new(Pump::default());
        if let Ok(mut live) = PUMP.lock() {
            *live = Some(Arc::downgrade(&pump));
        }
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            status: Mutex::new(Status::default()),
            child: Mutex::new(Some(child)),
            pump,
        });
        let pump_shared = shared.clone();
        let pump = std::thread::Builder::new()
            .name("spotify-pump".into())
            .spawn(move || pump_shared.pump.run(audio, &pump_shared.stop))?;
        let (commands, inbox) = sync_channel(16);
        let worker_shared = shared.clone();
        let worker = std::thread::Builder::new()
            .name("spotify-control".into())
            .spawn(move || control(pid, inbox, &worker_shared))?;
        Ok(Self {
            commands,
            shared,
            worker: Some(worker),
            pump: Some(pump),
        })
    }
    /// Queue a command; false when spotifyd is not answering commands.
    pub fn send(&self, command: Command) -> bool {
        if matches!(command, Command::PlayInContext { .. } | Command::OpenUri(_)) {
            // What is still in the pipe belongs to the song before.
            self.shared.pump.flush.store(true, Ordering::SeqCst);
        }
        self.commands.try_send(command).is_ok()
    }
    pub fn status(&self) -> Status {
        self.shared
            .status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.pump.wake.notify_all();
        if let Ok(mut child) = self.shared.child.lock() {
            if let Some(mut child) = child.take() {
                // SIGTERM first: spotifyd saves its session on the way out.
                // SAFETY: signalling our own child by its pid.
                unsafe {
                    libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
                }
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline {
                    if let Ok(Some(_)) = child.try_wait() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        // spotifyd is gone; a pump still writing to a decoder that stopped
        // reading wakes up with EPIPE when that decoder is dropped.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
    }
}

fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// spotifyd's configuration: the pipe backend into the FIFO, no volume of
/// its own (Reborn's volume applies), no audio cache (flash space), no
/// discovery on the network (one account: the one signed in here).
pub fn config(paths: &Paths, name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(48).collect();
    let name = if name.trim().is_empty() {
        "Y2Reborn".to_owned()
    } else {
        name
    };
    format!(
        "[global]\n\
         backend = \"pipe\"\n\
         device = {}\n\
         audio_format = \"S16\"\n\
         bitrate = 320\n\
         device_name = {}\n\
         device_type = \"speaker\"\n\
         cache_path = {}\n\
         no_audio_cache = true\n\
         volume_controller = \"none\"\n\
         use_mpris = true\n\
         dbus_type = \"system\"\n\
         disable_discovery = true\n\
         autoplay = false\n",
        toml_string(&paths.fifo().to_string_lossy()),
        toml_string(&name),
        toml_string(&paths.cache().to_string_lossy()),
    )
}

fn make_fifo(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    if let Ok(m) = fs::symlink_metadata(path) {
        if m.file_type().is_fifo() {
            return Ok(());
        }
        fs::remove_file(path)?;
    }
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: a NUL-terminated path that lives through the call.
    if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// --------------------------------------------------------------- control

fn control(pid: u32, inbox: Receiver<Command>, shared: &Shared) {
    let set = |f: &mut dyn FnMut(&mut Status)| {
        if let Ok(mut s) = shared.status.lock() {
            f(&mut s);
        }
    };
    let mut bus: Option<Mpris> = None;
    let mut next_poll = Instant::now();
    while !shared.stop.load(Ordering::SeqCst) {
        let exited = shared
            .child
            .lock()
            .ok()
            .and_then(|mut c| c.as_mut().map(|c| c.try_wait()))
            .and_then(|r| r.ok())
            .flatten();
        if let Some(status) = exited {
            let reason = match status.code() {
                Some(code) => format!("spotifyd stopped ({code})"),
                None => "spotifyd stopped".to_owned(),
            };
            set(&mut |s| {
                s.phase = Phase::Failed(reason.clone());
                s.playing = false;
            });
            return;
        }
        if bus.is_none() {
            bus = Mpris::find(pid);
            if bus.is_some() {
                set(&mut |s| s.phase = Phase::Ready);
            }
        }
        let wait = next_poll.saturating_duration_since(Instant::now());
        match inbox.recv_timeout(wait.min(Duration::from_millis(250))) {
            Ok(command) => {
                let result = match &bus {
                    Some(mpris) => mpris.run(&command),
                    None => Err("not on the bus yet".to_owned()),
                };
                if let Err(error) = result {
                    set(&mut |s| {
                        s.refused += 1;
                        s.last_refusal = Some((command_name(&command), error.clone()));
                    });
                }
                // Read again soon: the song or the state changed.
                next_poll = Instant::now() + Duration::from_millis(300);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() < next_poll {
            continue;
        }
        next_poll = Instant::now() + Duration::from_secs(1);
        if let Some(mpris) = &bus {
            let read = mpris.read();
            set(&mut |s| match &read {
                Some((playing, song, position)) => {
                    s.active = true;
                    s.playing = *playing;
                    s.song = song.clone();
                    s.position_ms = *position;
                    s.read_at = Some(Instant::now());
                }
                None => {
                    s.active = false;
                    s.playing = false;
                }
            });
        }
    }
}

struct Mpris {
    c: dbus::blocking::Connection,
    name: String,
}
impl Mpris {
    /// spotifyd's bus name carries its pid; any instance will do otherwise.
    fn find(pid: u32) -> Option<Self> {
        let c = dbus::blocking::Connection::new_system().ok()?;
        let (names,): (Vec<String>,) = c
            .with_proxy(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                Duration::from_secs(2),
            )
            .method_call("org.freedesktop.DBus", "ListNames", ())
            .ok()?;
        let own = format!("{MPRIS_PREFIX}{pid}");
        let name = names
            .iter()
            .find(|n| **n == own)
            .or_else(|| names.iter().find(|n| n.starts_with(MPRIS_PREFIX)))?
            .clone();
        Some(Self { c, name })
    }
    fn call<A: dbus::arg::AppendAll>(
        &self,
        path: &str,
        iface: &str,
        method: &str,
        args: A,
    ) -> Result<(), dbus::Error> {
        self.c
            .with_proxy(&self.name, path, Duration::from_secs(5))
            .method_call(iface, method, args)
    }
    /// A fresh spotifyd has no Player object until playback is transferred
    /// to it (PiPod-OS found this on real hardware): transfer, then retry.
    fn with_transfer<A: dbus::arg::AppendAll + Clone>(
        &self,
        path: &str,
        iface: &str,
        method: &str,
        args: A,
    ) -> Result<(), dbus::Error> {
        match self.call(path, iface, method, args.clone()) {
            Err(e)
                if e.name().is_some_and(|n| {
                    n.contains("UnknownObject")
                        || (n.contains("UnknownMethod") && path == MPRIS_PATH)
                }) =>
            {
                self.call(CONTROLS_PATH, CONTROLS, "TransferPlayback", ())?;
                std::thread::sleep(Duration::from_secs(1));
                self.call(path, iface, method, args)
            }
            other => other,
        }
    }
    fn run(&self, command: &Command) -> Result<(), String> {
        let result = match command {
            Command::PlayInContext { context, track } => {
                match self.call(
                    CONTROLS_PATH,
                    CONTROLS,
                    "PlayContextTrack",
                    (context.as_str(), track.as_str()),
                ) {
                    // An unpatched spotifyd: the track on its own album.
                    Err(e) if e.name().is_some_and(|n| n.contains("UnknownMethod")) => {
                        self.with_transfer(MPRIS_PATH, PLAYER, "OpenUri", (track.as_str(),))
                    }
                    other => other,
                }
            }
            Command::OpenUri(uri) => {
                self.with_transfer(MPRIS_PATH, PLAYER, "OpenUri", (uri.as_str(),))
            }
            Command::Play => self.with_transfer(MPRIS_PATH, PLAYER, "Play", ()),
            Command::Pause => self.call(MPRIS_PATH, PLAYER, "Pause", ()),
            Command::Next => self.call(MPRIS_PATH, PLAYER, "Next", ()),
            Command::Previous => self.call(MPRIS_PATH, PLAYER, "Previous", ()),
            Command::Seek(ms) => self.call(MPRIS_PATH, PLAYER, "Seek", (ms * 1000,)),
        };
        result.map_err(|e| e.name().unwrap_or("unknown").to_owned())
    }
    /// (playing, song, position in ms), or None before anything played.
    fn read(&self) -> Option<(bool, Option<Song>, u64)> {
        use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
        let p = self
            .c
            .with_proxy(&self.name, MPRIS_PATH, Duration::from_secs(2));
        let status: String = p.get(PLAYER, "PlaybackStatus").ok()?;
        let position: i64 = p.get(PLAYER, "Position").unwrap_or(0);
        let metadata: dbus::arg::PropMap = p.get(PLAYER, "Metadata").unwrap_or_default();
        Some((
            status == "Playing",
            song_of(&metadata),
            (position.max(0) / 1000) as u64,
        ))
    }
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::PlayInContext { .. } => "play_in_context",
        Command::OpenUri(_) => "open_uri",
        Command::Play => "play",
        Command::Pause => "pause",
        Command::Next => "next",
        Command::Previous => "previous",
        Command::Seek(_) => "seek",
    }
}

/// The song from MPRIS metadata (xesam:artist is a list).
pub fn song_of(metadata: &dbus::arg::PropMap) -> Option<Song> {
    use dbus::arg::RefArg;
    let string = |key: &str| {
        metadata
            .get(key)
            .and_then(|v| v.0.as_str())
            .unwrap_or("")
            .to_owned()
    };
    let artists = metadata
        .get("xesam:artist")
        .and_then(|v| v.0.as_iter())
        .map(|i| {
            i.flat_map(|a| match a.as_iter() {
                Some(inner) => inner
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect(),
                None => a
                    .as_str()
                    .map(str::to_owned)
                    .into_iter()
                    .collect::<Vec<_>>(),
            })
            .collect::<Vec<_>>()
            .join(", ")
        })
        .unwrap_or_default();
    let id = match metadata.get("mpris:trackid") {
        Some(v) => v.0.as_str().map(str::to_owned).unwrap_or_default(),
        None => String::new(),
    };
    let title = string("xesam:title");
    if title.is_empty() {
        return None;
    }
    let length_us = metadata
        .get("mpris:length")
        .and_then(|v| v.0.as_i64())
        .unwrap_or(0);
    Some(Song {
        id: if id.is_empty() { title.clone() } else { id },
        title,
        artist: artists,
        album: string("xesam:album"),
        art_url: Some(string("mpris:artUrl"))
            .filter(|u| u.starts_with("https://") && u.len() <= 512)
            .unwrap_or_default(),
        length_ms: (length_us.max(0) / 1000) as u64,
    })
}

// ------------------------------------------------------------------- audio

/// Moves spotifyd's PCM from the FIFO to whichever decoder is attached. With
/// none attached it stops reading, so spotifyd waits instead of racing ahead.
#[derive(Default)]
struct Pump {
    consumer: Mutex<Option<std::fs::File>>,
    wake: Condvar,
    /// Drop what is buffered before the next decoder hears it.
    flush: AtomicBool,
}

static PUMP: Mutex<Option<std::sync::Weak<Pump>>> = Mutex::new(None);

/// A streamed WAV header: sizes unknown (FFmpeg reads to the end of a pipe).
pub fn wav_header() -> [u8; 44] {
    let mut h = [0u8; 44];
    let byte_rate = RATE * FRAME as u32;
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&2u16.to_le_bytes()); // stereo
    h[24..28].copy_from_slice(&RATE.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&(FRAME as u16).to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    h
}

impl Pump {
    fn run(&self, mut fifo: std::fs::File, stop: &AtomicBool) {
        let mut buffer = vec![0u8; 16384];
        // Bytes of a frame split across reads, carried to the next one.
        let mut carry = 0usize;
        // Bytes still to drop after a flush to reach a frame boundary.
        let mut skip = 0usize;
        while !stop.load(Ordering::SeqCst) {
            let mut consumer = {
                let Ok(mut slot) = self.consumer.lock() else {
                    return;
                };
                while slot.is_none() && !stop.load(Ordering::SeqCst) {
                    slot = match self.wake.wait_timeout(slot, Duration::from_millis(500)) {
                        Ok((slot, _)) => slot,
                        Err(_) => return,
                    };
                }
                match slot.take() {
                    Some(c) => c,
                    None => return,
                }
            };
            if self.flush.swap(false, Ordering::SeqCst) {
                skip = (skip + drain(&fifo, carry)) % FRAME;
                carry = 0;
            }
            // The FIFO never ends (its write end is held here too): wait for
            // sound a little at a time, so stopping is noticed.
            if !readable(&fifo, 250) {
                self.put_back(consumer);
                continue;
            }
            let mut n = match fifo.read(&mut buffer[carry..]) {
                Ok(0) | Err(_) => {
                    std::thread::sleep(Duration::from_millis(20));
                    self.put_back(consumer);
                    continue;
                }
                Ok(n) => n + carry,
            };
            if skip > 0 {
                let dropped = skip.min(n - carry);
                buffer.copy_within(carry + dropped..n, carry);
                n -= dropped;
                skip -= dropped;
            }
            let whole = n - n % FRAME;
            let delivered = consumer.write_all(&buffer[..whole]).is_ok();
            buffer.copy_within(whole..n, 0);
            carry = n - whole;
            if delivered {
                self.put_back(consumer);
            }
            // Else the decoder is gone (EPIPE): wait for the next one.
        }
    }
    fn put_back(&self, consumer: std::fs::File) {
        if let Ok(mut slot) = self.consumer.lock() {
            // A newer decoder attached meanwhile wins.
            if slot.is_none() {
                *slot = Some(consumer);
            }
        }
    }
}

fn readable(fifo: &std::fs::File, timeout_ms: i32) -> bool {
    let mut poll = libc::pollfd {
        fd: fifo.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one pollfd that lives through the call.
    unsafe { libc::poll(&mut poll, 1, timeout_ms) > 0 }
}

/// Read and drop what the FIFO holds now (after `carry` bytes already read);
/// returns how many more bytes to drop to reach a frame boundary.
fn drain(fifo: &std::fs::File, carry: usize) -> usize {
    let fd = fifo.as_raw_fd();
    // SAFETY: fcntl on a descriptor this function borrows.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    let mut dropped = carry;
    let mut scratch = [0u8; 16384];
    let mut reader = fifo;
    while let Ok(n) = reader.read(&mut scratch) {
        if n == 0 {
            break;
        }
        dropped += n;
    }
    // SAFETY: as above, restoring the flags read before.
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    // Keep the frame boundary where it was: the remainder still owed.
    (FRAME - dropped % FRAME) % FRAME
}

/// Spotify's sound for one decoder: a pipe that starts with a WAV header.
/// Dropping it detaches; spotifyd then waits (or is paused).
pub struct Audio {
    read: OwnedFd,
}
impl Audio {
    pub fn attach() -> io::Result<Self> {
        let pump = PUMP
            .lock()
            .ok()
            .and_then(|p| p.as_ref().and_then(std::sync::Weak::upgrade))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "Spotify isn't running"))?;
        let mut fds = [0; 2];
        // SAFETY: pipe2 fills two new descriptors that are owned below.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both descriptors were just created and are not shared.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let mut write = std::fs::File::from(write);
        write.write_all(&wav_header())?;
        if let Ok(mut slot) = pump.consumer.lock() {
            *slot = Some(write);
        }
        pump.wake.notify_all();
        Ok(Self { read })
    }
    /// The read end for `Decoder::open_stream`.
    pub fn reader(&self) -> BorrowedFd<'_> {
        self.read.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("reborn-spotify-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn base64_matches_rfc_4648() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded);
        }
    }

    #[test]
    fn device_code_and_poll_replies() {
        let now = Instant::now();
        let v = serde_json::json!({"device_code":"dc","user_code":"ABCD-EFGH",
            "verification_uri_complete":"https://www.spotify.com/pair?code=ABCD-EFGH",
            "interval":5,"expires_in":600});
        let mut code = parse_code(&v, now).unwrap();
        assert_eq!(code.user_code, "ABCD-EFGH");
        assert_eq!(code.interval, Duration::from_secs(5));
        assert!(!format!("{code:?}").contains("dc\""));
        let pending = serde_json::json!({"error":"authorization_pending"});
        assert!(matches!(
            parse_poll(&pending, &mut code, 0),
            Ok(Poll::Pending)
        ));
        let slow = serde_json::json!({"error":"slow_down"});
        assert!(matches!(parse_poll(&slow, &mut code, 0), Ok(Poll::Pending)));
        assert_eq!(code.interval, Duration::from_secs(10));
        let denied = serde_json::json!({"error":"access_denied"});
        assert_eq!(
            parse_poll(&denied, &mut code, 0).err(),
            Some(Error::Refused("access_denied".into()))
        );
        let ok = serde_json::json!({"access_token":"at","refresh_token":"rt","expires_in":3600});
        match parse_poll(&ok, &mut code, 100) {
            Ok(Poll::Approved(a)) => {
                assert_eq!(
                    (a.access_token.as_str(), a.refresh_token.as_str()),
                    ("at", "rt")
                );
                assert_eq!(a.expires_at, 3700);
                assert!(!format!("{a:?}").contains("rt"));
            }
            _ => panic!("approved"),
        }
        let half = serde_json::json!({"access_token":"at"});
        assert!(parse_poll(&half, &mut code, 0).is_err());
    }

    #[test]
    fn sign_in_writes_private_files_and_forget_removes_them() {
        let root = dir("files");
        let paths = Paths::new(&root);
        let account = Account {
            refresh_token: "rt".into(),
            access_token: "token".into(),
            expires_at: 10,
            name: String::new(),
        };
        sign_in(&paths, &account).unwrap();
        assert_eq!(load_account(&paths).unwrap().refresh_token, "rt");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&paths.account()), 0o600);
        assert_eq!(mode(&paths.oauth()), 0o700);
        let blob: serde_json::Value =
            serde_json::from_slice(&fs::read(paths.oauth().join("credentials.json")).unwrap())
                .unwrap();
        assert_eq!(blob["auth_type"], 3);
        assert_eq!(blob["auth_data"], base64(b"token"));
        assert!(blob["username"].is_null());
        forget(&paths).unwrap();
        assert!(load_account(&paths).is_none());
        assert!(!paths.cache().exists());
        // Forgetting twice is fine.
        forget(&paths).unwrap();
    }

    #[test]
    fn config_quotes_the_name_and_keeps_audio_in_reborn() {
        let paths = Paths::new(Path::new("/data/reborn"));
        let text = config(&paths, "Kai's \"Y2\"\\\n");
        assert!(text.contains("device_name = \"Kai's \\\"Y2\\\"\\\\\"\n"));
        assert!(text.contains("backend = \"pipe\""));
        assert!(text.contains("device = \"/data/reborn/state/spotify/audio.pcm\""));
        assert!(text.contains("volume_controller = \"none\""));
        assert!(text.contains("no_audio_cache = true"));
        assert!(config(&paths, "  ").contains("device_name = \"Y2Reborn\""));
    }

    #[test]
    fn listings_parse_and_arguments_are_checked() {
        let json = br#"{"items":[{"label":"Song - Artist","uri":"spotify:track:abc","id":"alb"},
            {"label":"  "}],"next":"50","context":"spotify:user:me:collection"}"#;
        let l = parse_listing(json).unwrap();
        assert_eq!(l.items.len(), 1);
        assert_eq!(l.items[0].uri.as_deref(), Some("spotify:track:abc"));
        assert_eq!(l.next.as_deref(), Some("50"));
        assert_eq!(l.context.as_deref(), Some("spotify:user:me:collection"));
        assert!(parse_listing(b"nope").is_err());
        let paths = Paths::new(Path::new("/nonexistent"));
        assert_eq!(
            browse(&paths, "rm", "", "", 0).err().as_deref(),
            Some("unknown list")
        );
        assert!(browse(&paths, "playlist-tracks", "../x", "", 0).is_err());
        assert!(browse(&paths, "liked-songs", "", "5; ls", 0).is_err());
    }

    #[test]
    fn mpris_metadata_becomes_a_song() {
        use dbus::arg::{RefArg, Variant};
        let mut m = dbus::arg::PropMap::new();
        let v = |x: Box<dyn RefArg>| Variant(x);
        m.insert("xesam:title".into(), v(Box::new("Song".to_owned())));
        m.insert(
            "xesam:artist".into(),
            v(Box::new(vec!["A".to_owned(), "B".to_owned()])),
        );
        m.insert("xesam:album".into(), v(Box::new("Album".to_owned())));
        m.insert("mpris:length".into(), v(Box::new(215_000_000i64)));
        m.insert(
            "mpris:artUrl".into(),
            v(Box::new("https://i.scdn.co/image/ab".to_owned())),
        );
        m.insert(
            "mpris:trackid".into(),
            v(Box::new(dbus::Path::from("/org/spotify/track/1"))),
        );
        let song = song_of(&m).unwrap();
        assert_eq!(song.title, "Song");
        assert_eq!(song.artist, "A, B");
        assert_eq!(song.album, "Album");
        assert_eq!(song.length_ms, 215_000);
        assert_eq!(song.art_url, "https://i.scdn.co/image/ab");
        assert_eq!(song.id, "/org/spotify/track/1");
        assert!(song_of(&dbus::arg::PropMap::new()).is_none());
    }

    #[test]
    fn position_moves_on_while_playing_and_stops_at_the_end() {
        let mut s = Status {
            playing: true,
            position_ms: 1000,
            read_at: Some(Instant::now() - Duration::from_millis(500)),
            song: Some(Song {
                length_ms: 1200,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(s.position_now(), 1200);
        s.playing = false;
        assert_eq!(s.position_now(), 1000);
    }

    /// `n` bytes, failing the test instead of hanging when they don't come.
    fn read_within(mut file: &std::fs::File, n: usize) -> Vec<u8> {
        let mut got = vec![];
        let deadline = Instant::now() + Duration::from_secs(5);
        while got.len() < n {
            assert!(Instant::now() < deadline, "only {} of {n} bytes", got.len());
            if readable(file, 100) {
                let mut buf = vec![0u8; n - got.len()];
                let k = file.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..k]);
            }
        }
        got
    }

    /// Both tests below use the one live pump.
    static PUMP_TESTS: Mutex<()> = Mutex::new(());

    #[test]
    fn a_started_daemon_feeds_its_sound_to_an_attached_decoder() {
        let _one = PUMP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let root = dir("daemon");
        // A stand-in spotifyd: writes two frames into the FIFO beside its
        // configuration (`--no-daemon --config-path <file>`), then waits.
        let fake = root.join("spotifyd");
        fs::write(
            &fake,
            "#!/bin/sh\nprintf abcdefgh > \"$(dirname \"$3\")/audio.pcm\"\nexec sleep 30\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("REBORN_SPOTIFYD", &fake);
        let paths = Paths::new(&root);
        let daemon = Daemon::start(&paths, "Test").unwrap();
        assert!(fs::read_to_string(paths.config())
            .unwrap()
            .contains("device_name = \"Test\""));
        let audio = Audio::attach().unwrap();
        let file = std::fs::File::from(audio.read.try_clone().unwrap());
        let got = read_within(&file, 44 + 8);
        assert_eq!(&got[44..], b"abcdefgh");
        assert_eq!(daemon.status().phase, Phase::Starting);
        let pid = daemon
            .shared
            .child
            .lock()
            .unwrap()
            .as_ref()
            .map(Child::id)
            .unwrap();
        drop(file);
        drop(audio);
        drop(daemon);
        // Stopped with the daemon, and nothing attaches any more.
        // SAFETY: probing a pid with signal 0.
        assert_ne!(unsafe { libc::kill(pid as libc::pid_t, 0) }, 0);
        assert!(Audio::attach().is_err());
    }

    #[test]
    fn pump_frames_audio_as_wav_and_waits_without_a_decoder() {
        let _one = PUMP_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let root = dir("pump");
        let fifo = root.join("audio.pcm");
        make_fifo(&fifo).unwrap();
        make_fifo(&fifo).unwrap(); // again: kept
        let reader = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&fifo)
            .unwrap();
        let mut writer = OpenOptions::new().write(true).open(&fifo).unwrap();
        let pump = Arc::new(Pump::default());
        *PUMP.lock().unwrap() = Some(Arc::downgrade(&pump));
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (pump.clone(), stop.clone());
        let worker = std::thread::spawn(move || p.run(reader, &s));
        // Stale sound from before a new song is dropped on request.
        writer.write_all(&[9u8; 8]).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        pump.flush.store(true, Ordering::SeqCst);
        let audio = Audio::attach().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        writer.write_all(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let file = std::fs::File::from(audio.read.try_clone().unwrap());
        let got = read_within(&file, 44 + 8);
        assert_eq!(&got[..44], &wav_header());
        assert_eq!(&got[44..], &[1, 2, 3, 4, 5, 6, 7, 8]);
        stop.store(true, Ordering::SeqCst);
        pump.wake.notify_all();
        drop(file);
        drop(audio);
        worker.join().unwrap();
    }

    #[test]
    fn wav_header_describes_cd_audio() {
        let h = wav_header();
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(h[24..28].try_into().unwrap()), 44_100);
        assert_eq!(u16::from_le_bytes(h[22..24].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(h[34..36].try_into().unwrap()), 16);
    }
}
