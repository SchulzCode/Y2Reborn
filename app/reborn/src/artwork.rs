//! Library thumbnails: bounded background decoding with an on-disk cache.
//! Native decoding never runs on the UI thread.
use reborn_core::Track;
use reborn_graphics::{ART_SIZE, THUMB_SIZE};
use reborn_media::{external_artwork, folder_artwork_candidates, Cancel, MediaFile};
use reborn_observability::isolated;
use reborn_platform::storage::{pin_track, PinnedTrack};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc, Condvar, Mutex,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub source: String,
    pub id: i64,
    path: PathBuf,
    size: u64,
    mtime: i64,
    mount: Option<u64>,
    embedded: bool,
    music: PathBuf,
}
impl Key {
    pub fn new(track: &Track, mount: Option<u64>, music: &Path) -> Self {
        Self {
            source: track.source_id.clone(),
            id: track.id,
            path: track.path.clone(),
            size: track.size,
            mtime: track.mtime,
            mount,
            embedded: track.artwork,
            music: music.into(),
        }
    }
    /// Size and modification time of the file, part of a cache file's name.
    fn revision(&self) -> (u64, i64) {
        (self.size, self.mtime)
    }
    /// The folder's cover images as they are now (names, sizes, modification
    /// times; 0 for none), part of a cache file's name: a `cover.jpg` deleted,
    /// added or replaced changes it while the song files stay the same, and the
    /// old thumbnail stayed in Cover Flow (Y2Reborn #105). One folder listing per
    /// thumbnail, on the worker thread.
    fn folder_revision(&self) -> u64 {
        let path = if self.path.is_absolute() {
            self.path.clone()
        } else {
            self.music.join(&self.path)
        };
        let Some(folder) = path.parent() else {
            return 0;
        };
        let names = folder_artwork_candidates(folder).unwrap_or_default();
        if names.is_empty() {
            return 0;
        }
        // FNV-1a: the same value on every run and every Rust version.
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut add = |bytes: &[u8]| {
            for byte in bytes {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        for name in names {
            let (size, modified) = std::fs::metadata(folder.join(&name))
                .map(|m| {
                    let modified = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |d| d.as_nanos());
                    (m.len(), modified)
                })
                .unwrap_or((0, 0));
            add(name.as_bytes());
            add(&size.to_le_bytes());
            add(&modified.to_le_bytes());
        }
        hash.max(1)
    }
    /// The cover as `side` x `side` RGBA, if the file is still the one this key names.
    pub fn load_cover(&self, side: usize, cancel: &Cancel) -> Option<Vec<u8>> {
        let source = self.pin()?;
        let (pixels, _) = load(self, &source, side, cancel);
        let pixels = pixels.filter(|v| v.len() == side * side * 4)?;
        source.is_current().then_some(pixels)
    }
    fn pin(&self) -> Option<Arc<PinnedTrack>> {
        let track = Track {
            source_id: self.source.clone(),
            path: self.path.clone(),
            ..Default::default()
        };
        let pin = pin_track(&track, &self.music).ok()?;
        (pin.mount_id() == self.mount
            && pin.matches_revision(self.size, self.mtime)
            && pin.is_current())
        .then(|| Arc::new(pin))
    }
}
/// What supplies a picture: a track's cover or a downloaded station logo.
pub enum Art {
    Track(Key),
    // The benchmark build shares this file and decodes covers only.
    #[allow(dead_code)]
    Logo(PathBuf),
}
impl From<Key> for Art {
    fn from(key: Key) -> Self {
        Self::Track(key)
    }
}
impl Art {
    /// The picture as a `side` x `side` RGBA square: a cover, or a logo on its tile.
    fn load(&self, side: usize, cancel: &Cancel) -> Option<Vec<u8>> {
        match self {
            Self::Track(key) => key.load_cover(side, cancel),
            Self::Logo(path) => {
                let _cpu_hint = reborn_platform::workload::CpuWorkloadLease::acquire_workload_hint(
                    reborn_platform::workload::Class::ArtworkDecode,
                );
                reborn_media::logo_image(path, side as u32)
                    .ok()
                    .and_then(|fitted| reborn_ui::art::logo_tile(&fitted))
            }
        }
    }
    /// The folder's covers (`Key::folder_revision`); a logo has no folder.
    fn folder_revision(&self) -> u64 {
        match self {
            Self::Track(key) => key.folder_revision(),
            Self::Logo(_) => 0,
        }
    }
    /// Size and modification time of the source, part of a cache file's name.
    fn revision(&self) -> (u64, i64) {
        match self {
            Self::Track(key) => key.revision(),
            Self::Logo(path) => std::fs::metadata(path)
                .map(|m| {
                    (
                        m.len(),
                        m.modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0, |d| d.as_secs() as i64),
                    )
                })
                .unwrap_or((0, 0)),
        }
    }
}

/// A thumbnail wanted by the UI: the opaque id the renderer uses, what
/// supplies the picture and the directory of the on-disk cache.
pub struct Job {
    pub thumb: u64,
    pub art: Art,
    pub cache: PathBuf,
}
pub struct Reply {
    pub thumb: u64,
    /// A full-resolution (`ART_SIZE`) cover for Cover Flow's centre, not a thumbnail.
    #[allow(dead_code)]
    pub full: bool,
    /// `THUMB_SIZE` RGBA, square (the look shapes it), or `None` when the track has no cover.
    pub rgba: Option<Vec<u8>>,
    pub decode_ms: f64,
    pub cache_hit: bool,
}
#[derive(Default)]
struct Shared {
    queue: VecDeque<Job>,
    /// Cover Flow's centre cover at full resolution; the latest request wins.
    cover: Option<Job>,
    current: Option<u64>,
    stop: bool,
    /// Replies sent and not yet taken by the player, by cover and whether it
    /// is a full-resolution one. Such a cover is not decoded again: while the
    /// player takes fewer replies per pass than the worker makes (a cached
    /// thumbnail takes a few milliseconds), asking again on every pass would
    /// fill the channel with copies, delay every new cover behind them and
    /// hold their pixels in memory.
    unread: HashMap<(u64, bool), u32>,
}
/// Cached thumbnails are at most this many files, one per album (about 72 MB):
/// a 30 000-song library has about 2 500 albums.
const CACHE_LIMIT: usize = 3000;
/// When the cache is full, this many of the least recently shown thumbnails
/// make room, so the albums someone browses stay cached.
const CACHE_EVICT: usize = CACHE_LIMIT / 10;
/// A shown thumbnail's file is marked recent (one inode write) at most this often.
const TOUCH_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

pub struct Worker {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    replies: Receiver<Reply>,
    failed: HashSet<u64>,
}
impl Worker {
    pub fn spawn() -> Result<Self, std::io::Error> {
        let shared = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let (tx, replies) = channel();
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("thumbnails".into())
            .spawn(move || run(&worker, &tx))?;
        Ok(Self {
            shared,
            replies,
            failed: HashSet::new(),
        })
    }
    /// Replace the queue with the thumbnails the screen shows now, most
    /// important first. Work that already failed is not retried until
    /// `forget_failures`; the job being decoded finishes regardless.
    pub fn want(&mut self, jobs: Vec<Job>) {
        let (lock, wake) = &*self.shared;
        let Ok(mut shared) = lock.lock() else {
            return;
        };
        let current = shared.current;
        let queue = jobs
            .into_iter()
            .filter(|job| {
                !self.failed.contains(&job.thumb)
                    && Some(job.thumb) != current
                    && !shared.unread.contains_key(&(job.thumb, false))
            })
            .collect();
        shared.queue = queue;
        if !shared.queue.is_empty() {
            wake.notify_one();
        }
    }
    /// Decode one cover at full resolution before any queued thumbnail.
    #[allow(dead_code)]
    pub fn want_cover(&mut self, job: Job) {
        let (lock, wake) = &*self.shared;
        let Ok(mut shared) = lock.lock() else {
            return;
        };
        if !shared.unread.contains_key(&(job.thumb, true)) {
            shared.cover = Some(job);
            wake.notify_one();
        }
    }
    /// The next reply, if one is waiting; its cover may be asked for again.
    #[allow(dead_code)]
    pub fn try_reply(&self) -> Option<Reply> {
        let reply = self.replies.try_recv().ok()?;
        self.taken(&reply);
        Some(reply)
    }
    /// Wait up to `wait` for the next reply.
    #[allow(dead_code)]
    pub fn reply_within(&self, wait: std::time::Duration) -> Option<Reply> {
        let reply = self.replies.recv_timeout(wait).ok()?;
        self.taken(&reply);
        Some(reply)
    }
    fn taken(&self, reply: &Reply) {
        let (lock, _) = &*self.shared;
        if let Ok(mut shared) = lock.lock() {
            let key = (reply.thumb, reply.full);
            if let Some(n) = shared.unread.get_mut(&key) {
                *n -= 1;
                if *n == 0 {
                    shared.unread.remove(&key);
                }
            }
        }
    }
    // Used by the player; the benchmark build shares this file without them.
    #[allow(dead_code)]
    pub fn record_failure(&mut self, thumb: u64) {
        if self.failed.len() < 4096 {
            self.failed.insert(thumb);
        }
    }
    #[allow(dead_code)]
    pub fn forget_failures(&mut self) {
        self.failed.clear();
    }
    /// Drop queued work; true once nothing is being decoded.
    pub fn quiesce(&mut self) -> bool {
        let (lock, _) = &*self.shared;
        lock.lock().is_ok_and(|mut shared| {
            shared.queue.clear();
            shared.cover = None;
            shared.current.is_none()
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let (lock, wake) = &*self.shared;
        if let Ok(mut shared) = lock.lock() {
            shared.stop = true;
            shared.queue.clear();
        }
        wake.notify_all();
    }
}

/// 31: folder covers won over embedded pictures ("thumb-" files rebuilt).
/// Beta 8.18: thumbnails are 128x128, decoded straight from the picture and
/// cached as 4:2:0 YCbCr (corners are rounded after reading); older files are
/// rebuilt once and removed. Beta 9.3.17: the name also carries the folder's
/// covers (`Key::folder_revision`, Y2Reborn #105).
const THUMB_PREFIX: &str = "thumb4-";
const LEGACY_THUMB_PREFIXES: [&str; 3] = ["thumb-", "thumb2-", "thumb3-"];
const CACHED_BYTES: usize = THUMB_SIZE * THUMB_SIZE * 3 / 2;
fn cache_path(job: &Job) -> PathBuf {
    let (size, mtime) = job.art.revision();
    let folder = job.art.folder_revision();
    job.cache.join(format!(
        "{THUMB_PREFIX}{:016x}-{mtime:x}-{size:x}-{folder:x}.t128",
        job.thumb
    ))
}
/// A `THUMB_SIZE` cover as JPEG's 4:2:0 YCbCr (BT.601, full range): luma per
/// texel, then Cb and Cr per 2x2 block. Half the bytes of RGB; at thumbnail
/// size the shared colour is not visible.
fn encode_420(rgba: &[u8]) -> Vec<u8> {
    const N: usize = THUMB_SIZE;
    let rgb = |x: usize, y: usize| {
        let i = (y * N + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2]].map(f32::from)
    };
    let byte = |v: f32| v.round().clamp(0., 255.) as u8;
    let mut out = Vec::with_capacity(CACHED_BYTES);
    for y in 0..N {
        for x in 0..N {
            let [r, g, b] = rgb(x, y);
            out.push(byte(0.299 * r + 0.587 * g + 0.114 * b));
        }
    }
    let (mut cb, mut cr) = (vec![], vec![]);
    for y in (0..N).step_by(2) {
        for x in (0..N).step_by(2) {
            let mut m = [0f32; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = rgb(x + dx, y + dy);
                (0..3).for_each(|c| m[c] += p[c] / 4.);
            }
            let [r, g, b] = m;
            cb.push(byte(128. - 0.168_736 * r - 0.331_264 * g + 0.5 * b));
            cr.push(byte(128. + 0.5 * r - 0.418_688 * g - 0.081_312 * b));
        }
    }
    out.extend(cb);
    out.extend(cr);
    out
}
/// Opaque RGBA from `encode_420`, chroma interpolated between block centres.
fn decode_420(bytes: &[u8]) -> Vec<u8> {
    const N: usize = THUMB_SIZE;
    const H: usize = N / 2;
    let (luma, chroma) = bytes.split_at(N * N);
    let (cb, cr) = chroma.split_at(H * H);
    let sample = |plane: &[u8], x: usize, y: usize| {
        // Block centres sit at texel 2i + 0.5.
        let fx = ((x as f32 - 0.5) / 2.).clamp(0., (H - 1) as f32);
        let fy = ((y as f32 - 0.5) / 2.).clamp(0., (H - 1) as f32);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(H - 1), (y0 + 1).min(H - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let at = |x: usize, y: usize| f32::from(plane[y * H + x]);
        let top = at(x0, y0) * (1. - tx) + at(x1, y0) * tx;
        let bottom = at(x0, y1) * (1. - tx) + at(x1, y1) * tx;
        top * (1. - ty) + bottom * ty - 128.
    };
    let byte = |v: f32| v.round().clamp(0., 255.) as u8;
    let mut out = Vec::with_capacity(N * N * 4);
    for y in 0..N {
        for x in 0..N {
            let l = f32::from(luma[y * N + x]);
            let (b, r) = (sample(cb, x, y), sample(cr, x, y));
            out.extend([
                byte(l + 1.402 * r),
                byte(l - 0.344_136 * b - 0.714_136 * r),
                byte(l + 1.772 * b),
                255,
            ]);
        }
    }
    out
}
/// Remove the `count` least recently shown thumbnails; how many went.
fn evict(dir: &Path, count: usize) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(THUMB_PREFIX))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort();
    files
        .into_iter()
        .take(count)
        .filter(|(_, path)| std::fs::remove_file(path).is_ok())
        .count()
}
/// Mark a cached thumbnail as shown now, unless it was marked recently.
fn touch(path: &Path) {
    let now = std::time::SystemTime::now();
    let Ok(file) = std::fs::File::options().write(true).open(path) else {
        return;
    };
    let stale = file
        .metadata()
        .and_then(|m| m.modified())
        .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > TOUCH_AFTER));
    if stale {
        let _ = file.set_modified(now);
    }
}

fn run(shared: &(Mutex<Shared>, Condvar), tx: &Sender<Reply>) {
    let (lock, wake) = shared;
    // Counted on the first job; every job names the same cache directory.
    let mut cached = 0usize;
    let mut counted = false;
    loop {
        let (job, full) = {
            let Ok(mut guard) = lock.lock() else {
                return;
            };
            loop {
                if guard.stop {
                    return;
                }
                if let Some(job) = guard.cover.take() {
                    guard.current = Some(job.thumb);
                    break (job, true);
                }
                if let Some(job) = guard.queue.pop_front() {
                    guard.current = Some(job.thumb);
                    break (job, false);
                }
                guard = match wake.wait(guard) {
                    Ok(g) => g,
                    Err(_) => return,
                };
            }
        };
        if !counted {
            counted = true;
            cached = std::fs::read_dir(&job.cache)
                .map(|d| {
                    d.flatten()
                        .filter(|e| {
                            let name = e.file_name();
                            let name = name.to_string_lossy();
                            if LEGACY_THUMB_PREFIXES.iter().any(|p| name.starts_with(p)) {
                                let _ = std::fs::remove_file(e.path());
                                return false;
                            }
                            name.starts_with(THUMB_PREFIX)
                        })
                        .count()
                })
                .unwrap_or(0);
        }
        let started = std::time::Instant::now();
        if full {
            let rgba = Cancel::new()
                .ok()
                .and_then(|cancel| isolated(|| job.art.load(ART_SIZE, &cancel)).ok().flatten());
            if let Ok(mut guard) = lock.lock() {
                guard.current = None;
                *guard.unread.entry((job.thumb, true)).or_insert(0) += 1;
            }
            let reply = Reply {
                thumb: job.thumb,
                full: true,
                rgba,
                decode_ms: started.elapsed().as_secs_f64() * 1000.,
                cache_hit: false,
            };
            if tx.send(reply).is_err() {
                return;
            }
            continue;
        }
        let path = cache_path(&job);
        let hit = std::fs::read(&path)
            .ok()
            .filter(|bytes| bytes.len() == CACHED_BYTES);
        let (rgba, cache_hit) = match hit {
            Some(bytes) => {
                touch(&path);
                (reborn_ui::art::thumbnail(&decode_420(&bytes)), true)
            }
            None => {
                let cancel = Cancel::new().ok();
                // A bug on one odd picture costs its cover, not the player.
                let cover = cancel.and_then(|cancel| {
                    isolated(|| job.art.load(THUMB_SIZE, &cancel))
                        .ok()
                        .flatten()
                });
                let rgba = cover.as_deref().and_then(reborn_ui::art::thumbnail);
                if let (Some(cover), Some(_)) = (&cover, &rgba) {
                    if cached >= CACHE_LIMIT {
                        cached -= evict(&job.cache, CACHE_EVICT).min(cached);
                    }
                    if cached < CACHE_LIMIT {
                        let temp = path.with_extension("tmp");
                        if std::fs::write(&temp, encode_420(cover)).is_ok()
                            && std::fs::rename(&temp, &path).is_ok()
                        {
                            cached += 1;
                        } else {
                            let _ = std::fs::remove_file(&temp);
                        }
                    }
                }
                (rgba, false)
            }
        };
        if let Ok(mut guard) = lock.lock() {
            guard.current = None;
            *guard.unread.entry((job.thumb, false)).or_insert(0) += 1;
        }
        let reply = Reply {
            thumb: job.thumb,
            full: false,
            rgba,
            decode_ms: started.elapsed().as_secs_f64() * 1000.,
            cache_hit,
        };
        if tx.send(reply).is_err() {
            return;
        }
    }
}

fn load(key: &Key, source: &PinnedTrack, side: usize, cancel: &Cancel) -> (Option<Vec<u8>>, bool) {
    let _cpu_hint = reborn_platform::workload::CpuWorkloadLease::acquire_workload_hint(
        reborn_platform::workload::Class::ArtworkDecode,
    );
    // The folder's cover first (one image for the whole album, chosen by the
    // owner), the file's embedded picture only when the folder has none.
    for name in folder_artwork_candidates(&source.sidecar_directory()).unwrap_or_default() {
        if cancel.is_cancelled() {
            break;
        }
        if let Ok(file) = source.open_sidecar(&name) {
            if let Ok(pixels) = external_artwork(file.path(), side as u32) {
                return (Some(pixels), false);
            }
        }
    }
    if key.embedded && !cancel.is_cancelled() {
        if let Ok(mut media) = MediaFile::open(source.path(), cancel.clone()) {
            if let Ok(pixels) = media.artwork(side as u32) {
                return (Some(pixels), true);
            }
        }
    }
    (None, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A 1x1 red PNG: a picture that differs from every fixture's artwork.
    const RED: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2,
        0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0,
        3, 1, 1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    fn fixture_track(root: &Path, name: &str, id: i64, embedded: bool) -> Track {
        let path = root.join(name);
        let meta = std::fs::metadata(&path).unwrap();
        Track {
            id,
            source_id: "internal".into(),
            path,
            artwork: embedded,
            size: meta.len(),
            mtime: meta
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .min(i64::MAX as u128) as i64,
            ..Default::default()
        }
    }
    fn job(root: &Path, track: &Track, thumb: u64) -> Job {
        Job {
            thumb,
            art: Key::new(track, None, root).into(),
            cache: root.join("cache"),
        }
    }
    fn reply(worker: &mut Worker, job: Job) -> Reply {
        worker.want(vec![job]);
        worker
            .reply_within(std::time::Duration::from_secs(10))
            .unwrap()
    }
    fn unread(worker: &Worker) -> u32 {
        let (lock, _) = &*worker.shared;
        lock.lock().unwrap().unread.values().sum()
    }
    #[test]
    fn a_cover_waiting_to_be_taken_is_not_decoded_again() {
        let root = std::env::temp_dir().join(format!("reborn-thumbs-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("cache")).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
        std::fs::copy(fixtures.join("artwork.flac"), root.join("song.flac")).unwrap();
        let track = fixture_track(&root, "song.flac", 1, true);
        let mut worker = Worker::spawn().unwrap();
        // The player asks on every pass until it has taken the reply.
        for _ in 0..50 {
            worker.want(vec![job(&root, &track, 7)]);
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        // Wait for publication, retaining unread accounting until the assertion below. The worker
        // increments that count before sending, so polling it could race the channel send.
        let first = worker
            .replies
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert_eq!(unread(&worker), 1, "one reply, not one per request");
        worker.taken(&first);
        assert!(worker.try_reply().is_none());
        assert_eq!(unread(&worker), 0);
        // Once taken it may be asked for again (now from the disk cache).
        assert!(reply(&mut worker, job(&root, &track, 7)).cache_hit);
        drop(worker);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn removable_mount_and_file_revision_are_part_of_art_identity() {
        let mut t = Track {
            id: 1,
            source_id: "sd".into(),
            path: "/media/sd/a.flac".into(),
            ..Default::default()
        };
        let key = Key::new(&t, Some(10), Path::new("/data/music"));
        assert_ne!(key, Key::new(&t, Some(11), Path::new("/data/music")));
        t.mtime = 4;
        assert_ne!(key, Key::new(&t, Some(10), Path::new("/data/music")));
    }
    #[test]
    fn thumbnails_are_cached_on_disk_and_never_pin_removable_media() {
        let root = std::env::temp_dir().join(format!("reborn-thumbs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("cache")).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
        std::fs::copy(fixtures.join("artwork.flac"), root.join("song.flac")).unwrap();
        let track = fixture_track(&root, "song.flac", 1, true);
        // A thumbnail made under the old embedded-first order is discarded.
        std::fs::write(root.join("cache/thumb-0000000000000007-1-1.t96"), b"old").unwrap();
        // So is a 96x96 one from before Beta 8.18.
        std::fs::write(root.join("cache/thumb2-0000000000000007-1-1.t96"), b"old").unwrap();
        // And one named before the folder's covers were part of the name (9.3.17).
        std::fs::write(root.join("cache/thumb3-0000000000000007-1-1.t128"), b"old").unwrap();
        let mut worker = Worker::spawn().unwrap();
        let first = reply(&mut worker, job(&root, &track, 7));
        assert!(!root.join("cache/thumb-0000000000000007-1-1.t96").exists());
        assert!(!root.join("cache/thumb2-0000000000000007-1-1.t96").exists());
        assert!(!root.join("cache/thumb3-0000000000000007-1-1.t128").exists());
        let cached: Vec<_> = std::fs::read_dir(root.join("cache"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| std::fs::metadata(e.path()).unwrap().len())
            .collect();
        assert_eq!(cached, [CACHED_BYTES as u64], "one 4:2:0 file");
        let rgba = first.rgba.expect("embedded cover decodes");
        assert_eq!(rgba.len(), THUMB_SIZE * THUMB_SIZE * 4);
        assert_eq!(rgba[3], 255, "square: the look rounds it at upload");
        assert!(!first.cache_hit);
        let again = reply(&mut worker, job(&root, &track, 7));
        assert!(again.cache_hit);
        let again = again.rgba.unwrap();
        assert_eq!(again.len(), rgba.len());
        let worst = again.iter().zip(&rgba).map(|(a, b)| a.abs_diff(*b)).max();
        assert!(worst <= Some(3), "4:2:0 keeps the cover: {worst:?}");
        let open_pins = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|e| std::fs::read_link(e.path()).ok())
            .filter(|path| path.starts_with(&root))
            .count();
        assert_eq!(open_pins, 0, "thumbnail work must not keep SD eject pins");
        // A replaced file has a new revision and therefore a new cache entry.
        let mut changed = track.clone();
        changed.mtime += 1;
        assert!(!reply(&mut worker, job(&root, &changed, 7)).cache_hit);
        drop(worker);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_or_changed_covers_report_failure_and_are_not_retried() {
        let root = std::env::temp_dir().join(format!("reborn-thumbs-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("cache")).unwrap();
        std::fs::write(root.join("bad.flac"), b"bad media").unwrap();
        let bad = fixture_track(&root, "bad.flac", 5, true);
        let mut worker = Worker::spawn().unwrap();
        let r = reply(&mut worker, job(&root, &bad, 9));
        assert!(r.rgba.is_none());
        worker.record_failure(9);
        worker.want(vec![job(&root, &bad, 9)]);
        assert!(worker
            .reply_within(std::time::Duration::from_millis(300))
            .is_none());
        worker.forget_failures();
        assert!(reply(&mut worker, job(&root, &bad, 9)).rgba.is_none());
        // A source replaced in place (same size and mtime) fails the freshness check.
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
        std::fs::copy(fixtures.join("artwork.flac"), root.join("song.flac")).unwrap();
        let track = fixture_track(&root, "song.flac", 1, true);
        let key = Key::new(&track, None, &root);
        let source = key.pin().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let modified = std::fs::metadata(root.join("song.flac"))
            .unwrap()
            .modified()
            .unwrap();
        let mut bytes = std::fs::read(root.join("song.flac")).unwrap();
        bytes[0] ^= 1;
        std::fs::write(root.join("song.flac"), bytes).unwrap();
        std::fs::File::open(root.join("song.flac"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(!source.is_current());
        assert!(key.load_cover(ART_SIZE, &Cancel::new().unwrap()).is_none());
        drop(worker);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn pinned_sidecar_art_is_fresh_and_cancelled_requests_do_not_fall_through() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("reborn-art-sidecar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
        std::fs::copy(fixtures.join("tone.flac"), root.join("song.flac")).unwrap();
        std::fs::write(root.join("CoVeR.jpg"), b"bad cover").unwrap();
        std::fs::copy(
            fixtures.join("artwork-external.png"),
            root.join("FoLdEr.PNG"),
        )
        .unwrap();
        symlink(
            fixtures.join("artwork-external.jpg"),
            root.join("album.jpg"),
        )
        .unwrap();
        let key = Key::new(&fixture_track(&root, "song.flac", 1, false), None, &root);
        let old = key.load_cover(ART_SIZE, &Cancel::new().unwrap()).unwrap();
        assert_eq!(
            old,
            external_artwork(&root.join("FoLdEr.PNG"), ART_SIZE as u32).unwrap()
        );
        let cancel = Cancel::new().unwrap();
        cancel.cancel();
        assert!(cancel.is_cancelled());
        assert_eq!(
            load(&key, &key.pin().unwrap(), ART_SIZE, &cancel),
            (None, false)
        );
        std::fs::remove_file(root.join("FoLdEr.PNG")).unwrap();
        assert!(key.load_cover(ART_SIZE, &Cancel::new().unwrap()).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn folder_cover_wins_and_embedded_is_the_fallback() {
        let root = std::env::temp_dir().join(format!("reborn-art-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
        std::fs::copy(fixtures.join("artwork.flac"), root.join("song.flac")).unwrap();
        std::fs::write(root.join("cover.png"), RED).unwrap();
        let key = Key::new(&fixture_track(&root, "song.flac", 1, true), None, &root);
        let folder = key.load_cover(ART_SIZE, &Cancel::new().unwrap()).unwrap();
        assert_eq!(
            folder,
            external_artwork(&root.join("cover.png"), ART_SIZE as u32).unwrap()
        );
        let embedded = MediaFile::open(&root.join("song.flac"), Cancel::new().unwrap())
            .unwrap()
            .artwork(ART_SIZE as u32)
            .unwrap();
        assert_ne!(
            folder, embedded,
            "fixtures must differ for this test to mean anything"
        );
        std::fs::remove_file(root.join("cover.png")).unwrap();
        assert_eq!(
            key.load_cover(ART_SIZE, &Cancel::new().unwrap()).unwrap(),
            embedded
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_folder_cover_deleted_or_added_makes_a_new_thumbnail() {
        // Y2Reborn #105: cover.jpg deleted, the songs unchanged; Cover Flow kept the old picture.
        let root =
            std::env::temp_dir().join(format!("reborn-thumbs-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("cache")).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fixtures");
        std::fs::copy(fixtures.join("artwork.flac"), root.join("song.flac")).unwrap();
        std::fs::write(root.join("cover.png"), RED).unwrap();
        let track = fixture_track(&root, "song.flac", 1, true);
        let mut worker = Worker::spawn().unwrap();
        let folder = reply(&mut worker, job(&root, &track, 7));
        assert!(!folder.cache_hit);
        assert!(reply(&mut worker, job(&root, &track, 7)).cache_hit);
        std::fs::remove_file(root.join("cover.png")).unwrap();
        let embedded = reply(&mut worker, job(&root, &track, 7));
        assert!(!embedded.cache_hit, "the folder's cover is gone");
        assert_ne!(embedded.rgba, folder.rgba, "the song's own picture now");
        assert!(reply(&mut worker, job(&root, &track, 7)).cache_hit);
        std::fs::write(root.join("cover.png"), RED).unwrap();
        let back = reply(&mut worker, job(&root, &track, 7));
        assert!(!back.cache_hit, "a folder cover added again");
        assert_eq!(back.rgba, folder.rgba);
        drop(worker);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cached_thumbnails_keep_their_colours_in_half_the_bytes() {
        let n = THUMB_SIZE;
        // A smooth gradient and a flat colour block, as covers mostly are.
        let cover: Vec<u8> = (0..n * n)
            .flat_map(|i| {
                let (x, y) = (i % n, i / n);
                if x < n / 2 {
                    [(x * 4) as u8, (y * 2) as u8, 160, 255]
                } else {
                    [30, 140, 200, 255]
                }
            })
            .collect();
        let bytes = encode_420(&cover);
        assert_eq!(bytes.len(), CACHED_BYTES);
        assert_eq!(CACHED_BYTES * 2, n * n * 3);
        let back = decode_420(&bytes);
        assert_eq!(back.len(), cover.len());
        let worst = cover
            .iter()
            .zip(&back)
            .enumerate()
            // Away from the one hard colour edge in the middle.
            .filter(|(i, _)| (i / 4 % n).abs_diff(n / 2) > 2)
            .map(|(_, (a, b))| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(worst <= 3, "largest channel error {worst}");
        assert!(back.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    }
    #[test]
    fn a_full_cache_drops_the_least_recently_shown_thumbnails() {
        let root = std::env::temp_dir().join(format!("reborn-thumbs-lru-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 24 * 3600);
        for i in 0..5u64 {
            let path = root.join(format!("{THUMB_PREFIX}{i:016x}-1-1.t128"));
            std::fs::write(&path, b"x").unwrap();
            let when = old + std::time::Duration::from_secs(i * 3600);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
        std::fs::write(root.join("other.rgba"), b"not a thumbnail").unwrap();
        // Showing the oldest one makes it recent.
        touch(&root.join(format!("{THUMB_PREFIX}{:016x}-1-1.t128", 0)));
        assert_eq!(evict(&root, 2), 2);
        let mut left: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "other.rgba".to_string(),
                format!("{THUMB_PREFIX}{:016x}-1-1.t128", 0),
                format!("{THUMB_PREFIX}{:016x}-1-1.t128", 3),
                format!("{THUMB_PREFIX}{:016x}-1-1.t128", 4),
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Real logos end to end (needs the internet):
    /// `REBORN_LOGO_OUT=dir cargo test -p reborn -- --ignored live_logos`
    /// writes each `ART_SIZE` tile as raw RGBA next to a list of the stations.
    #[test]
    #[ignore]
    fn live_logos_download_decode_and_tile() {
        use reborn_platform::radio;
        let out = PathBuf::from(std::env::var("REBORN_LOGO_OUT").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("reborn-logos")
                .display()
                .to_string()
        }));
        std::fs::create_dir_all(&out).unwrap();
        let directory = radio::Directory::discover();
        let stations = directory.popular().unwrap();
        let (mut tried, mut tiles) = (0, 0);
        let mut names = String::new();
        for s in stations.iter().filter(|s| !s.favicon.is_empty()).take(24) {
            tried += 1;
            let key = s.logo_key().unwrap();
            let Ok(file) = radio::fetch_logo(&s.favicon, &out, key) else {
                names.push_str(&format!("{key:016x}\tno logo\t{}\n", s.name));
                continue;
            };
            let tile = Art::Logo(file).load(ART_SIZE, &Cancel::new().unwrap());
            if let Some(tile) = &tile {
                tiles += 1;
                std::fs::write(out.join(format!("{key:016x}.rgba")), tile).unwrap();
            }
            names.push_str(&format!(
                "{key:016x}\t{}\t{}\n",
                if tile.is_some() {
                    "tile"
                } else {
                    "undecodable"
                },
                s.name
            ));
        }
        std::fs::write(out.join("stations.tsv"), names).unwrap();
        eprintln!("{tiles} tiles from {tried} logos in {}", out.display());
        assert!(
            tiles * 2 >= tried,
            "most popular stations have a usable logo"
        );
    }
}
