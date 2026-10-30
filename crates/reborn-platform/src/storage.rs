#![forbid(unsafe_code)]
use reborn_core::{MediaSource, Source, Track};
use std::{
    fs,
    os::fd::AsRawFd,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};
use y2p_contract::lenient::Shaped;
use y2p_contract::storage::{Identity, MediaRecord, Y2MediaClaim};

pub struct PinnedTrack {
    _file: fs::File,
    _parent: fs::File,
    path: PathBuf,
    source: Source,
    logical_path: PathBuf,
    revision: fs::Metadata,
}
/// Immutable source/file evidence for caches; owns no descriptor or mount pin.
#[derive(Clone)]
pub struct TrackClaim {
    source: Source,
    logical_path: PathBuf,
    revision: fs::Metadata,
}
pub struct PinnedSidecar {
    _file: fs::File,
    path: PathBuf,
}
impl PinnedSidecar {
    pub fn path(&self) -> &Path {
        &self.path
    }
}
impl PinnedTrack {
    /// The directory descriptor is retained with the media file; callers may
    /// enumerate candidate names here, then open each with open_sidecar.
    pub fn sidecar_directory(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self._parent.as_raw_fd()))
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn mount_id(&self) -> Option<u64> {
        self.source.mount_id
    }
    pub fn claim(&self) -> TrackClaim {
        TrackClaim {
            source: self.source.clone(),
            logical_path: self.logical_path.clone(),
            revision: self.revision.clone(),
        }
    }
    pub fn matches_revision(&self, size: u64, mtime: i64) -> bool {
        self.claim().matches_revision(size, mtime)
    }
    pub fn is_current(&self) -> bool {
        self.claim().is_current()
    }
    pub fn open_sidecar(&self, name: &str) -> Result<PinnedSidecar, String> {
        let mut components = Path::new(name).components();
        let Some(std::path::Component::Normal(name)) = components.next() else {
            return Err("invalid sidecar name".into());
        };
        if components.next().is_some() {
            return Err("invalid sidecar name".into());
        }
        let file = source_file(&self._parent, name)?;
        Ok(PinnedSidecar {
            path: PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd())),
            _file: file,
        })
    }
}
impl TrackClaim {
    pub fn matches_revision(&self, size: u64, mtime: i64) -> bool {
        self.revision.len() == size
            && self
                .revision
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .is_some_and(|t| t.as_nanos().min(i64::MAX as u128) as i64 == mtime)
    }
    /// Fast freshness check for a previously admitted source. No blkid probe,
    /// helper, mutation or cached claim can qualify a replacement mount/card.
    pub fn is_current(&self) -> bool {
        let Ok(live) = fs::symlink_metadata(&self.logical_path) else {
            return false;
        };
        let pinned = &self.revision;
        if !live.is_file()
            || pinned.dev() != live.dev()
            || pinned.ino() != live.ino()
            || pinned.len() != live.len()
            || pinned.mtime() != live.mtime()
            || pinned.mtime_nsec() != live.mtime_nsec()
            || pinned.ctime() != live.ctime()
            || pinned.ctime_nsec() != live.ctime_nsec()
        {
            return false;
        }
        let mountinfo =
            parse_mountinfo(&fs::read_to_string("/proc/self/mountinfo").unwrap_or_default());
        if self.source.mount_id.is_some_and(|id| {
            !mountinfo
                .iter()
                .any(|(current, root)| *current == id && self.source.root.starts_with(root))
        }) {
            return false;
        }
        if let MediaSource::SdCard(uuid) = &self.source.kind {
            if Path::new("/etc/y2linux/capabilities.json").exists() {
                let claim = fs::read_to_string("/run/y2/media-mount.json")
                    .ok()
                    .and_then(|s| serde_json::from_str::<MediaRecord>(&s).ok());
                let boot =
                    fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
                let identity = claim.as_ref().and_then(media_claim_identity);
                let instance = claim.as_ref().and_then(|c| {
                    let device = c.device_id.as_deref()?;
                    if device.len() > 24 || !device.bytes().all(|b| b.is_ascii_digit() || b == b':')
                    {
                        return None;
                    }
                    block_instance(&PathBuf::from(format!("/sys/dev/block/{device}")))
                });
                if identity.is_none()
                    || sd_source_id(uuid, identity.as_deref()) != self.source.id
                    || !sd_claim_matches(
                        claim.as_ref(),
                        uuid,
                        self.source.mount_id,
                        boot.trim(),
                        instance.as_deref(),
                    )
                {
                    return false;
                }
            }
        }
        true
    }
}
fn source_file(parent: &fs::File, name: &std::ffi::OsStr) -> Result<fs::File, String> {
    let path = PathBuf::from(format!("/proc/self/fd/{}", parent.as_raw_fd())).join(name);
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.dev() != parent.metadata().map_err(|e| e.to_string())?.dev()
        || descriptor_mount(&file)? != descriptor_mount(parent)?
    {
        return Err("not a regular source file".into());
    }
    Ok(file)
}
fn descriptor_mount(file: &fs::File) -> Result<u64, String> {
    fs::read_to_string(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))
        .map_err(|e| e.to_string())?
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:")?.trim().parse().ok())
        .ok_or_else(|| "source descriptor mount unavailable".into())
}
fn pin_source_path(track: &Track, source: &Source) -> Result<PinnedTrack, String> {
    if !source.online || source.id != track.source_id {
        return Err("track source unavailable".into());
    }
    let relative = track
        .path
        .strip_prefix(&source.root)
        .map_err(|_| "track outside source")?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("track outside source".into());
    }
    let mut root = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&source.root)
        .map_err(|e| e.to_string())?;
    if !root.metadata().map_err(|e| e.to_string())?.is_dir() {
        return Err("track source unavailable".into());
    }
    let mount = descriptor_mount(&root)?;
    if source.mount_id.is_some_and(|expected| mount != expected) {
        return Err("track source mount changed".into());
    }
    let device = root.metadata().map_err(|e| e.to_string())?.dev();
    let components = relative.components().collect::<Vec<_>>();
    for component in &components[..components.len() - 1] {
        let path = PathBuf::from(format!("/proc/self/fd/{}", root.as_raw_fd()))
            .join(component.as_os_str());
        let next = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| e.to_string())?;
        if next.metadata().map_err(|e| e.to_string())?.dev() != device
            || descriptor_mount(&next)? != mount
        {
            return Err("track crosses source filesystem".into());
        }
        root = next;
    }
    let file = source_file(&root, components.last().unwrap().as_os_str())?;
    Ok(PinnedTrack {
        revision: file.metadata().map_err(|e| e.to_string())?,
        path: PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd())),
        _file: file,
        _parent: root,
        source: source.clone(),
        logical_path: track.path.clone(),
    })
}
/// Validate the live card claim and pin a regular file before decoder admission.
/// Keep this value alive for the decoder's lifetime, including queued tracks.
pub fn pin_track(track: &Track, music: &Path) -> Result<PinnedTrack, String> {
    let current_sources = || {
        let mut current = sources(music);
        // Desktop/headless fixtures use an explicit directory without Y2DATA.
        // Managed Y2 media always requires its real platform mount/claim.
        if !platform_manages_media() && !current.iter().any(|s| s.id == "internal") {
            current.push(Source {
                id: "internal".into(),
                kind: MediaSource::Internal,
                root: music.into(),
                mount: "configured directory".into(),
                mount_id: None,
                online: music.is_dir(),
            });
        }
        current
    };
    let source = current_sources()
        .into_iter()
        .find(|s| s.online && s.id == track.source_id)
        .ok_or("track source unavailable")?;
    let pinned = pin_source_path(track, &source)?;
    if !current_sources().contains(&source) {
        return Err("track source changed during open".into());
    }
    Ok(pinned)
}
#[derive(Clone, Debug)]
pub struct Mount {
    pub device: String,
    pub path: PathBuf,
    pub filesystem: String,
    pub options: String,
}
fn unescape(s: &str) -> String {
    s.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}
pub fn parse_mounts(s: &str) -> Vec<Mount> {
    s.lines()
        .filter_map(|l| {
            let v = l.split_whitespace().collect::<Vec<_>>();
            if v.len() < 4 {
                return None;
            }
            Some(Mount {
                device: unescape(v[0]),
                path: unescape(v[1]).into(),
                filesystem: v[2].into(),
                options: v[3].into(),
            })
        })
        .collect()
}
pub fn parse_mountinfo(s: &str) -> Vec<(u64, PathBuf)> {
    s.lines()
        .filter_map(|line| {
            let (mount, _) = line.split_once(" - ")?;
            let fields = mount.split_whitespace().collect::<Vec<_>>();
            Some((
                fields.first()?.parse().ok()?,
                unescape(fields.get(4)?).into(),
            ))
        })
        .collect()
}
pub fn mounts() -> Vec<Mount> {
    parse_mounts(&fs::read_to_string("/proc/mounts").unwrap_or_default())
}
#[derive(Clone, PartialEq, Eq)]
struct MountUuidKey {
    device: String,
    mount: u64,
    instance: String,
}

#[derive(Default)]
struct MountUuidCache(Vec<(MountUuidKey, String)>);
impl MountUuidCache {
    fn probe(
        &mut self,
        key: MountUuidKey,
        probe: impl FnOnce() -> Option<String>,
    ) -> Option<String> {
        if let Some((_, uuid)) = self.0.iter().find(|(old, _)| old == &key) {
            return Some(uuid.clone());
        }
        self.0.retain(|(old, _)| old.device != key.device);
        let uuid = probe()?;
        if self.0.len() >= 4 {
            self.0.remove(0);
        }
        self.0.push((key, uuid.clone()));
        Some(uuid)
    }
}

fn mounted_uuid(device: &str, mount: Option<u64>) -> Option<String> {
    // Mount ID and the kernel's diskseq/CID-qualified block lifetime are checked
    // on every observation. Only the immutable UUID probe can be reused; cached
    // evidence never admits a replacement mount/card or overrides live claims.
    let instance = block_instance(&PathBuf::from(format!(
        "/sys/class/block/{}",
        Path::new(device).file_name()?.to_str()?
    )));
    let (Some(mount), Some(instance)) = (mount, instance) else {
        return super::filesystem_uuid(device);
    };
    static CACHE: OnceLock<Mutex<MountUuidCache>> = OnceLock::new();
    let key = MountUuidKey {
        device: device.into(),
        mount,
        instance,
    };
    CACHE
        .get_or_init(|| Mutex::new(MountUuidCache::default()))
        .lock()
        .ok()?
        .probe(key, || super::filesystem_uuid(device))
}

/// Y2Media: the internal user-media partition (eMMC p8), mounted by the platform.
pub const INTERNAL_MEDIA: &str = "/media/internal";
const INTERNAL_MEDIA_MUSIC: &str = "/media/internal/music";
const LEGACY_MUSIC: &str = "/data/music";

/// The mount that backs an internal music directory: Y2Media for its own folder, Y2DATA otherwise.
pub fn internal_mount(music: &Path) -> &'static str {
    if music.starts_with(INTERNAL_MEDIA) {
        INTERNAL_MEDIA
    } else {
        "/data"
    }
}

/// Choose the internal music directory from what the platform has recorded. Pure so the
/// decision is testable: Y2Media only while its Ready record names this boot and this very mount.
fn choose_internal_music(marker: &str, boot_id: &str, mountinfo: &[(u64, PathBuf)]) -> PathBuf {
    let claim = serde_json::from_str::<Y2MediaClaim>(marker).unwrap_or_default();
    let current = mountinfo
        .iter()
        .filter(|(_, path)| path == Path::new(INTERNAL_MEDIA))
        .collect::<Vec<_>>();
    let ready = claim.state.as_deref() == Some("Ready")
        && !boot_id.is_empty()
        && claim.boot_id.is_some_and(|id| id.is(Some(boot_id)))
        && current.len() == 1
        && claim.mount_id.and_then(|id| u64::try_from(id).ok()) == Some(current[0].0);
    PathBuf::from(if ready {
        INTERNAL_MEDIA_MUSIC
    } else {
        LEGACY_MUSIC
    })
}

/// Internal music lives on Y2Media once the platform has mounted it, and on Y2DATA before that
/// (an image or device that has not been provisioned yet keeps working).
pub fn internal_music_directory() -> PathBuf {
    choose_internal_music(
        &fs::read_to_string("/run/y2/internal-media.json").unwrap_or_default(),
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap_or_default()
            .trim(),
        &parse_mountinfo(&fs::read_to_string("/proc/self/mountinfo").unwrap_or_default()),
    )
}

pub fn sources(music: &Path) -> Vec<Source> {
    let m = mounts();
    let mountinfo =
        parse_mountinfo(&fs::read_to_string("/proc/self/mountinfo").unwrap_or_default());
    let mut result = vec![];
    for (mount_path, internal) in [(internal_mount(music), true), ("/media/sd", false)] {
        if let Some(m) = m.iter().find(|m| m.path == Path::new(mount_path)) {
            if !internal && !Path::new(&m.device).exists() {
                continue;
            }
            let observed_mount = mountinfo
                .iter()
                .find_map(|(id, path)| (path == Path::new(mount_path)).then_some(*id));
            let mut uuid = mounted_uuid(&m.device, observed_mount)
                .unwrap_or_else(|| format!("device:{}", m.device));
            let mut sd_identity = None;
            if !internal && Path::new("/etc/y2linux/capabilities.json").exists() {
                let claim = fs::read_to_string("/run/y2/media-mount.json")
                    .ok()
                    .and_then(|s| serde_json::from_str::<MediaRecord>(&s).ok());
                let boot =
                    fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
                if uuid.starts_with("device:") {
                    if let Some(identity) = claim.as_ref().and_then(media_claim_identity) {
                        uuid = identity;
                    }
                }
                let instance = claim.as_ref().and_then(|v| {
                    let device = v.device_id.as_deref()?;
                    if device.len() > 24 || !device.bytes().all(|b| b.is_ascii_digit() || b == b':')
                    {
                        return None;
                    }
                    block_instance(&PathBuf::from(format!("/sys/dev/block/{device}")))
                });
                if !sd_claim_matches(
                    claim.as_ref(),
                    &uuid,
                    observed_mount,
                    boot.trim(),
                    instance.as_deref(),
                ) {
                    continue;
                }
                sd_identity = claim.as_ref().and_then(media_claim_identity);
                if sd_identity.is_none() {
                    continue;
                }
            }
            result.push(Source {
                id: if internal {
                    "internal".into()
                } else {
                    sd_source_id(&uuid, sd_identity.as_deref())
                },
                kind: if internal {
                    MediaSource::Internal
                } else {
                    MediaSource::SdCard(uuid)
                },
                root: if internal {
                    music.into()
                } else {
                    mount_path.into()
                },
                online: true,
                mount: m.device.clone(),
                mount_id: observed_mount,
            });
        }
    }
    result
}
fn block_instance(entry: &Path) -> Option<String> {
    let entry = fs::canonicalize(entry).ok()?;
    let disk = if entry.join("partition").is_file() {
        entry.parent()?
    } else {
        &entry
    };
    let dev = fs::read_to_string(entry.join("dev")).ok()?;
    let dev = dev.trim();
    let (major, minor) = dev.split_once(':')?;
    if major.is_empty()
        || minor.is_empty()
        || !major
            .bytes()
            .chain(minor.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let seq = fs::read_to_string(disk.join("diskseq")).ok()?;
    let seq = seq.trim();
    if seq.parse::<u64>().ok()? == 0 || !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("block:{dev}:diskseq:{seq}"))
}
fn sd_claim_matches(
    claim: Option<&MediaRecord>,
    uuid: &str,
    mount: Option<u64>,
    boot: &str,
    instance: Option<&str>,
) -> bool {
    claim.is_some_and(|c| {
        c.schema == Some(1)
            && !boot.is_empty()
            && c.of_boot(Some(boot))
            && (c.uuid.as_deref() == Some(uuid)
                || (c.uuid.is_none()
                    && identity(c).is_some_and(|i| uuid == format!("cid:{}", i.id))))
            && mount.is_some()
            && c.mount_id.and_then(|id| u64::try_from(id).ok()) == mount
            && c.state_is("Ready")
            && instance.is_some()
            && c.source_instance.as_deref() == instance
    })
}
/// The claim's card identity (none without one, or of another shape).
fn identity(claim: &MediaRecord) -> Option<&Identity> {
    match &claim.identity {
        Some(Shaped::Is(identity)) => Some(identity),
        _ => None,
    }
}
fn media_claim_identity(claim: &MediaRecord) -> Option<String> {
    let device = claim.device_id.as_deref()?;
    if !device.bytes().all(|b| b.is_ascii_digit() || b == b':') || device.len() > 24 {
        return None;
    }
    let entry = fs::canonicalize(format!("/sys/dev/block/{device}")).ok()?;
    if !entry.to_string_lossy().contains("/11240000.mmc/") {
        return None;
    }
    media_claim_identity_at(claim, &entry)
}
fn media_claim_identity_at(claim: &MediaRecord, entry: &Path) -> Option<String> {
    let identity = identity(claim)?;
    for (field, recorded) in [
        ("start", &identity.geometry.start),
        ("size", &identity.geometry.size),
    ] {
        let current = if field == "start" && !entry.join("partition").exists() {
            "0".into()
        } else {
            fs::read_to_string(entry.join(field)).ok()?
        };
        if recorded.as_deref() != Some(current.trim()) {
            return None;
        }
    }
    let disk = if entry.join("partition").exists() {
        entry.parent()?
    } else {
        entry
    };
    let cid = fs::read_to_string(disk.join("device/cid")).ok()?;
    if identity.cid.as_deref() != Some(cid.trim()) {
        return None;
    }
    let id = &identity.id;
    (id.len() == 64 && id.bytes().all(|c| c.is_ascii_hexdigit())).then(|| format!("cid:{id}"))
}
fn sd_source_id(uuid: &str, identity: Option<&str>) -> String {
    // Duplicate filesystem UUIDs on different cards must not alias a library.
    match identity {
        Some(id) if id != uuid => format!("uuid:{uuid}:{id}"),
        _ => format!("uuid:{uuid}"),
    }
}
pub fn platform_manages_media() -> bool {
    Path::new("/etc/y2linux/platform-contract").exists()
}
pub fn data_ready() -> bool {
    mounts()
        .iter()
        .any(|m| m.path == Path::new("/data") && m.filesystem == "ext4")
        && fs::read_to_string("/data/.y2data-schema").is_ok_and(|s| s.trim() == "1")
}
pub fn unmount_sd() -> Result<(), String> {
    helper("unmount")
}
pub fn mount_sd() -> Result<(), String> {
    if mounts().iter().any(|m| m.path == Path::new("/media/sd")) {
        return Ok(());
    }
    helper("mount")
}
fn helper(action: &str) -> Result<(), String> {
    // The platform owns filesystem selection/partition policy; no arbitrary shell or device argument.
    let mut child = Command::new("/usr/sbin/y2-media")
        .arg(action)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err("no unique supported SD filesystem".into())
            };
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            return Err("mount helper timeout".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}
pub fn sd_present() -> bool {
    sd_present_in(Path::new("/sys/class/block"))
}
/// A class entry is a link into /sys/devices: its target names the
/// controller, so one readlink per entry does (canonicalize walked every
/// component of every block device, the eMMC's many partitions too, each
/// observation). Anything that is no link is still resolved in full.
fn sd_present_in(class: &Path) -> bool {
    const SD_HOST: &str = "/11240000.mmc/";
    fs::read_dir(class)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|e| match fs::read_link(e.path()) {
            Ok(target) => target.to_string_lossy().contains(SD_HOST),
            Err(_) => {
                fs::canonicalize(e.path()).is_ok_and(|p| p.to_string_lossy().contains(SD_HOST))
            }
        })
}

/// Durable changes only: incomplete copies stay hidden and never trigger a scan.
pub fn usb_changes() -> (u64, bool) {
    let receipt = fs::read_to_string("/run/y2/usb-transfer.json")
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    receipt
        .filter(|r| {
            r["schema"] == y2p_contract::schema::USB_TRANSFER
                && !boot.trim().is_empty()
                && r["boot_id"].as_str() == Some(boot.trim())
        })
        .map(|r| {
            (
                r["refresh_generation"].as_u64().unwrap_or(0),
                r["active"] == true,
            )
        })
        .unwrap_or((0, false))
}

#[derive(Default)]
pub struct TransferRefresh {
    seen: u64,
    requested: u64,
    quiet_since: Option<Instant>,
    copying: bool,
}
impl TransferRefresh {
    pub fn observe(&mut self, generation: u64, copying: bool, now: Instant) {
        if generation != 0 && generation != self.seen {
            self.seen = generation;
            self.quiet_since = Some(now);
        }
        if self.copying && !copying {
            self.quiet_since = Some(now);
        }
        self.copying = copying;
    }
    pub fn ready(
        &self,
        scanner_busy: bool,
        media_ready: bool,
        sleep_prepared: bool,
        now: Instant,
    ) -> bool {
        self.seen != self.requested
            && !sleep_prepared
            && !self.copying
            && !scanner_busy
            && media_ready
            && self
                .quiet_since
                .is_some_and(|start| now.duration_since(start) >= Duration::from_secs(2))
    }
    pub fn requested(&mut self) {
        self.requested = self.seen;
    }
}
#[cfg(test)]
mod tests {
    /// The SD card is the block device linked below its controller, as
    /// /sys/class/block links point; eMMC partitions and loops are not.
    #[test]
    fn sd_card_found_by_its_class_link() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("reborn-sd-class-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let class = root.join("class/block");
        let devices = root.join("devices/platform");
        for dir in [
            "11230000.mmc/mmc_host/mmc0/mmc0:0001/block/mmcblk0/mmcblk0p1",
            "11240000.mmc/mmc_host/mmc1/mmc1:aaaa/block/mmcblk1",
            "virtual/block/loop0",
        ] {
            std::fs::create_dir_all(devices.join(dir)).unwrap();
        }
        std::fs::create_dir_all(&class).unwrap();
        let link = |target: &str, name: &str| {
            symlink(format!("../../devices/platform/{target}"), class.join(name)).unwrap()
        };
        link(
            "11230000.mmc/mmc_host/mmc0/mmc0:0001/block/mmcblk0/mmcblk0p1",
            "mmcblk0p1",
        );
        link("virtual/block/loop0", "loop0");
        assert!(!super::sd_present_in(&class));
        link(
            "11240000.mmc/mmc_host/mmc1/mmc1:aaaa/block/mmcblk1",
            "mmcblk1",
        );
        assert!(super::sd_present_in(&class));
        std::fs::remove_file(class.join("mmcblk1")).unwrap();
        assert!(!super::sd_present_in(&class));
        // An entry that is a directory, not a link, is resolved in full.
        let plain = devices.join("11240000.mmc/mmc_host/mmc1/mmc1:aaaa/block");
        assert!(super::sd_present_in(&plain));
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn uuid_probe_reused_only_for_same_mount_and_block_lifetime() {
        use super::{MountUuidCache, MountUuidKey};
        let mut cache = MountUuidCache::default();
        let mut calls = 0;
        let mut key = MountUuidKey {
            device: "/dev/mmcblk1p1".into(),
            mount: 35,
            instance: "block:179:9:diskseq:4".into(),
        };
        assert_eq!(
            cache.probe(key.clone(), || {
                calls += 1;
                Some("old".into())
            }),
            Some("old".into())
        );
        assert_eq!(
            cache.probe(key.clone(), || {
                calls += 1;
                Some("ignored".into())
            }),
            Some("old".into())
        );
        assert_eq!(calls, 1);
        key.mount = 36;
        assert_eq!(
            cache.probe(key.clone(), || {
                calls += 1;
                Some("remount".into())
            }),
            Some("remount".into())
        );
        key.instance = "block:179:9:diskseq:5".into();
        assert_eq!(
            cache.probe(key.clone(), || {
                calls += 1;
                Some("replacement".into())
            }),
            Some("replacement".into())
        );
        key.mount = 37;
        assert_eq!(cache.probe(key.clone(), || None), None);
        assert_eq!(
            cache.probe(key, || {
                calls += 1;
                Some("recovered".into())
            }),
            Some("recovered".into())
        );
        assert_eq!(calls, 4);
        assert_eq!(cache.0.len(), 1);
    }
    use super::*;
    #[test]
    fn transfer_refresh_coalesces_and_retries_after_busy_or_unstable_media() {
        let mut refresh = TransferRefresh::default();
        let now = Instant::now();
        refresh.observe(5, true, now);
        assert!(!refresh.ready(false, true, false, now + Duration::from_secs(10)));
        refresh.observe(6, false, now + Duration::from_secs(10));
        assert!(!refresh.ready(false, true, false, now + Duration::from_secs(11)));
        assert!(!refresh.ready(true, true, false, now + Duration::from_secs(12)));
        assert!(!refresh.ready(false, false, false, now + Duration::from_secs(12)));
        assert!(refresh.ready(false, true, false, now + Duration::from_secs(12)));
        refresh.requested();
        assert!(!refresh.ready(false, true, false, now + Duration::from_secs(15)));
        refresh.observe(7, false, now + Duration::from_secs(16));
        assert!(refresh.ready(false, true, false, now + Duration::from_secs(18)));
    }
    #[test]
    fn durable_usb_refresh_waits_for_restore_without_losing_its_generation() {
        let mut refresh = TransferRefresh::default();
        let now = Instant::now();
        refresh.observe(23, false, now);
        assert!(!refresh.ready(false, true, true, now + Duration::from_secs(3)));
        assert!(refresh.ready(false, true, false, now + Duration::from_secs(3)));
        refresh.requested();
        assert!(!refresh.ready(false, true, false, now + Duration::from_secs(4)));
    }
    #[test]
    fn mount_names_not_block_numbers() {
        let m = parse_mounts(
            "/dev/mmcblk1p7 /data ext4 rw 0 0\n/dev/mmcblk0p1 /media/sd vfat rw 0 0\n",
        );
        assert_eq!(m[0].path, Path::new("/data"));
        assert_eq!(m[1].device, "/dev/mmcblk0p1");
    }
    #[test]
    fn internal_music_uses_y2media_only_while_the_platform_record_matches_the_live_mount() {
        let mounted = vec![
            (9, PathBuf::from("/data")),
            (12, PathBuf::from("/media/internal")),
        ];
        let ready = r#"{"state":"Ready","boot_id":"b1","mount_id":12}"#;
        assert_eq!(
            choose_internal_music(ready, "b1", &mounted),
            Path::new("/media/internal/music")
        );
        // A remount (new id), another boot, a failed record, no record or a missing mount fall back.
        let legacy = Path::new("/data/music");
        assert_eq!(choose_internal_music(ready, "b2", &mounted), legacy);
        assert_eq!(choose_internal_music(ready, "", &mounted), legacy);
        assert_eq!(
            choose_internal_music(
                r#"{"state":"Ready","boot_id":"b1","mount_id":13}"#,
                "b1",
                &mounted
            ),
            legacy
        );
        assert_eq!(
            choose_internal_music(
                r#"{"state":"Failed","boot_id":"b1","mount_id":12}"#,
                "b1",
                &mounted
            ),
            legacy
        );
        assert_eq!(choose_internal_music("", "b1", &mounted), legacy);
        assert_eq!(choose_internal_music("not json", "b1", &mounted), legacy);
        assert_eq!(choose_internal_music(ready, "b1", &mounted[..1]), legacy);
        let stacked = vec![
            (12, PathBuf::from("/media/internal")),
            (13, PathBuf::from("/media/internal")),
        ];
        assert_eq!(choose_internal_music(ready, "b1", &stacked), legacy);
    }
    #[test]
    fn the_internal_source_is_backed_by_the_mount_that_holds_its_music() {
        assert_eq!(
            internal_mount(Path::new("/media/internal/music")),
            "/media/internal"
        );
        assert_eq!(internal_mount(Path::new("/data/music")), "/data");
        assert_eq!(internal_mount(Path::new("/media/internalx/music")), "/data");
    }
    #[test]
    fn escaped_mount() {
        assert_eq!(
            parse_mounts("/dev/a /media/a\\040b ext4 rw 0 0")[0].path,
            Path::new("/media/a b")
        );
    }

    #[test]
    fn mountinfo_keeps_mount_generation_and_decodes_mountpoint() {
        let parsed = parse_mountinfo(
            "36 25 179:7 / /media/sd\\040card rw,relatime shared:4 - vfat /dev/mmcblk0p1 rw\n",
        );
        assert_eq!(parsed, vec![(36, PathBuf::from("/media/sd card"))]);
    }
    #[test]
    fn disk_instance_survives_kernfs_recreation_and_detects_same_card_reinsert() {
        let root = std::env::temp_dir().join(format!("reborn-storage-{}", std::process::id()));
        let entry = root.join("disk/partition");
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("partition"), "1").unwrap();
        fs::write(entry.join("dev"), "179:9\n").unwrap();
        fs::write(root.join("disk/diskseq"), "17\n").unwrap();
        let before = block_instance(&entry).unwrap();
        fs::remove_dir_all(&entry).unwrap();
        fs::create_dir(&entry).unwrap();
        fs::write(entry.join("partition"), "1").unwrap();
        fs::write(entry.join("dev"), "179:9\n").unwrap();
        assert_eq!(block_instance(&entry).as_deref(), Some(before.as_str()));
        fs::write(root.join("disk/diskseq"), "18").unwrap();
        assert_ne!(block_instance(&entry).as_deref(), Some(before.as_str()));
        fs::remove_file(root.join("disk/diskseq")).unwrap();
        assert!(block_instance(&entry).is_none());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn uuidless_whole_card_and_geometry_claims_are_checked() {
        let root =
            std::env::temp_dir().join(format!("reborn-storage-claims-{}", std::process::id()));
        fs::create_dir_all(root.join("device")).unwrap();
        fs::write(root.join("device/cid"), "card-one\n").unwrap();
        fs::write(root.join("size"), "1000000\n").unwrap();
        let id = "a".repeat(64);
        let claim: MediaRecord = serde_json::from_value(serde_json::json!({"schema":1,
            "state":"Ready","uuid":null,"boot_id":"boot-one","mount_id":42,
            "source_instance":"block:179:8:diskseq:17",
            "identity":{"id":id,"cid":"card-one","filesystem":"exfat","uuid":null,
                "geometry":{"partition":null,"start":"0","size":"1000000"}}}))
        .unwrap();
        let identity = media_claim_identity_at(&claim, &root).unwrap();
        assert!(sd_claim_matches(
            Some(&claim),
            &identity,
            Some(42),
            "boot-one",
            Some("block:179:8:diskseq:17")
        ));
        fs::write(root.join("size"), "1000001").unwrap();
        assert!(media_claim_identity_at(&claim, &root).is_none());
        fs::write(root.join("size"), "1000000").unwrap();
        fs::write(root.join("device/cid"), "card-two").unwrap();
        assert!(media_claim_identity_at(&claim, &root).is_none());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn duplicate_uuid_cards_have_distinct_library_sources() {
        assert_eq!(sd_source_id("cid:one", Some("cid:one")), "uuid:cid:one");
        assert_ne!(
            sd_source_id("clone-uuid", Some("cid:one")),
            sd_source_id("clone-uuid", Some("cid:two"))
        );
        assert_eq!(
            sd_source_id("clone-uuid", Some("cid:one")),
            sd_source_id("clone-uuid", Some("cid:one"))
        );
    }
    #[test]
    fn new_platform_requires_matching_card_claim_and_boot() {
        let claim: MediaRecord = serde_json::from_value(serde_json::json!({"schema":1,
            "state":"Ready","uuid":"card-one","boot_id":"boot-one","mount_id":42,
            "source_instance":"1:2"}))
        .unwrap();
        assert!(!sd_claim_matches(
            Some(&claim),
            "card-one",
            Some(42),
            "boot-one",
            Some("3:4")
        ));
        assert!(sd_claim_matches(
            Some(&claim),
            "card-one",
            Some(42),
            "boot-one",
            Some("1:2")
        ));
        assert!(!sd_claim_matches(
            Some(&claim),
            "card-two",
            Some(42),
            "boot-one",
            Some("1:2")
        ));
        assert!(!sd_claim_matches(
            Some(&claim),
            "card-one",
            Some(43),
            "boot-one",
            Some("1:2")
        ));
        assert!(!sd_claim_matches(
            Some(&claim),
            "card-one",
            Some(42),
            "boot-two",
            Some("1:2")
        ));
        assert!(!sd_claim_matches(
            None,
            "card-one",
            Some(42),
            "boot-one",
            Some("1:2")
        ));
    }
}
#[test]
fn playback_pin_rejects_stale_card_id_mount_and_path_escape() {
    let root = std::env::temp_dir().join(format!("reborn-source-pin-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("song.flac"), b"original card").unwrap();
    let source = Source {
        id: "uuid:volume:cid:new".into(),
        kind: MediaSource::SdCard("volume".into()),
        root: root.clone(),
        mount: "fixture".into(),
        mount_id: None,
        online: true,
    };
    let mut track = Track {
        source_id: "uuid:volume".into(),
        path: root.join("song.flac"),
        ..Default::default()
    };
    assert!(pin_source_path(&track, &source).is_err());
    track.source_id = "uuid:volume:cid:old".into();
    assert!(pin_source_path(&track, &source).is_err());
    track.source_id = source.id.clone();
    let wrong_mount = Source {
        mount_id: Some(u64::MAX),
        ..source.clone()
    };
    assert!(pin_source_path(&track, &wrong_mount).is_err());
    track.path = root.join("../outside.flac");
    assert!(pin_source_path(&track, &source).is_err());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn playback_file_pin_keeps_original_after_file_and_directory_replacement() {
    let root = std::env::temp_dir().join(format!("reborn-path-pin-{}", std::process::id()));
    let media = root.join("media");
    fs::create_dir_all(&media).unwrap();
    fs::write(media.join("音楽.flac"), b"original card").unwrap();
    let track = Track {
        source_id: "internal".into(),
        path: media.join("音楽.flac"),
        ..Default::default()
    };
    let source = Source {
        id: "internal".into(),
        kind: MediaSource::Internal,
        root: media.clone(),
        mount: "fixture".into(),
        mount_id: None,
        online: true,
    };
    let pinned = pin_source_path(&track, &source).unwrap();
    fs::remove_file(&track.path).unwrap();
    fs::write(&track.path, b"replacement file").unwrap();
    assert_eq!(fs::read(pinned.path()).unwrap(), b"original card");
    fs::rename(&media, root.join("detached")).unwrap();
    fs::create_dir(&media).unwrap();
    fs::write(media.join("音楽.flac"), b"replacement card").unwrap();
    assert_eq!(fs::read(pinned.path()).unwrap(), b"original card");
    assert_eq!(fs::read(&track.path).unwrap(), b"replacement card");
    drop(pinned);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn playback_pin_refuses_symlinks_subdirectories_and_special_files() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let root = std::env::temp_dir().join(format!("reborn-symlink-pin-{}", std::process::id()));
    let media = root.join("media");
    let outside = root.join("outside");
    fs::create_dir_all(&media).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("song.flac"), b"outside source").unwrap();
    fs::write(media.join("song.flac"), b"inside source").unwrap();
    let source = Source {
        id: "internal".into(),
        kind: MediaSource::Internal,
        root: media.clone(),
        mount: "fixture".into(),
        mount_id: None,
        online: true,
    };
    symlink(outside.join("song.flac"), media.join("file.flac")).unwrap();
    symlink(&outside, media.join("subdir")).unwrap();
    symlink(&media, root.join("root-link")).unwrap();
    let socket = UnixListener::bind(media.join("socket.flac")).unwrap();
    assert!(Command::new("mkfifo")
        .arg(media.join("fifo.flac"))
        .status()
        .unwrap()
        .success());
    for name in ["file.flac", "subdir/song.flac", "socket.flac", "fifo.flac"] {
        let track = Track {
            source_id: source.id.clone(),
            path: media.join(name),
            ..Default::default()
        };
        assert!(pin_source_path(&track, &source).is_err(), "{name}");
    }
    let pinned = pin_source_path(
        &Track {
            source_id: source.id.clone(),
            path: media.join("song.flac"),
            ..Default::default()
        },
        &source,
    )
    .unwrap();
    for name in [
        "file.flac",
        "../outside/song.flac",
        "subdir/song.flac",
        "socket.flac",
        "fifo.flac",
    ] {
        assert!(pinned.open_sidecar(name).is_err(), "{name}");
    }
    let file = pinned.open_sidecar("song.flac").unwrap();
    assert_eq!(fs::read(file.path()).unwrap(), b"inside source");
    let linked_source = Source {
        root: root.join("root-link"),
        ..source
    };
    let track = Track {
        source_id: linked_source.id.clone(),
        path: linked_source.root.join("file.flac"),
        ..Default::default()
    };
    assert!(pin_source_path(&track, &linked_source).is_err());
    drop(socket);
    fs::remove_dir_all(root).unwrap();
}
