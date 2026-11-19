//! One replaceable pending job and one result; no polling or filesystem access on the UI thread.
use reborn_core::{
    lyrics::{self, Key},
    Track,
};
use reborn_ui::lyrics::{Prepared, State};
use std::{
    fs::File,
    io::Read,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
};

struct Job {
    key: Key,
    track: Track,
    music: PathBuf,
    revision: u64,
}
#[derive(Default)]
struct Inbox {
    job: Option<Job>,
    reply: Option<(Key, State)>,
    running: bool,
    closed: bool,
}
pub(super) struct Worker {
    inbox: Arc<(Mutex<Inbox>, Condvar)>,
    revision: Arc<AtomicU64>,
}
impl Worker {
    pub fn spawn() -> std::io::Result<Self> {
        Self::spawn_using(load)
    }
    fn spawn_using(
        load: impl Fn(&Job, &AtomicU64) -> State + Send + 'static,
    ) -> std::io::Result<Self> {
        let inbox = Arc::new((Mutex::new(Inbox::default()), Condvar::new()));
        let revision = Arc::new(AtomicU64::new(0));
        let (state, epoch) = (inbox.clone(), revision.clone());
        thread::Builder::new()
            .name("lyrics-reader".into())
            .spawn(move || loop {
                let job = {
                    let Ok(mut guard) = state.0.lock() else {
                        return;
                    };
                    while guard.job.is_none() && !guard.closed {
                        let Ok(next) = state.1.wait(guard) else {
                            return;
                        };
                        guard = next;
                    }
                    if guard.closed {
                        return;
                    }
                    guard.running = true;
                    guard.job.take()
                };
                let Some(job) = job else {
                    continue;
                };
                let result = load(&job, &epoch);
                let Ok(mut guard) = state.0.lock() else {
                    return;
                };
                guard.running = false;
                if epoch.load(Ordering::Acquire) == job.revision && !guard.closed {
                    guard.reply = Some((job.key, result));
                }
            })?;
        Ok(Self { inbox, revision })
    }
    pub fn request(&mut self, key: Key, track: Track, music: PathBuf) -> bool {
        let Ok(mut guard) = self.inbox.0.try_lock() else {
            return false;
        };
        let revision = self.revision.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        guard.job = Some(Job {
            key,
            track,
            music,
            revision,
        });
        guard.reply = None;
        self.inbox.1.notify_one();
        true
    }
    pub fn stop(&mut self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut guard) = self.inbox.0.try_lock() {
            guard.job = None;
            guard.reply = None;
        }
    }
    pub fn reply(&mut self) -> Option<(Key, State)> {
        self.inbox.0.try_lock().ok()?.reply.take()
    }
    pub fn quiesce(&mut self) -> bool {
        self.stop();
        self.inbox
            .0
            .try_lock()
            .is_ok_and(|guard| !guard.running && guard.job.is_none())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut guard) = self.inbox.0.lock() {
            guard.closed = true;
            guard.job = None;
        }
        self.inbox.1.notify_one();
    }
}

fn load(job: &Job, revision: &AtomicU64) -> State {
    if revision.load(Ordering::Acquire) != job.revision {
        return State::Missing;
    }
    match read(job, revision) {
        Ok(Some(document)) if !document.lines.is_empty() => {
            State::Ready(Arc::new(Prepared::new(document)))
        }
        Ok(_) => State::Missing,
        Err(error) => State::Failed(error),
    }
}

fn read(job: &Job, revision: &AtomicU64) -> Result<Option<lyrics::Lyrics>, String> {
    let source = reborn_platform::storage::pin_track(&job.track, &job.music)?;
    if !source.matches_revision(job.track.size, job.track.mtime) {
        return Err("The song changed; reopen Lyrics after the library refreshes".into());
    }
    let mut sidecar = None;
    for extension in ["lrc", "LRC"] {
        let path = job.track.path.with_extension(extension);
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("Invalid lyrics filename")?;
        match std::fs::metadata(source.sidecar_directory().join(name)) {
            Ok(meta) if meta.is_file() => {
                sidecar = Some(source.open_sidecar(name)?);
                break;
            }
            Ok(_) => return Err("Lyrics must be a regular local file".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let Some(sidecar) = sidecar else {
        return Ok(None);
    };
    let mut file = File::open(sidecar.path()).map_err(|e| e.to_string())?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    if length > lyrics::MAX_BYTES as u64 {
        return Err(lyrics::Error::TooLarge.message().into());
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length as usize)
        .map_err(|_| "Not enough memory for lyrics")?;
    let mut chunk = [0u8; 8192];
    while bytes.len() <= lyrics::MAX_BYTES {
        if revision.load(Ordering::Acquire) != job.revision {
            return Ok(None);
        }
        let count = file.read(&mut chunk).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    if !source.is_current() {
        return Err("Storage changed while reading lyrics".into());
    }
    lyrics::parse(&bytes)
        .map(Some)
        .map_err(|e| e.message().into())
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
