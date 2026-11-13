//! The routes over a loopback connection against a temporary folder.
use super::*;
use std::io::{Read, Write};
use std::sync::atomic::AtomicU32;

static WINDOWS: AtomicU32 = AtomicU32::new(0);
fn window(open: bool) -> Result<(), String> {
    if open {
        WINDOWS.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

struct Client {
    port: u16,
    cookie: String,
}

impl Client {
    /// One request; the status number, the head and the body.
    fn call(&self, method: &str, target: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port)).unwrap();
        let head = format!(
            "{method} {target} HTTP/1.1\r\nHost: y2\r\nCookie: {}\r\nContent-Length: {}\r\n\r\n",
            self.cookie,
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        let mut answer = vec![];
        stream.read_to_end(&mut answer).unwrap();
        let split = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&answer[..split]).into_owned();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, head, answer[split + 4..].to_vec())
    }
    fn status(&self, method: &str, target: &str) -> u16 {
        self.call(method, target, b"").0
    }
}

fn started(name: &str, guarded: bool) -> (Share, Client, PathBuf) {
    let root = std::env::temp_dir().join(format!("share-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("Albums")).unwrap();
    let storage = Storage {
        key: "internal",
        name: "Internal Storage",
        root: root.clone(),
        guarded,
    };
    let share = Share::start_with(vec![storage], 0, window).unwrap();
    let port = share.address.rsplit(':').next().unwrap().parse().unwrap();
    let cookie = String::new();
    (share, Client { port, cookie }, root)
}

fn entered(share: &Share, client: &mut Client) {
    let (status, head, _) = client.call("POST", &format!("/api/enter?code={}", share.code()), b"");
    assert_eq!(status, 200);
    let cookie = head.lines().find(|l| l.starts_with("Set-Cookie:")).unwrap();
    let pair = cookie["Set-Cookie:".len()..]
        .split(';')
        .next()
        .unwrap()
        .trim();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    client.cookie = pair.to_owned();
}

#[test]
fn nothing_opens_without_the_code_and_wrong_codes_lock() {
    let (share, mut client, root) = started("code", false);
    assert_eq!(share.code().len(), 6);
    // The page itself is public; everything behind it is not.
    let (status, _, page) = client.call("GET", "/", b"");
    assert!(status == 200 && String::from_utf8_lossy(&page).contains("Wi-Fi Share"));
    assert_eq!(client.status("GET", "/api/list"), 401);
    assert_eq!(
        client.status("PUT", "/api/file?root=internal&path=/a.mp3"),
        401
    );
    client.cookie = "y2share=guessed".into();
    assert_eq!(client.status("GET", "/api/list"), 401);
    for _ in 0..WRONG_CODES {
        assert_eq!(client.status("POST", "/api/enter?code=000000x"), 403);
    }
    // Locked now: even the right code waits.
    let right = format!("/api/enter?code={}", share.code());
    assert_eq!(client.status("POST", &right), 429);
    *share.shared.wrong.lock().unwrap() = (0, None);
    entered(&share, &mut client);
    assert_eq!(client.status("GET", "/api/list"), 200);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn files_are_listed_received_renamed_downloaded_and_deleted() {
    let (share, mut client, root) = started("files", true);
    entered(&share, &mut client);
    let (_, _, body) = client.call("GET", "/api/list", b"");
    let storages: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(storages["storages"][0]["name"], "Internal Storage");
    // An upload into a folder that does not exist yet (a dropped folder), with a name a browser
    // would encode.
    let before = WINDOWS.load(Ordering::Relaxed);
    let song = "/api/file?root=internal&path=/Albums/Bj%C3%B6rk/One%20Song.mp3";
    assert_eq!(client.call("PUT", song, b"music").0, 200);
    let file = root.join("Albums/Björk/One Song.mp3");
    assert_eq!(fs::read(&file).unwrap(), b"music");
    assert!(!root.join("Albums/Björk/.One Song.mp3.part").exists());
    assert!(
        WINDOWS.load(Ordering::Relaxed) > before,
        "the card's write window"
    );
    assert_eq!((share.received(), share.changes()), (1, 1));
    // The same name again is refused unless the page asked to replace.
    assert_eq!(client.call("PUT", song, b"other").0, 409);
    assert_eq!(fs::read(&file).unwrap(), b"music");
    assert_eq!(
        client.call("PUT", &format!("{song}&replace=1"), b"newer").0,
        200
    );
    assert_eq!(fs::read(&file).unwrap(), b"newer");
    let (_, _, body) = client.call(
        "GET",
        "/api/list?root=internal&path=/Albums/Bj%C3%B6rk",
        b"",
    );
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(list["entries"][0]["name"], "One Song.mp3");
    assert_eq!(list["entries"][0]["size"], 5);
    let (status, head, body) = client.call("GET", song, b"");
    assert!(status == 200 && body == b"newer" && head.contains("attachment"));
    let at = "root=internal&path=/Albums/Bj%C3%B6rk/One%20Song.mp3";
    assert_eq!(
        client.status("POST", &format!("/api/rename?{at}&to=Two.mp3")),
        200
    );
    assert!(root.join("Albums/Björk/Two.mp3").is_file() && !file.exists());
    assert_eq!(
        client.status("POST", "/api/folder?root=internal&path=/New"),
        200
    );
    assert_eq!(
        client.status("POST", "/api/folder?root=internal&path=/New"),
        409
    );
    assert_eq!(
        client.status("POST", "/api/delete?root=internal&path=/Albums"),
        200
    );
    assert!(!root.join("Albums").exists() && root.join("New").is_dir());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn nothing_outside_the_storage_is_reached() {
    let (share, mut client, root) = started("escape", false);
    entered(&share, &mut client);
    std::os::unix::fs::symlink("/etc", root.join("out")).unwrap();
    fs::write(root.join(".secret"), b"x").unwrap();
    for target in [
        "/api/file?root=internal&path=/../../etc/hostname",
        "/api/file?root=internal&path=/out/hostname",
        "/api/file?root=internal&path=/.secret",
        "/api/list?root=internal&path=/..",
        "/api/list?root=internal&path=/out",
    ] {
        assert_eq!(client.status("GET", target), 403, "{target}");
    }
    assert_eq!(
        client
            .call("PUT", "/api/file?root=internal&path=/../x.mp3", b"x")
            .0,
        403
    );
    assert_eq!(
        client
            .call("PUT", "/api/file?root=internal&path=/.x.mp3", b"x")
            .0,
        403
    );
    assert_eq!(
        client.status("POST", "/api/rename?root=internal&path=/Albums&to=../x"),
        403
    );
    assert_eq!(
        client.status("POST", "/api/rename?root=internal&path=/Albums&to=.x"),
        403
    );
    // The storage itself is neither renamed nor deleted, and another storage is not there.
    assert_eq!(
        client.status("POST", "/api/delete?root=internal&path=/"),
        404
    );
    assert_eq!(
        client.status("POST", "/api/rename?root=internal&path=/&to=x"),
        403
    );
    assert_eq!(client.status("GET", "/api/list?root=sd&path=/"), 404);
    // Stopping closes the door.
    let port = client.port;
    drop(share);
    std::thread::sleep(Duration::from_millis(300));
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
    fs::remove_dir_all(root).unwrap();
}
