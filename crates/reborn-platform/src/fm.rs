//! FM radio: the MT6627 tuner of the Y2's connectivity chip (Y2Linux Beta 8).
//!
//! The kernel owns the chip (`/sys/devices/platform/18070000.connectivity/fm`:
//! `power`, `frequency` in kHz, `signal`, `scan`, `state`); the tuner's sound
//! arrives as the ALSA capture "Y2 FM Capture" (48 kHz stereo). Like internet
//! radio, a [`Stream`] hands the decoder a pipe: `arecord` writes a streaming
//! WAV into it, so FM plays through the same path as everything else (wired
//! headphones or Bluetooth, volume, pause). The wired headphone cable is the
//! aerial. Stations are found by the tuner's own quality decision (soft-mute
//! tune); there is no RDS yet.
use reborn_core::RadioStation;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

/// Station addresses: `fm:98100` is 98.1 MHz.
pub const SCHEME: &str = "fm:";
pub const KHZ_MIN: u32 = 87_500;
pub const KHZ_MAX: u32 = 108_000;
/// Scan raster (kHz). 100 kHz is the European/US grid of the stock Y2.
pub const SCAN_STEP: u32 = 100;
const ARECORD: &str = "/usr/bin/arecord";
const CAPTURE_NAME: &str = "Y2 FM Capture";
/// The tuner stays on this long after a stream ends, so moving to the next
/// station does not power-cycle it.
const POWER_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// No FM tuner interface (an older Y2Linux kernel).
    NoTuner,
    /// The tuner's two firmware files from the owner's system.img are missing.
    NeedsFirmware,
    Ready,
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub sysfs: PathBuf,
    pub firmware: PathBuf,
    pub pcm: PathBuf,
    pub arecord: PathBuf,
}
impl Default for Paths {
    fn default() -> Self {
        Self {
            sysfs: "/sys/devices/platform/18070000.connectivity/fm".into(),
            firmware: "/lib/firmware/mediatek/mt6582".into(),
            pcm: "/proc/asound/pcm".into(),
            arecord: ARECORD.into(),
        }
    }
}

fn khz_valid(khz: u32) -> bool {
    (KHZ_MIN..=KHZ_MAX).contains(&khz) && khz.is_multiple_of(50)
}

/// "98.1 MHz" (two decimals only on the 50 kHz raster: "98.15 MHz").
pub fn mhz(khz: u32) -> String {
    if khz.is_multiple_of(100) {
        format!("{}.{} MHz", khz / 1000, khz % 1000 / 100)
    } else {
        format!("{}.{:02} MHz", khz / 1000, khz % 1000 / 10)
    }
}

/// The FM station for a frequency, as the radio screens show it.
pub fn station(khz: u32) -> RadioStation {
    RadioStation {
        name: format!("FM {}", mhz(khz)),
        url: format!("{SCHEME}{khz}"),
        ..RadioStation::default()
    }
}

/// The frequency of an `fm:` station address.
pub fn station_khz(url: &str) -> Option<u32> {
    url.strip_prefix(SCHEME)?
        .parse()
        .ok()
        .filter(|k| khz_valid(*k))
}

pub fn is_fm(url: &str) -> bool {
    url.starts_with(SCHEME)
}

/// Saved scan result (`/data/reborn/state/fm.json`): the stations and whether
/// a scan ever completed, so a player that receives nothing hides FM.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub scanned: bool,
    pub stations: Vec<u32>,
}
pub fn load(path: &Path) -> Saved {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Saved>(&b).ok())
        .map(|mut s| {
            s.stations.retain(|k| khz_valid(*k));
            s.stations.dedup();
            s
        })
        .unwrap_or_default()
}
pub fn save(path: &Path, saved: &Saved) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&tmp, serde_json::to_vec(saved).map_err(io::Error::other)?)?;
    fs::rename(tmp, path)
}

/// Strongest of neighbours: a strong transmitter can also pass the quality
/// decision one raster step beside its own frequency.
pub fn strongest(found: &[(u32, i32)], step: u32) -> Vec<u32> {
    let mut out: Vec<(u32, i32)> = vec![];
    for &(khz, dbm) in found {
        match out.last_mut() {
            Some(last) if khz - last.0 <= step => {
                if dbm > last.1 {
                    *last = (khz, dbm);
                }
            }
            _ => out.push((khz, dbm)),
        }
    }
    out.into_iter().map(|(k, _)| k).collect()
}

impl Paths {
    pub fn availability(&self) -> Availability {
        if !self.sysfs.join("power").exists() {
            return Availability::NoTuner;
        }
        let present = ["mt6627_fm_v1_patch.bin", "mt6627_fm_v1_coeff.bin"]
            .iter()
            .all(|n| fs::metadata(self.firmware.join(n)).is_ok_and(|m| m.is_file() && m.len() > 0));
        if present {
            Availability::Ready
        } else {
            Availability::NeedsFirmware
        }
    }
    fn write(&self, name: &str, value: &str) -> io::Result<()> {
        fs::write(self.sysfs.join(name), value)
    }
    pub fn powered(&self) -> bool {
        fs::read_to_string(self.sysfs.join("power")).is_ok_and(|s| s.trim() == "1")
    }
    pub fn power(&self, on: bool) -> io::Result<()> {
        if on == self.powered() {
            return Ok(());
        }
        self.write("power", if on { "1" } else { "0" })
    }
    pub fn tune(&self, khz: u32) -> io::Result<()> {
        if !khz_valid(khz) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not an FM frequency",
            ));
        }
        self.write("frequency", &khz.to_string())
    }
    /// dBm and stereo of the station tuned now.
    pub fn signal(&self) -> Option<(i32, bool)> {
        let text = fs::read_to_string(self.sysfs.join("signal")).ok()?;
        let mut dbm = None;
        let mut stereo = None;
        for part in text.split_whitespace() {
            if let Some(v) = part.strip_prefix("rssi_dbm=") {
                dbm = v.parse().ok();
            } else if let Some(v) = part.strip_prefix("stereo=") {
                stereo = Some(v == "1");
            }
        }
        Some((dbm?, stereo?))
    }
    /// Percent of a scan that is running (lock-free kernel state).
    pub fn progress(&self) -> Option<u8> {
        let text = fs::read_to_string(self.sysfs.join("state")).ok()?;
        text.split_whitespace()
            .find_map(|p| p.strip_prefix("progress="))
            .and_then(|v| v.parse::<i32>().ok())
            .map(|v| v.clamp(0, 100) as u8)
    }
    /// Search the band (blocks for the whole scan, ~10–30 s). The tuner is
    /// switched on for it when needed and returned to its previous state.
    pub fn scan(&self) -> io::Result<Vec<u32>> {
        let was_on = self.powered();
        self.power(true)?;
        let result = self.write("scan", &SCAN_STEP.to_string()).and_then(|_| {
            fs::read_to_string(self.sysfs.join("scan")).map(|text| parse_scan(&text))
        });
        if !was_on && active_streams() == 0 {
            let _ = self.power(false);
        }
        result.map(|found| strongest(&found, SCAN_STEP))
    }
    pub fn cancel_scan(&self) -> io::Result<()> {
        self.write("scan", "0")
    }
    /// The ALSA device of the tuner's capture, from /proc/asound/pcm
    /// ("00-02: Y2 FM Capture … : capture 1").
    pub fn capture_device(&self) -> Option<String> {
        let text = fs::read_to_string(&self.pcm).ok()?;
        text.lines().find_map(|line| {
            let (id, rest) = line.split_once(':')?;
            if !rest.contains(CAPTURE_NAME) || !rest.contains("capture") {
                return None;
            }
            let (card, device) = id.trim().split_once('-')?;
            Some(format!(
                "hw:{},{}",
                card.parse::<u32>().ok()?,
                device.parse::<u32>().ok()?
            ))
        })
    }
}

fn parse_scan(text: &str) -> Vec<(u32, i32)> {
    let mut found: Vec<(u32, i32)> = text
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let khz = parts.next()?.parse().ok().filter(|k| khz_valid(*k))?;
            let dbm = parts.next()?.parse().ok()?;
            Some((khz, dbm))
        })
        .collect();
    found.sort();
    found
}

/// Streams alive now, and a generation that a late power-off checks.
static STREAMS: Mutex<(u64, u64)> = Mutex::new((0, 0));
fn active_streams() -> u64 {
    STREAMS.lock().map(|s| s.0).unwrap_or(0)
}

/// One FM station playing: the tuner on and tuned, `arecord` writing the
/// tuner's sound as a WAV stream into the pipe the decoder reads.
pub struct Stream {
    out: Option<ChildStdout>,
    child: Child,
    paths: Paths,
}
impl Stream {
    pub fn start(url: &str) -> io::Result<Self> {
        Self::start_with(Paths::default(), url)
    }
    pub fn start_with(paths: Paths, url: &str) -> io::Result<Self> {
        let khz = station_khz(url)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not an FM station"))?;
        match paths.availability() {
            Availability::NoTuner => return Err(io::Error::other("this player has no FM tuner")),
            Availability::NeedsFirmware => {
                return Err(io::Error::other("FM needs the tuner firmware"))
            }
            Availability::Ready => {}
        }
        let device = paths
            .capture_device()
            .ok_or_else(|| io::Error::other("FM sound input missing"))?;
        {
            let mut s = STREAMS.lock().unwrap_or_else(|e| e.into_inner());
            s.0 += 1;
            s.1 += 1;
        }
        let started = paths
            .power(true)
            .and_then(|_| paths.tune(khz))
            .and_then(|_| {
                Command::new(&paths.arecord)
                    .args([
                        "-q", "-D", &device, "-f", "S16_LE", "-c", "2", "-r", "48000",
                    ])
                    .args(["-t", "wav"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
            });
        match started {
            Ok(mut child) => Ok(Self {
                out: child.stdout.take(),
                child,
                paths,
            }),
            Err(error) => {
                release(&paths);
                Err(error)
            }
        }
    }
    pub fn reader(&self) -> BorrowedFd<'_> {
        self.out.as_ref().expect("open until drop").as_fd()
    }
    pub fn khz(url: &str) -> Option<u32> {
        station_khz(url)
    }
    /// Signal of the station playing, for Now Playing.
    pub fn signal(&self) -> Option<(i32, bool)> {
        self.paths.signal()
    }
}
impl Drop for Stream {
    fn drop(&mut self) {
        // Kill first, then close: arecord blocked on a full pipe must not
        // keep wait() from returning (the internet radio's Beta 4 lesson).
        let _ = self.child.kill();
        drop(self.out.take());
        let _ = self.child.wait();
        release(&self.paths);
    }
}

/// One stream ended: switch the tuner off after a grace period unless
/// another stream started meanwhile.
fn release(paths: &Paths) {
    let generation = {
        let mut s = STREAMS.lock().unwrap_or_else(|e| e.into_inner());
        s.0 = s.0.saturating_sub(1);
        s.1
    };
    let paths = paths.clone();
    let _ = std::thread::Builder::new()
        .name("fm-power-off".into())
        .spawn(move || {
            std::thread::sleep(POWER_GRACE);
            let idle = STREAMS
                .lock()
                .map(|s| s.0 == 0 && s.1 == generation)
                .unwrap_or(false);
            if idle {
                let _ = paths.power(false);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Dir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir(name: &str) -> Dir {
        let root = std::env::temp_dir().join(format!("reborn-fm-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Dir(root)
    }

    fn fake(root: &Path) -> Paths {
        let p = Paths {
            sysfs: root.join("fm"),
            firmware: root.join("firmware"),
            pcm: root.join("pcm"),
            arecord: root.join("arecord"),
        };
        fs::create_dir_all(&p.sysfs).unwrap();
        fs::create_dir_all(&p.firmware).unwrap();
        p
    }

    #[test]
    fn availability_needs_the_tuner_and_both_files() {
        let dir = tempdir("avail");
        let p = fake(dir.path());
        assert_eq!(p.availability(), Availability::NoTuner);
        fs::write(p.sysfs.join("power"), "0\n").unwrap();
        assert_eq!(p.availability(), Availability::NeedsFirmware);
        fs::write(p.firmware.join("mt6627_fm_v1_patch.bin"), [1; 1028]).unwrap();
        assert_eq!(p.availability(), Availability::NeedsFirmware);
        fs::write(p.firmware.join("mt6627_fm_v1_coeff.bin"), [1; 1092]).unwrap();
        assert_eq!(p.availability(), Availability::Ready);
    }

    #[test]
    fn stations_frequencies_and_names() {
        assert_eq!(mhz(98_100), "98.1 MHz");
        assert_eq!(mhz(87_500), "87.5 MHz");
        assert_eq!(mhz(104_450), "104.45 MHz");
        let s = station(98_100);
        assert_eq!(s.name, "FM 98.1 MHz");
        assert_eq!(station_khz(&s.url), Some(98_100));
        assert!(is_fm(&s.url) && !is_fm("https://x"));
        assert_eq!(station_khz("fm:87400"), None);
        assert_eq!(station_khz("fm:98120"), None);
        assert_eq!(station_khz("fm:108050"), None);
        assert_eq!(station_khz("fm:abc"), None);
    }

    #[test]
    fn scan_results_keep_the_strongest_neighbour() {
        let found =
            parse_scan("98100 -70\n87500 -80\n98200 -60\n98300 -75\n105000 -90\nbad\n90 1\n");
        assert_eq!(found[0], (87_500, -80));
        assert_eq!(strongest(&found, 100), vec![87_500, 98_200, 105_000]);
        assert_eq!(strongest(&[], 100), Vec::<u32>::new());
    }

    #[test]
    fn signal_progress_and_capture_device() {
        let dir = tempdir("signal");
        let p = fake(dir.path());
        fs::write(p.sysfs.join("signal"), "rssi_dbm=-62 stereo=1\n").unwrap();
        assert_eq!(p.signal(), Some((-62, true)));
        fs::write(
            p.sysfs.join("state"),
            "powered=1 lost=0 khz=98100 chip=6627 rom=0 patch=0001 progress=42 error=0\n",
        )
        .unwrap();
        assert_eq!(p.progress(), Some(42));
        fs::write(&p.pcm, "00-00: Y2 Headphone Playback (*) :  : playback 1\n00-02: Y2 FM Capture snd-soc-dummy-dai-2 :  : capture 1\n").unwrap();
        assert_eq!(p.capture_device().as_deref(), Some("hw:0,2"));
        fs::write(&p.pcm, "00-00: Y2 Headphone Playback (*) :  : playback 1\n").unwrap();
        assert_eq!(p.capture_device(), None);
    }

    #[test]
    fn saved_scan_round_trips_and_drops_bad_entries() {
        let dir = tempdir("saved");
        let path = dir.path().join("state/fm.json");
        assert_eq!(load(&path), Saved::default());
        save(
            &path,
            &Saved {
                scanned: true,
                stations: vec![98_100, 98_100, 1, 104_400],
            },
        )
        .unwrap();
        assert_eq!(
            load(&path),
            Saved {
                scanned: true,
                stations: vec![98_100, 104_400]
            }
        );
    }

    #[test]
    fn a_stream_powers_tunes_records_and_powers_off_after_the_grace() {
        let dir = tempdir("stream");
        let p = fake(dir.path());
        fs::write(p.sysfs.join("power"), "0\n").unwrap();
        for n in ["mt6627_fm_v1_patch.bin", "mt6627_fm_v1_coeff.bin"] {
            fs::write(p.firmware.join(n), [1; 8]).unwrap();
        }
        fs::write(&p.pcm, "00-02: Y2 FM Capture x :  : capture 1\n").unwrap();
        // A stand-in arecord: records its arguments, writes a few bytes.
        let args = dir.path().join("args");
        fs::write(
            &p.arecord,
            format!(
                "#!/bin/sh\necho \"$@\" > {}\nprintf RIFF\nsleep 30\n",
                args.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p.arecord, fs::Permissions::from_mode(0o755)).unwrap();
        let stream = Stream::start_with(p.clone(), "fm:98100").unwrap();
        let mut head = [0u8; 4];
        let mut file = fs::File::from(stream.reader().try_clone_to_owned().unwrap());
        use std::io::Read;
        file.read_exact(&mut head).unwrap();
        assert_eq!(&head, b"RIFF");
        assert_eq!(fs::read_to_string(p.sysfs.join("power")).unwrap(), "1");
        assert_eq!(
            fs::read_to_string(p.sysfs.join("frequency")).unwrap(),
            "98100"
        );
        let recorded = fs::read_to_string(&args).unwrap();
        assert!(
            recorded.contains("-D hw:0,2")
                && recorded.contains("-r 48000")
                && recorded.contains("-t wav")
        );
        drop(file);
        drop(stream);
        assert_eq!(fs::read_to_string(p.sysfs.join("power")).unwrap(), "1"); // grace
        std::thread::sleep(POWER_GRACE + Duration::from_millis(400));
        assert_eq!(fs::read_to_string(p.sysfs.join("power")).unwrap(), "0");
        assert!(Stream::start_with(p.clone(), "https://x").is_err());
        fs::remove_file(p.firmware.join("mt6627_fm_v1_coeff.bin")).unwrap();
        assert!(Stream::start_with(p, "fm:98100").is_err());
    }
}
