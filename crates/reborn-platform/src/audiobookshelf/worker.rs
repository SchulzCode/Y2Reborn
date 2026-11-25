//! Serial network and download work; the UI only sends commands and reads replies.
use super::{Account, Book, BookPage, Client, Library, SearchResults, Server};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};

/// Credentials stay in the command and are never printable with `Debug`.
pub enum Command {
    Status {
        request: u64,
    },
    SignIn {
        request: u64,
        server: String,
        username: String,
        password: String,
    },
    Libraries {
        request: u64,
    },
    Books {
        request: u64,
        library: String,
        page: u32,
    },
    Search {
        request: u64,
        library: String,
        query: String,
    },
    Details {
        request: u64,
        item: String,
    },
    Cover {
        request: u64,
        item: String,
    },
    ListCover {
        request: u64,
        item: String,
    },
    Downloaded {
        request: u64,
        root: PathBuf,
        item: String,
    },
    Download {
        request: u64,
        root: PathBuf,
        mount_id: u64,
        item: String,
    },
    Forget {
        request: u64,
    },
}

pub enum Event {
    Status {
        request: u64,
        connected: bool,
        username: Option<String>,
    },
    SignIn {
        request: u64,
        result: Result<(), String>,
        username: Option<String>,
    },
    Libraries {
        request: u64,
        result: Result<Vec<Library>, String>,
    },
    Books {
        request: u64,
        result: Result<BookPage, String>,
    },
    Search {
        request: u64,
        result: Result<SearchResults, String>,
    },
    Details {
        request: u64,
        result: Result<Book, String>,
    },
    Cover {
        request: u64,
        result: Result<Vec<u8>, String>,
    },
    ListCover {
        request: u64,
        item: String,
        result: Result<Vec<u8>, String>,
    },
    Downloaded {
        request: u64,
        result: Result<bool, String>,
    },
    Progress {
        request: u64,
        bytes: u64,
        total: u64,
    },
    Download {
        request: u64,
        result: Result<(), String>,
    },
    Forget {
        request: u64,
        result: Result<bool, String>,
    },
}

pub struct Worker {
    commands: SyncSender<Command>,
    pub events: Receiver<Event>,
    gate: Arc<Mutex<Gate>>,
    thread: std::thread::JoinHandle<()>,
}

impl Worker {
    /// `state` must be a file under Reborn's private state directory.
    pub fn start(state: PathBuf, supported: fn(&Path) -> bool) -> Result<Self, String> {
        Self::start_with(state, supported, "/usr/bin/curl".into())
    }

    fn start_with(
        state: PathBuf,
        supported: fn(&Path) -> bool,
        curl: String,
    ) -> Result<Self, String> {
        let (commands, incoming) = mpsc::sync_channel(16);
        let (outgoing, events) = mpsc::sync_channel(32);
        let gate = Arc::new(Mutex::new(Gate::default()));
        let runner = Runner {
            account: Account::load(&state),
            state,
            supported,
            curl,
            incoming,
            outgoing,
            gate: gate.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("audiobookshelf".into())
            .spawn(move || runner.run())
            .map_err(|_| "Couldn't start Audiobookshelf".to_owned())?;
        Ok(Self {
            commands,
            events,
            gate,
            thread,
        })
    }

    pub fn submit(&self, command: Command) -> Result<(), &'static str> {
        let mut gate = self
            .gate
            .lock()
            .map_err(|_| "Audiobookshelf worker stopped")?;
        if gate.prepared {
            return Err("Sleep is being prepared");
        }
        gate.pending += 1;
        if self.commands.try_send(command).is_err() {
            gate.pending -= 1;
            return Err("Audiobookshelf is busy");
        }
        Ok(())
    }

    /// Refuse a storage or sleep handoff until every queued operation ends.
    pub fn quiesce(&self) -> bool {
        let Ok(mut gate) = self.gate.lock() else {
            return false;
        };
        gate.prepared = true;
        gate.pending == 0 || self.thread.is_finished()
    }

    pub fn resume(&self) {
        if let Ok(mut gate) = self.gate.lock() {
            gate.prepared = false;
        }
    }

    /// Storage setup must not run while a queued operation can touch media.
    pub fn busy(&self) -> bool {
        self.gate.lock().map_or(true, |gate| gate.pending != 0)
    }

    pub fn poll(&self) -> Result<Option<Event>, &'static str> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err("Audiobookshelf worker stopped"),
        }
    }
}

struct Runner {
    state: PathBuf,
    supported: fn(&Path) -> bool,
    curl: String,
    account: Option<Account>,
    incoming: Receiver<Command>,
    outgoing: SyncSender<Event>,
    gate: Arc<Mutex<Gate>>,
}

#[derive(Default)]
struct Gate {
    prepared: bool,
    pending: usize,
}

impl Runner {
    fn run(mut self) {
        crate::lower_thread_priority();
        while let Ok(command) = self.incoming.recv() {
            let event = self.execute(command);
            if let Ok(mut gate) = self.gate.lock() {
                gate.pending -= 1;
            }
            if self.outgoing.send(event).is_err() {
                break;
            }
        }
    }

    fn execute(&mut self, command: Command) -> Event {
        match command {
            Command::Status { request } => Event::Status {
                request,
                connected: self.account.is_some(),
                username: self
                    .account
                    .as_ref()
                    .and_then(|account| account.username.clone()),
            },
            Command::SignIn {
                request,
                server,
                username,
                password,
            } => {
                let result = self.sign_in(&server, &username, &password);
                let username = result.is_ok().then_some(username);
                Event::SignIn {
                    request,
                    result,
                    username,
                }
            }
            Command::Libraries { request } => Event::Libraries {
                request,
                result: self.call(|client| client.libraries().map(|list| list.libraries)),
            },
            Command::Books {
                request,
                library,
                page,
            } => Event::Books {
                request,
                result: self.call(|client| client.items(&library, page)),
            },
            Command::Search {
                request,
                library,
                query,
            } => Event::Search {
                request,
                result: self.call(|client| client.search(&library, &query)),
            },
            Command::Details { request, item } => Event::Details {
                request,
                result: self.call(|client| client.item(&item)),
            },
            Command::Cover { request, item } => Event::Cover {
                request,
                result: self.call(|client| client.cover(&item)),
            },
            Command::ListCover { request, item } => {
                let result = self.call(|client| client.cover(&item));
                Event::ListCover {
                    request,
                    item,
                    result,
                }
            }
            Command::Downloaded {
                request,
                root,
                item,
            } => Event::Downloaded {
                request,
                result: self.downloaded(&root, &item),
            },
            Command::Download {
                request,
                root,
                mount_id,
                item,
            } => Event::Download {
                request,
                result: self.download(request, &root, mount_id, &item),
            },
            Command::Forget { request } => Event::Forget {
                request,
                result: self.forget(),
            },
        }
    }

    fn client(&self) -> Result<Client, String> {
        let mut client = self
            .account
            .as_ref()
            .ok_or("Sign in to Audiobookshelf")?
            .client()
            .map_err(str::to_owned)?;
        client.curl.clone_from(&self.curl);
        Ok(client)
    }

    fn sign_in(&mut self, address: &str, username: &str, password: &str) -> Result<(), String> {
        let server = Server::new(address).map_err(str::to_owned)?;
        let client = Client {
            server: server.clone(),
            token: String::new(),
            curl: self.curl.clone(),
        };
        let session = client
            .login_inner(username, password)
            .map_err(str::to_owned)?;
        let mut account = Account::new(server, session);
        account.username = Some(username.to_owned());
        account
            .save(&self.state)
            .map_err(|_| "Couldn't save the Audiobookshelf session")?;
        self.account = Some(account);
        Ok(())
    }

    fn refresh(&mut self) -> Result<(), String> {
        let client = self.client()?;
        let account = self.account.as_mut().ok_or("Sign in to Audiobookshelf")?;
        let next = match client.refresh(account.session.refresh_token()) {
            Ok(next) => next,
            Err("Sign in to Audiobookshelf again") => {
                self.forget_local()?;
                return Err("Sign in to Audiobookshelf again".into());
            }
            Err(error) => return Err(error.into()),
        };
        account.session = next;
        account
            .save(&self.state)
            .map_err(|_| "Couldn't save the Audiobookshelf session".into())
    }

    fn call<T>(
        &mut self,
        mut request: impl FnMut(&Client) -> Result<T, &'static str>,
    ) -> Result<T, String> {
        match request(&self.client()?) {
            Err("Sign in to Audiobookshelf again") => {
                self.refresh()?;
                match request(&self.client()?) {
                    Err("Sign in to Audiobookshelf again") => {
                        self.forget_local()?;
                        Err("Sign in to Audiobookshelf again".into())
                    }
                    result => result.map_err(str::to_owned),
                }
            }
            result => result.map_err(str::to_owned),
        }
    }

    fn download(
        &mut self,
        request: u64,
        root: &Path,
        mount_id: u64,
        item: &str,
    ) -> Result<(), String> {
        let book = self.call(|client| client.item(item))?;
        if book.id != item {
            return Err("Audiobookshelf returned the wrong book".into());
        }
        let result = self.download_once(request, root, mount_id, &book);
        if matches!(&result, Err(error) if error == "Sign in to Audiobookshelf again") {
            self.refresh()?;
            let result = self.download_once(request, root, mount_id, &book);
            if matches!(&result, Err(error) if error == "Sign in to Audiobookshelf again") {
                self.forget_local()?;
            }
            result
        } else {
            result
        }
    }

    fn downloaded(&self, root: &Path, item: &str) -> Result<bool, String> {
        let key = self
            .client()?
            .server
            .book_key(item)
            .map_err(str::to_owned)?;
        super::staging::downloaded(root, &key)
    }

    fn download_once(
        &self,
        request: u64,
        root: &Path,
        mount_id: u64,
        book: &super::Book,
    ) -> Result<(), String> {
        let reply = &self.outgoing;
        self.client()?.download_book_verified(
            root,
            Some(mount_id),
            book,
            self.supported,
            |bytes, total| {
                let _ = reply.try_send(Event::Progress {
                    request,
                    bytes,
                    total,
                });
            },
        )
    }

    fn forget_local(&mut self) -> Result<(), String> {
        match std::fs::remove_file(&self.state) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Couldn't remove the Audiobookshelf session".into()),
        }
        self.account = None;
        Ok(())
    }

    fn forget(&mut self) -> Result<bool, String> {
        let remote = self.account.as_ref().and_then(|account| {
            self.client()
                .ok()
                .map(|client| (client, account.session.refresh_token().to_owned()))
        });
        self.forget_local()?;
        Ok(remote.is_none_or(|(client, token)| client.logout(&token).is_ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    #[test]
    fn rejected_refresh_forgets_session_but_network_failure_keeps_it() {
        for (name, refresh, connected) in [
            ("rejected", "printf '\\n401'", false),
            ("offline", "exit 7", true),
        ] {
            let root =
                std::env::temp_dir().join(format!("reborn-abs-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let state = root.join("account.json");
            let mut account = Account::new(
                Server::new("https://books.example.test").unwrap(),
                super::super::AuthSession::new("access".into(), "refresh".into()).unwrap(),
            );
            account.username = Some("reader".into());
            account.save(&state).unwrap();
            let script = root.join("curl");
            let source = format!(
                "#!/bin/sh\ncat >/dev/null\ncase \"$*\" in */auth/refresh*) {refresh};; \
                 *) printf '\\n401';; esac\n"
            );
            std::fs::write(&script, source).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let worker =
                Worker::start_with(state.clone(), |_| true, script.to_string_lossy().into())
                    .unwrap();
            worker.submit(Command::Libraries { request: 1 }).unwrap();
            assert!(matches!(
                worker.events.recv_timeout(Duration::from_secs(3)).unwrap(),
                Event::Libraries { result: Err(_), .. }
            ));
            worker.submit(Command::Status { request: 2 }).unwrap();
            match worker.events.recv_timeout(Duration::from_secs(3)).unwrap() {
                Event::Status {
                    connected: actual,
                    username,
                    ..
                } => {
                    assert_eq!(actual, connected);
                    assert_eq!(username.as_deref(), connected.then_some("reader"));
                }
                _ => panic!("expected status"),
            }
            assert_eq!(state.exists(), connected);
            drop(worker);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn offline_logout_still_removes_local_session() {
        let root =
            std::env::temp_dir().join(format!("reborn-abs-offline-logout-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let state = root.join("account.json");
        Account::new(
            Server::new("https://books.example.test").unwrap(),
            super::super::AuthSession::new("access".into(), "refresh".into()).unwrap(),
        )
        .save(&state)
        .unwrap();
        let script = root.join("curl");
        std::fs::write(&script, "#!/bin/sh\ncat >/dev/null\nexit 7\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let worker =
            Worker::start_with(state.clone(), |_| true, script.to_string_lossy().into()).unwrap();
        worker.submit(Command::Forget { request: 1 }).unwrap();
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Forget {
                result: Ok(false),
                ..
            }
        ));
        assert!(!state.exists());
        drop(worker);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn forget_reports_when_the_session_cannot_be_removed() {
        let root = std::env::temp_dir().join(format!("reborn-abs-forget-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let worker = Worker::start(root.clone(), |_| true).unwrap();
        worker.submit(Command::Forget { request: 7 }).unwrap();
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Forget {
                request: 7,
                result: Err(_)
            }
        ));
        drop(worker);
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn worker_signs_in_refreshes_downloads_and_forgets() {
        let root = std::env::temp_dir().join(format!("reborn-abs-worker-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("curl");
        let marker = root.join("retried");
        let mock = r##"#!/bin/sh
cat >/dev/null
case "$*" in
  */login*) printf '{"user":{"accessToken":"access","refreshToken":"refresh"}}\n200';;
  */auth/refresh*) printf '{"user":{"accessToken":"newaccess","refreshToken":"newrefresh"}}\n200';;
  */logout*) printf '{}\n200';;
  */api/libraries*)
    if [ ! -e '__MARKER__' ]; then : > '__MARKER__'; printf '\n401';
    else printf '{"libraries":[{"id":"lib-1","name":"Books","mediaType":"book"}]}\n200'; fi;;
  *"/api/items/book-1?expanded=1"*)
    printf '%s' '{"id":"book-1","media":{"metadata":{"title":"A Book"},'
    printf '%s' '"audioFiles":[{"index":1,"ino":"123",'
    printf '%s' '"metadata":{"filename":"part.mp3","size":3}}]}}'
    printf '\n200';;
  */cover*) printf 'cover-bytes\n200';;
  */file/123/download*) printf one; printf 200 >&2;;
  *) exit 1;;
esac
"##
        .replace("__MARKER__", &marker.to_string_lossy());
        std::fs::write(&script, mock).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = root.join("private/account.json");
        let worker =
            Worker::start_with(state.clone(), |_| true, script.to_string_lossy().into()).unwrap();
        assert!(worker
            .submit(Command::SignIn {
                request: 1,
                server: "https://books.example.test/prefix".into(),
                username: "reader".into(),
                password: "test-password".into()
            })
            .is_ok());
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::SignIn {
                request: 1,
                result: Ok(()),
                ..
            }
        ));
        worker.submit(Command::Status { request: 7 }).unwrap();
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Status { connected: true, username: Some(name), .. } if name == "reader"
        ));
        assert!(worker.submit(Command::Libraries { request: 2 }).is_ok());
        match worker.events.recv_timeout(Duration::from_secs(3)).unwrap() {
            Event::Libraries {
                request: 2,
                result: Ok(libraries),
            } => assert_eq!(libraries.len(), 1),
            _ => panic!("expected the library reply"),
        }
        assert_eq!(
            Account::load(&state).unwrap().session.refresh_token(),
            "newrefresh"
        );
        assert_eq!(
            Account::load(&state).unwrap().username.as_deref(),
            Some("reader")
        );
        assert!(worker
            .submit(Command::Cover {
                request: 6,
                item: "book-1".into()
            })
            .is_ok());
        match worker.events.recv_timeout(Duration::from_secs(3)).unwrap() {
            Event::Cover {
                request: 6,
                result: Ok(bytes),
            } => assert_eq!(bytes, b"cover-bytes"),
            _ => panic!("expected the cover reply"),
        }
        worker
            .submit(Command::ListCover {
                request: 8,
                item: "book-1".into(),
            })
            .unwrap();
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::ListCover { item, result: Ok(bytes), .. }
                if item == "book-1" && bytes == b"cover-bytes"
        ));
        assert!(worker
            .submit(Command::Download {
                request: 3,
                root: root.clone(),
                mount_id: crate::storage::descriptor_mount(&std::fs::File::open(&root).unwrap())
                    .unwrap(),
                item: "book-1".into()
            })
            .is_ok());
        loop {
            match worker.events.recv_timeout(Duration::from_secs(3)).unwrap() {
                Event::Progress { request: 3, .. } => {}
                Event::Download {
                    request: 3,
                    result: Ok(()),
                } => break,
                _ => panic!("expected download progress or completion"),
            }
        }
        let key = Server::new("https://books.example.test/prefix")
            .unwrap()
            .book_key("book-1")
            .unwrap();
        assert_eq!(
            std::fs::read(
                root.join("Audiobooks/Audiobookshelf")
                    .join(key)
                    .join("A Book/0000000001.mp3")
            )
            .unwrap(),
            b"one"
        );
        assert!(worker
            .submit(Command::Downloaded {
                request: 5,
                root: root.clone(),
                item: "book-1".into(),
            })
            .is_ok());
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Downloaded {
                request: 5,
                result: Ok(true)
            }
        ));
        assert!(worker.submit(Command::Forget { request: 4 }).is_ok());
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::Forget {
                request: 4,
                result: Ok(true)
            }
        ));
        assert!(!state.exists());
        drop(worker);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn quiescence_waits_for_work_and_refuses_new_commands() {
        let root = std::env::temp_dir().join(format!("reborn-abs-quiesce-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("curl");
        std::fs::write(
            &script,
            concat!(
                "#!/bin/sh\ncat >/dev/null\nsleep 0.2\n",
                "printf '{\"user\":{\"accessToken\":\"access\",",
                "\"refreshToken\":\"refresh\"}}\\n200'\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let worker = Worker::start_with(
            root.join("state.json"),
            |_| true,
            script.to_string_lossy().into(),
        )
        .unwrap();
        assert!(worker
            .submit(Command::SignIn {
                request: 1,
                server: "https://books.example.test".into(),
                username: "reader".into(),
                password: "test".into(),
            })
            .is_ok());
        assert!(!worker.quiesce());
        assert!(worker.submit(Command::Libraries { request: 2 }).is_err());
        worker.resume();
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_secs(3)).unwrap(),
            Event::SignIn { result: Ok(()), .. }
        ));
        assert!(worker.quiesce());
        assert!(worker.submit(Command::Libraries { request: 3 }).is_err());
        worker.resume();
        drop(worker);
        std::fs::remove_dir_all(root).unwrap();
    }
}
