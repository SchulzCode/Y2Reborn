//! Audiobookshelf server addresses and the records needed to browse books.
//!
//! A server can be hosted below a reverse proxy path, so API routes must be
//! appended to that path rather than resolved from the domain root.
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

pub mod staging;
pub mod worker;

/// A configured HTTPS server, including any reverse proxy path.
#[derive(Clone, PartialEq, Eq)]
pub struct Server {
    base: String,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Server { .. }")
    }
}

impl Server {
    /// Reject ambiguous addresses before they can be used with credentials.
    pub fn new(address: &str) -> Result<Self, &'static str> {
        let address = address.trim().trim_end_matches('/');
        let Some(rest) = address.strip_prefix("https://") else {
            return Err("Audiobookshelf requires an HTTPS address");
        };
        let host = rest.split('/').next().unwrap_or("");
        if host.is_empty()
            || host.contains('@')
            || address.chars().any(|c| c.is_control() || c.is_whitespace())
            || address.contains(['?', '#', '\\', '%'])
            || rest
                .split('/')
                .skip(1)
                .any(|part| part == "." || part == "..")
        {
            return Err("Invalid Audiobookshelf server address");
        }
        Ok(Self {
            base: address.to_owned(),
        })
    }

    pub fn libraries(&self) -> String {
        format!("{}/api/libraries", self.base)
    }

    pub fn refresh(&self) -> String {
        format!("{}/auth/refresh", self.base)
    }

    pub fn login(&self) -> String {
        format!("{}/login", self.base)
    }

    pub fn logout(&self) -> String {
        format!("{}/logout", self.base)
    }

    pub fn items(&self, library: &str, page: u32) -> Result<String, &'static str> {
        Ok(format!(
            "{}/api/libraries/{}/items?limit=50&page={page}&sort=media.metadata.title",
            self.base,
            identifier(library)?
        ))
    }

    pub fn item(&self, item: &str) -> Result<String, &'static str> {
        Ok(format!(
            "{}/api/items/{}?expanded=1",
            self.base,
            identifier(item)?
        ))
    }

    pub fn search(&self, library: &str, query: &str) -> Result<String, &'static str> {
        Ok(format!(
            "{}/api/libraries/{}/search?q={}",
            self.base,
            identifier(library)?,
            encode_query(query)
        ))
    }

    pub fn cover(&self, item: &str) -> Result<String, &'static str> {
        Ok(format!(
            "{}/api/items/{}/cover",
            self.base,
            identifier(item)?
        ))
    }

    pub fn file(&self, item: &str, inode: &str) -> Result<String, &'static str> {
        Ok(format!(
            "{}/api/items/{}/file/{}/download",
            self.base,
            identifier(item)?,
            identifier(inode)?
        ))
    }

    /// A stable local identity without putting the server address in filenames.
    pub fn book_key(&self, item: &str) -> Result<String, &'static str> {
        identifier(item)?;
        let source = crate::lastfm::hex(&crate::lastfm::md5(self.base.as_bytes()));
        let book = crate::lastfm::hex(&crate::lastfm::md5(item.as_bytes()));
        Ok(format!("abs-{source}-{book}"))
    }
}

fn identifier(value: &str) -> Result<&str, &'static str> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid Audiobookshelf identifier");
    }
    Ok(value)
}

fn encode_query(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// A bearer token is never placed in curl's arguments or printed by Debug.
pub struct Client {
    pub server: Server,
    token: String,
    curl: String,
}

impl Client {
    /// Exchange credentials for the server's revocable session.
    pub fn login(
        server: Server,
        username: &str,
        password: &str,
    ) -> Result<AuthSession, &'static str> {
        let client = Self {
            server,
            token: String::new(),
            curl: "/usr/bin/curl".into(),
        };
        client.login_inner(username, password)
    }

    pub fn new(server: Server, token: String) -> Result<Self, &'static str> {
        if !valid_token(&token) {
            return Err("Invalid Audiobookshelf session");
        }
        Ok(Self {
            server,
            token,
            curl: "/usr/bin/curl".into(),
        })
    }

    pub fn libraries(&self) -> Result<Libraries, &'static str> {
        self.get(&self.server.libraries())
    }

    pub fn items(&self, library: &str, page: u32) -> Result<BookPage, &'static str> {
        self.get(&self.server.items(library, page)?)
    }

    pub fn item(&self, item: &str) -> Result<Book, &'static str> {
        self.get(&self.server.item(item)?)
    }

    pub fn cover(&self, item: &str) -> Result<Vec<u8>, &'static str> {
        self.request(&self.server.cover(item)?)
    }

    pub fn search(&self, library: &str, query: &str) -> Result<SearchResults, &'static str> {
        self.get(&self.server.search(library, query)?)
    }

    /// Refresh tokens rotate; the caller must persist the new pair before use.
    pub fn refresh(&self, refresh_token: &str) -> Result<AuthSession, &'static str> {
        if !valid_token(refresh_token) {
            return Err("Invalid Audiobookshelf session");
        }
        let config = format!("header = \"x-refresh-token: {refresh_token}\"\n");
        let body = self.request_config(&self.server.refresh(), &config, true)?;
        parse_session(&body)
    }

    pub fn logout(&self, refresh_token: &str) -> Result<(), &'static str> {
        if !valid_token(refresh_token) {
            return Err("Invalid Audiobookshelf session");
        }
        let config = format!("header = \"x-refresh-token: {refresh_token}\"\n");
        self.request_config(&self.server.logout(), &config, true)?;
        Ok(())
    }

    fn login_inner(&self, username: &str, password: &str) -> Result<AuthSession, &'static str> {
        if username.is_empty() || password.is_empty() {
            return Err("Enter an Audiobookshelf username and password");
        }
        let body = serde_json::to_string(&Login { username, password })
            .map_err(|_| "Couldn't prepare the sign-in request")?;
        let encoded = body.replace('\\', "\\\\").replace('"', "\\\"");
        let config = format!(
            "header = \"Content-Type: application/json\"\n\
             header = \"x-return-tokens: true\"\n\
             data-raw = \"{encoded}\"\n"
        );
        let body = self
            .request_config(&self.server.login(), &config, true)
            .map_err(|error| {
                if error == "Sign in to Audiobookshelf again" {
                    "Sign-in rejected; check username and password"
                } else {
                    error
                }
            })?;
        parse_session(&body)
    }

    /// Stream one complete file into a staging writer. The caller publishes
    /// nothing until every file has passed its size check.
    pub fn download_file<W: Write>(
        &self,
        item: &str,
        file: &AudioFile,
        output: &mut W,
        mut progress: impl FnMut(u64),
    ) -> Result<(), &'static str> {
        let url = self.server.file(item, &file.ino)?;
        if file.metadata.size == 0 {
            return Err("Audiobookshelf didn't provide a file size");
        }
        let mut child = Command::new(&self.curl)
            .args([
                "-q",
                "--silent",
                "--fail",
                "--proto",
                "=https",
                "--tlsv1.2",
                "--connect-timeout",
                "10",
                "--speed-limit",
                "1",
                "--speed-time",
                "30",
                "--config",
                "-",
                "--write-out",
                "%{stderr}%{http_code}",
                "--",
                &url,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| "Couldn't start the download")?;
        let config = format!("header = \"Authorization: Bearer {}\"\n", self.token);
        let sent = child
            .stdin
            .take()
            .is_some_and(|mut input| input.write_all(config.as_bytes()).is_ok());
        if !sent {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Couldn't start the download");
        }
        let copied = match child.stdout.take() {
            Some(input) => copy_download(input, output, file.metadata.size, &mut progress),
            None => Err("Couldn't read the download"),
        };
        if copied.is_err() {
            let _ = child.kill();
        }
        let result = child.wait_with_output().map_err(|_| "Download failed")?;
        match result.stderr.as_slice() {
            b"401" => return Err("Sign in to Audiobookshelf again"),
            b"403" => return Err("Audiobookshelf download permission denied"),
            b"404" => return Err("Audiobookshelf audio file not found"),
            _ => {}
        }
        copied?;
        if !result.status.success() || result.stderr != b"200" {
            return Err("Download failed; check the connection and try again");
        }
        Ok(())
    }

    /// Download each included audio file into private staging and publish the
    /// directory only after every file passes verification.
    pub fn download_book(
        &self,
        root: &Path,
        book: &Book,
        supported: impl Fn(&Path) -> bool,
        progress: impl FnMut(u64, u64),
    ) -> Result<(), String> {
        self.download_book_verified(root, None, book, supported, progress)
    }

    pub fn download_book_verified(
        &self,
        root: &Path,
        expected_mount: Option<u64>,
        book: &Book,
        supported: impl Fn(&Path) -> bool,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<(), String> {
        let plan = book.download_plan(supported).map_err(str::to_owned)?;
        let total = plan.bytes;
        let key = self.server.book_key(&book.id).map_err(str::to_owned)?;
        let stage = staging::Stage::begin_verified(
            root,
            expected_mount,
            &key,
            &book.media.metadata.title,
            plan,
        )?;
        if let Ok(cover) = self.cover(&book.id) {
            stage.save_cover(&cover)?;
        }
        let mut parts: Vec<_> = book
            .media
            .audio_files
            .iter()
            .filter(|file| !file.exclude)
            .collect();
        parts.sort_by_key(|file| file.index);
        let mut completed = 0u64;
        for file in parts {
            let mut output = stage.start_part(file.index)?;
            self.download_file(&book.id, file, &mut output, |bytes| {
                progress(completed.saturating_add(bytes), total);
            })
            .map_err(str::to_owned)?;
            drop(output);
            stage.finish_part(file.index)?;
            completed = completed.saturating_add(file.metadata.size);
        }
        stage.publish()
    }

    fn get<T: DeserializeOwned>(&self, url: &str) -> Result<T, &'static str> {
        let body = self.request(url)?;
        serde_json::from_slice(&body).map_err(|_| "Invalid response from Audiobookshelf")
    }

    fn request(&self, url: &str) -> Result<Vec<u8>, &'static str> {
        let config = format!("header = \"Authorization: Bearer {}\"\n", self.token);
        self.request_config(url, &config, false)
    }

    fn request_config(&self, url: &str, config: &str, post: bool) -> Result<Vec<u8>, &'static str> {
        const MAX_RESPONSE: u64 = 4 * 1024 * 1024;
        let mut command = Command::new(&self.curl);
        command.args([
            "-q",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--connect-timeout",
            "10",
            "--max-time",
            "30",
            "--config",
            "-",
            "--write-out",
            "\\n%{http_code}",
        ]);
        if post {
            command.args(["--request", "POST"]);
        }
        let mut child = command
            .args(["--", url])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "Couldn't start the Audiobookshelf request")?;
        let input = child
            .stdin
            .take()
            .ok_or("Couldn't start the Audiobookshelf request")?;
        if (&input).write_all(config.as_bytes()).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Couldn't start the Audiobookshelf request");
        }
        drop(input);
        let mut response = Vec::new();
        let read = child
            .stdout
            .take()
            .ok_or("Couldn't read the Audiobookshelf response")?
            .take(MAX_RESPONSE + 1)
            .read_to_end(&mut response);
        if read.is_err() || response.len() as u64 > MAX_RESPONSE {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Audiobookshelf response is too large or unreadable");
        }
        let status = child.wait().map_err(|_| "Couldn't reach Audiobookshelf")?;
        if !status.success() {
            return Err("Couldn't reach Audiobookshelf");
        }
        let split = response
            .iter()
            .rposition(|b| *b == b'\n')
            .ok_or("Invalid response from Audiobookshelf")?;
        let http = &response[split + 1..];
        match http {
            b"200" => response.truncate(split),
            b"401" => return Err("Sign in to Audiobookshelf again"),
            b"403" => return Err("Audiobookshelf access denied"),
            b"404" => return Err("Audiobookshelf item or library not found"),
            _ => return Err("Audiobookshelf couldn't complete the request"),
        }
        Ok(response)
    }
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
}

/// Authentication secrets are intentionally not printable with `Debug`.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSession {
    access_token: String,
    refresh_token: String,
}

impl AuthSession {
    pub fn new(access_token: String, refresh_token: String) -> Result<Self, &'static str> {
        if !valid_token(&access_token) || !valid_token(&refresh_token) {
            return Err("Invalid Audiobookshelf session");
        }
        Ok(Self {
            access_token,
            refresh_token,
        })
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn refresh_token(&self) -> &str {
        &self.refresh_token
    }

    /// Keep the rotating session in the private Reborn state directory.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        save_private(path, self)
    }

    pub fn load(path: &Path) -> Option<Self> {
        let session: Self = load_private(path)?;
        Self::new(session.access_token, session.refresh_token).ok()
    }
}

/// Server and rotating tokens are saved together, so a crash cannot pair a
/// token from one account with a different server.
pub struct Account {
    pub server: Server,
    session: AuthSession,
    pub username: Option<String>,
}

#[derive(serde::Serialize, Deserialize)]
struct StoredAccount {
    server: String,
    session: AuthSession,
    #[serde(default)]
    username: Option<String>,
}

impl Account {
    pub fn new(server: Server, session: AuthSession) -> Self {
        Self {
            server,
            session,
            username: None,
        }
    }

    pub fn load(path: &Path) -> Option<Self> {
        let saved: StoredAccount = load_private(path)?;
        let server = Server::new(&saved.server).ok()?;
        let session =
            AuthSession::new(saved.session.access_token, saved.session.refresh_token).ok()?;
        Some(Self {
            server,
            session,
            username: saved.username,
        })
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        #[derive(serde::Serialize)]
        struct AccountRef<'a> {
            server: &'a str,
            session: &'a AuthSession,
            username: &'a Option<String>,
        }
        save_private(
            path,
            &AccountRef {
                server: &self.server.base,
                session: &self.session,
                username: &self.username,
            },
        )
    }

    pub fn client(&self) -> Result<Client, &'static str> {
        Client::new(self.server.clone(), self.session.access_token.clone())
    }

    /// Hold the new token in memory even if saving fails: the server may
    /// already have invalidated the old refresh token.
    pub fn refresh(&mut self, path: &Path) -> Result<(), &'static str> {
        let next = self.client()?.refresh(self.session.refresh_token())?;
        self.session = next;
        self.save(path)
            .map_err(|_| "Couldn't save the Audiobookshelf session")
    }
}

fn save_private(path: &Path, value: &impl serde::Serialize) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no state directory"))?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".audiobookshelf-{:016x}.tmp", crate::random_u64()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| {
        serde_json::to_writer(&mut file, value).map_err(std::io::Error::other)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        std::fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn load_private<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

#[derive(Deserialize)]
struct AuthReply {
    user: AuthSession,
}

#[derive(serde::Serialize)]
struct Login<'a> {
    username: &'a str,
    password: &'a str,
}

fn parse_session(body: &[u8]) -> Result<AuthSession, &'static str> {
    let reply: AuthReply =
        serde_json::from_slice(body).map_err(|_| "Invalid response from Audiobookshelf")?;
    AuthSession::new(reply.user.access_token, reply.user.refresh_token)
}

fn copy_download(
    mut input: impl Read,
    output: &mut impl Write,
    expected: u64,
    progress: &mut impl FnMut(u64),
) -> Result<(), &'static str> {
    let mut buffer = [0u8; 32 * 1024];
    let mut copied = 0u64;
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|_| "Couldn't read the download")?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(read as u64)
            .ok_or("Download is too large")?;
        if copied > expected {
            return Err("Downloaded file is larger than expected");
        }
        output.write_all(&buffer[..read]).map_err(|e| {
            if e.kind() == std::io::ErrorKind::StorageFull {
                "Not enough storage for this book"
            } else {
                "Couldn't write the downloaded file"
            }
        })?;
        progress(copied);
    }
    if copied != expected {
        return Err("Download ended before the file was complete");
    }
    Ok(())
}

/// Only the fields Reborn uses are retained; server paths stay out of state.
#[derive(Debug, Deserialize)]
pub struct Libraries {
    pub libraries: Vec<Library>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    pub id: String,
    pub name: String,
    pub media_type: String,
}

#[derive(Debug, Deserialize)]
pub struct BookPage {
    pub results: Vec<Book>,
    pub total: u64,
    pub page: u64,
}

#[derive(Debug, Deserialize)]
pub struct SearchResults {
    #[serde(default)]
    pub book: Vec<SearchBook>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchBook {
    pub library_item: Book,
}

#[derive(Debug, Deserialize)]
pub struct Book {
    pub id: String,
    pub media: BookMedia,
}

impl Book {
    /// Required bytes for the server's included audio files, before allowing
    /// a download to begin. Missing or repeated identifiers are not safe to
    /// treat as a complete book.
    pub fn download_size(&self) -> Result<u64, &'static str> {
        let mut size = 0u64;
        let mut inodes = std::collections::HashSet::new();
        let mut indexes = std::collections::HashSet::new();
        for file in self.media.audio_files.iter().filter(|file| !file.exclude) {
            identifier(&file.ino)?;
            if file.metadata.size == 0 || !inodes.insert(&file.ino) || !indexes.insert(file.index) {
                return Err("Audiobookshelf returned incomplete audio file details");
            }
            size = size
                .checked_add(file.metadata.size)
                .ok_or("Book is too large")?;
        }
        if size == 0 {
            return Err("This book has no downloadable audio files");
        }
        Ok(size)
    }

    /// Build names from the server's order, never from server-provided paths.
    /// The scanner's own format predicate keeps both components in agreement.
    pub fn download_plan(
        &self,
        supported: impl Fn(&Path) -> bool,
    ) -> Result<DownloadPlan, &'static str> {
        let bytes = self.download_size()?;
        let mut files: Vec<_> = self
            .media
            .audio_files
            .iter()
            .filter(|file| !file.exclude)
            .collect();
        files.sort_by_key(|file| file.index);
        let numbered = files
            .iter()
            .all(|file| file.track_num_from_meta.is_some_and(|track| track > 0));
        for pair in files.windows(2) {
            let before = (
                pair[0].disc_num_from_meta.unwrap_or(0),
                if numbered {
                    pair[0].track_num_from_meta.unwrap_or(0)
                } else {
                    0
                },
            );
            let after = (
                pair[1].disc_num_from_meta.unwrap_or(0),
                if numbered {
                    pair[1].track_num_from_meta.unwrap_or(0)
                } else {
                    0
                },
            );
            if before > after {
                return Err("Book parts have conflicting playback order");
            }
        }
        let mut parts = Vec::with_capacity(files.len());
        for file in &files {
            let ext = Path::new(&file.metadata.filename)
                .extension()
                .and_then(|value| value.to_str())
                .filter(|value| {
                    !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric())
                })
                .ok_or("Audiobookshelf audio file has no supported extension")?
                .to_ascii_lowercase();
            let name = format!("{:010}.{ext}", file.index);
            if !supported(Path::new(&name)) {
                return Err("This audiobook contains an unsupported audio format");
            }
            parts.push(DownloadPart {
                index: file.index,
                inode: file.ino.clone(),
                bytes: file.metadata.size,
                name,
            });
        }
        let m4b = parts
            .iter()
            .filter(|part| part.name.ends_with(".m4b"))
            .count();
        if m4b > 0 && parts.len() > 1 {
            let album = files
                .first()
                .and_then(|file| file.album_tag())
                .unwrap_or("");
            let album = reborn_core::sort::fold(album);
            if m4b != parts.len()
                || album.is_empty()
                || !files.iter().all(|file| {
                    file.album_tag()
                        .is_some_and(|tag| reborn_core::sort::fold(tag) == album)
                })
            {
                return Err("M4B parts need matching album tags to play as one book");
            }
        }
        if m4b == 1 {
            parts[0].name = format!("{}.m4b", staging::book_folder(&self.media.metadata.title));
            if !supported(Path::new(&parts[0].name)) {
                return Err("This audiobook contains an unsupported audio format");
            }
        }
        Ok(DownloadPlan { bytes, parts })
    }
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct DownloadPlan {
    pub bytes: u64,
    pub parts: Vec<DownloadPart>,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct DownloadPart {
    pub index: u32,
    pub inode: String,
    pub bytes: u64,
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookMedia {
    pub metadata: BookMetadata,
    #[serde(default)]
    pub audio_files: Vec<AudioFile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BookMetadata {
    pub title: String,
    #[serde(default)]
    pub author_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioFile {
    pub index: u32,
    pub ino: String,
    pub metadata: FileMetadata,
    #[serde(default)]
    pub exclude: bool,
    #[serde(default)]
    pub disc_num_from_meta: Option<u32>,
    #[serde(default)]
    pub track_num_from_meta: Option<u32>,
    #[serde(default)]
    pub meta_tags: Option<AudioTags>,
}

impl AudioFile {
    fn album_tag(&self) -> Option<&str> {
        self.meta_tags.as_ref()?.tag_album.as_deref()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTags {
    #[serde(default)]
    pub tag_album: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FileMetadata {
    pub filename: String,
    pub size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mock_program(root: &Path, name: &str, body: &str) -> String {
        let path = root.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path.to_string_lossy().into()
    }

    #[test]
    fn routes_keep_the_proxy_prefix_and_reject_path_injection() {
        let server = Server::new("https://books.example.test/audiobookshelf/").unwrap();
        assert_eq!(
            server.libraries(),
            "https://books.example.test/audiobookshelf/api/libraries"
        );
        assert_eq!(
            server.file("item-1", "123").unwrap(),
            "https://books.example.test/audiobookshelf/api/items/item-1/file/123/download"
        );
        assert!(server.file("../other", "123").is_err());
        assert!(server.item("a?token=secret").is_err());
        assert_eq!(
            server.search("library-1", "Jane & 李").unwrap(),
            concat!(
                "https://books.example.test/audiobookshelf/api/libraries/",
                "library-1/search?q=Jane%20%26%20%E6%9D%8E"
            )
        );
    }

    #[test]
    fn rejects_addresses_that_could_redirect_credentials() {
        for address in [
            "http://books.example.test",
            "https://user@books.example.test",
            "https://books.example.test/../other",
            "https://books.example.test/%2e%2e/other",
            "https://books.example.test/?token=secret",
            "https://books.example.test/#fragment",
            "https://books.example.test/\nheader",
        ] {
            assert!(Server::new(address).is_err(), "accepted {address:?}");
        }
    }

    #[test]
    fn reads_a_book_with_multiple_audio_files() {
        let item: Book = serde_json::from_str(
            r#"{"id":"book-1","media":{"metadata":{"title":"A Book","authorName":"A Writer"},
                "audioFiles":[{"index":1,"ino":"123","metadata":{"filename":"01.mp3","size":24}},
                {"index":2,"ino":"456","metadata":{"filename":"02.mp3","size":42},
                "exclude":true}]}}"#,
        )
        .unwrap();
        assert_eq!(item.media.metadata.title, "A Book");
        assert_eq!(item.media.audio_files.len(), 2);
        assert!(item.media.audio_files[1].exclude);
        assert_eq!(item.media.audio_files[0].metadata.size, 24);
        assert_eq!(item.download_size(), Ok(24));
    }

    #[test]
    fn reads_search_results_without_download_details() {
        let search: SearchResults = serde_json::from_str(
            r#"{"book":[{"libraryItem":{"id":"book-1","media":{"metadata":
                {"title":"A Book","authorName":"A Writer"}}}}]}"#,
        )
        .unwrap();
        assert_eq!(search.book[0].library_item.id, "book-1");
        assert_eq!(
            search.book[0]
                .library_item
                .media
                .metadata
                .author_name
                .as_deref(),
            Some("A Writer")
        );
        assert!(search.book[0].library_item.media.audio_files.is_empty());
    }

    #[test]
    fn refuses_incomplete_or_duplicate_file_plans() {
        let mut item: Book = serde_json::from_str(
            r#"{"id":"book-1","media":{"metadata":{"title":"A Book"},"audioFiles":[
                {"index":1,"ino":"123","metadata":{"filename":"01.mp3","size":5}},
                {"index":2,"ino":"456","metadata":{"filename":"02.mp3","size":7}}]}}"#,
        )
        .unwrap();
        assert_eq!(item.download_size(), Ok(12));
        item.media.audio_files[1].ino = "123".into();
        assert!(item.download_size().is_err());
        item.media.audio_files[1].ino = "456".into();
        item.media.audio_files[1].metadata.size = 0;
        assert!(item.download_size().is_err());
        item.media.audio_files[1].metadata.size = u64::MAX;
        assert!(item.download_size().is_err());
    }

    #[test]
    fn download_plan_uses_safe_names_and_refuses_split_m4b_books() {
        let mut item: Book = serde_json::from_str(
            r#"{"id":"book-1","media":{"metadata":{"title":"A Book"},"audioFiles":[
                {"index":2,"ino":"456","metadata":{"filename":"../../second.MP3","size":7}},
                {"index":1,"ino":"123","metadata":{"filename":"first.mp3","size":5}}]}}"#,
        )
        .unwrap();
        let plan = item
            .download_plan(|path| path.extension().is_some())
            .unwrap();
        assert_eq!(plan.bytes, 12);
        assert_eq!(plan.parts[0].name, "0000000001.mp3");
        assert_eq!(plan.parts[1].name, "0000000002.mp3");
        assert!(item.download_plan(|_| false).is_err());
        item.media.audio_files[0].track_num_from_meta = Some(1);
        item.media.audio_files[1].track_num_from_meta = Some(2);
        assert!(item.download_plan(|_| true).is_err());
        item.media.audio_files[0].track_num_from_meta = None;
        item.media.audio_files[1].track_num_from_meta = None;
        item.media.audio_files[0].metadata.filename = "second.m4b".into();
        assert!(item.download_plan(|_| true).is_err());
    }

    #[test]
    fn a_single_m4b_keeps_a_readable_title_without_file_tags() {
        let book: Book = serde_json::from_str(
            r#"{"id":"book-1","media":{"metadata":{"title":"../A Book"},"audioFiles":[
                {"index":1,"ino":"123","metadata":{"filename":"original.m4b","size":3}}]}}"#,
        )
        .unwrap();
        let plan = book.download_plan(|_| true).unwrap();
        assert_eq!(plan.parts[0].name, "A Book.m4b");
    }

    #[test]
    fn multipart_m4b_requires_one_shared_album() {
        let mut book: Book = serde_json::from_str(
            r#"{"id":"book-1","media":{"metadata":{"title":"A Book"},"audioFiles":[
                {"index":1,"ino":"123","metadata":{"filename":"01.m4b","size":3},
                 "metaTags":{"tagAlbum":"A Book"}},
                {"index":2,"ino":"456","metadata":{"filename":"02.m4b","size":4},
                 "metaTags":{"tagAlbum":"A Book"}}]}}"#,
        )
        .unwrap();
        assert_eq!(book.download_plan(|_| true).unwrap().parts.len(), 2);
        book.media.audio_files[1]
            .meta_tags
            .as_mut()
            .unwrap()
            .tag_album = Some("Other".into());
        assert!(book.download_plan(|_| true).is_err());
    }

    #[test]
    fn mock_download_publishes_all_parts_and_rejects_duplicates() {
        let root = std::env::temp_dir().join(format!("reborn-abs-book-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("curl");
        std::fs::write(
            &script,
            concat!(
                "#!/bin/sh\ncat >/dev/null\n",
                "case \"$*\" in */file/123/download*) printf one;; ",
                "*/file/456/download*) printf 'two!';; *) exit 1;; esac\n",
                "printf 200 >&2\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let book: Book = serde_json::from_str(
            r#"{"id":"book-1","media":{"metadata":{"title":"A Book"},"audioFiles":[
                {"index":1,"ino":"123","metadata":{"filename":"01.mp3","size":3}},
                {"index":2,"ino":"456","metadata":{"filename":"02.mp3","size":4}}]}}"#,
        )
        .unwrap();
        let mut client = Client::new(
            Server::new("https://books.example.test/proxy").unwrap(),
            "test-token".into(),
        )
        .unwrap();
        client.curl = script.to_string_lossy().into();
        let mut progress = Vec::new();
        client
            .download_book(
                &root,
                &book,
                |_| true,
                |done, total| progress.push((done, total)),
            )
            .unwrap();
        let destination = root
            .join("Audiobooks/Audiobookshelf")
            .join(client.server.book_key(&book.id).unwrap());
        assert_eq!(
            std::fs::read(destination.join("A Book/0000000001.mp3")).unwrap(),
            b"one"
        );
        assert_eq!(
            std::fs::read(destination.join("A Book/0000000002.mp3")).unwrap(),
            b"two!"
        );
        assert_eq!(progress.last(), Some(&(7, 7)));
        assert!(client
            .download_book(&root, &book, |_| true, |_, _| {})
            .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mock_download_restarts_after_a_failed_part() {
        let root = std::env::temp_dir().join(format!("reborn-abs-retry-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("curl");
        let failed = concat!(
            "#!/bin/sh\ncat >/dev/null\n",
            "case \"$*\" in */file/123/download*) printf one; printf 200 >&2;; ",
            "*) printf 403 >&2; exit 22;; esac\n"
        );
        std::fs::write(&script, failed).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let book: Book = serde_json::from_str(
            r#"{"id":"book-1","media":{"metadata":{"title":"A Book"},"audioFiles":[
                {"index":1,"ino":"123","metadata":{"filename":"01.mp3","size":3}},
                {"index":2,"ino":"456","metadata":{"filename":"02.mp3","size":4}}]}}"#,
        )
        .unwrap();
        let mut client = Client::new(
            Server::new("https://books.example.test/proxy").unwrap(),
            "test-token".into(),
        )
        .unwrap();
        client.curl = script.to_string_lossy().into();
        assert!(client
            .download_book(&root, &book, |_| true, |_, _| {})
            .is_err());
        let destination = root
            .join("Audiobooks/Audiobookshelf")
            .join(client.server.book_key(&book.id).unwrap());
        assert!(!destination.exists());
        let succeeds = concat!(
            "#!/bin/sh\ncat >/dev/null\n",
            "case \"$*\" in */file/123/download*) printf one;; ",
            "*/file/456/download*) printf 'two!';; *) exit 1;; esac\n",
            "printf 200 >&2\n"
        );
        client.curl = mock_program(&root, "curl-retry", succeeds);
        client
            .download_book(&root, &book, |_| true, |_, _| {})
            .unwrap();
        assert_eq!(
            std::fs::read(destination.join("A Book/0000000002.mp3")).unwrap(),
            b"two!"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refresh_and_private_session_storage() {
        let root = std::env::temp_dir().join(format!("reborn-abs-session-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let session = AuthSession::new("access-token".into(), "refresh-token".into()).unwrap();
        let path = root.join("state.json");
        session.save(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let loaded = AuthSession::load(&path).unwrap();
        assert_eq!(loaded.refresh_token(), "refresh-token");
        let script = root.join("curl");
        let input = root.join("input");
        std::fs::write(
            &script,
            format!(
                concat!(
                    "#!/bin/sh\ncat > '{}'\n",
                    "printf '{{\"user\":{{\"accessToken\":\"new-access\",",
                    "\"refreshToken\":\"new-refresh\"}}}}\\n200'\n"
                ),
                input.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut client = Client::new(
            Server::new("https://books.example.test/proxy").unwrap(),
            loaded.access_token().into(),
        )
        .unwrap();
        client.curl = script.to_string_lossy().into();
        let next = client.refresh(loaded.refresh_token()).unwrap();
        assert_eq!(next.access_token(), "new-access");
        assert_eq!(next.refresh_token(), "new-refresh");
        assert!(std::fs::read_to_string(input)
            .unwrap()
            .contains("x-refresh-token: refresh-token"));
        let signed_in = client.login_inner("reader", "a\"b\\c\n").unwrap();
        assert_eq!(signed_in.access_token(), "new-access");
        next.save(&path).unwrap();
        assert_eq!(
            AuthSession::load(&path).unwrap().refresh_token(),
            "new-refresh"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn account_keeps_server_and_session_together_in_a_private_file() {
        let root = std::env::temp_dir().join(format!("reborn-abs-account-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("account.json");
        let account = Account::new(
            Server::new("https://books.example.test/prefix").unwrap(),
            AuthSession::new("access-token".into(), "refresh-token".into()).unwrap(),
        );
        account.save(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let loaded = Account::load(&path).unwrap();
        assert_eq!(
            loaded.server.libraries(),
            "https://books.example.test/prefix/api/libraries"
        );
        assert_eq!(loaded.client().unwrap().token, "access-token");
        assert!(!format!("{:?}", loaded.server).contains("example"));
        std::fs::write(
            &path,
            concat!(
                r#"{"server":"https://books.example.test","session":{"#,
                r#""accessToken":"access","refreshToken":"refresh"}}"#
            ),
        )
        .unwrap();
        assert!(Account::load(&path).unwrap().username.is_none());
        std::fs::write(&path, r#"{"server":"http://example.test","session":{}}"#).unwrap();
        assert!(Account::load(&path).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn request_keeps_the_token_out_of_arguments_and_reports_auth_errors() {
        let root = std::env::temp_dir().join(format!("reborn-abs-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let program = root.join("curl");
        let args = root.join("args");
        let input = root.join("input");
        std::fs::write(
            &program,
            format!(
                concat!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                    "cat > '{}'\nprintf '{{\"libraries\":[]}}\\n200'\n"
                ),
                args.display(),
                input.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut client = Client::new(
            Server::new("https://books.example.test").unwrap(),
            "test-token".into(),
        )
        .unwrap();
        client.curl = program.to_string_lossy().into();
        assert!(client.libraries().unwrap().libraries.is_empty());
        assert!(!std::fs::read_to_string(args)
            .unwrap()
            .contains("test-token"));
        assert!(std::fs::read_to_string(input)
            .unwrap()
            .contains("Bearer test-token"));
        client.curl = mock_program(
            &root,
            "curl-unauthorized",
            "#!/bin/sh\ncat >/dev/null\nprintf '{}\\n401'\n",
        );
        assert_eq!(
            client.libraries().unwrap_err(),
            "Sign in to Audiobookshelf again"
        );
        let file = AudioFile {
            index: 1,
            ino: "123".into(),
            metadata: FileMetadata {
                filename: "01.mp3".into(),
                size: 5,
            },
            exclude: false,
            disc_num_from_meta: None,
            track_num_from_meta: None,
            meta_tags: None,
        };
        client.curl = mock_program(
            &root,
            "curl-success",
            "#!/bin/sh\ncat >/dev/null\nprintf sound\nprintf 200 >&2\n",
        );
        let mut bytes = Vec::new();
        let mut seen = Vec::new();
        client
            .download_file("book-1", &file, &mut bytes, |n| seen.push(n))
            .unwrap();
        assert_eq!(bytes, b"sound");
        assert_eq!(seen.last(), Some(&5));
        client.curl = mock_program(
            &root,
            "curl-incomplete",
            "#!/bin/sh\ncat >/dev/null\nprintf bad\nprintf 200 >&2\n",
        );
        assert_eq!(
            client.download_file("book-1", &file, &mut Vec::new(), |_| {}),
            Err("Download ended before the file was complete")
        );
        client.curl = mock_program(
            &root,
            "curl-forbidden",
            "#!/bin/sh\ncat >/dev/null\nprintf 403 >&2\nexit 22\n",
        );
        assert_eq!(
            client.download_file("book-1", &file, &mut Vec::new(), |_| {}),
            Err("Audiobookshelf download permission denied")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
