#![forbid(unsafe_code)]
use dbus::{
    arg::{PropMap, RefArg},
    blocking::{
        stdintf::org_freedesktop_dbus::{ObjectManager, Properties},
        Connection,
    },
    channel::{MatchingReceiver, Sender},
    message::MatchRule,
    Message, Path as DbusPath,
};
use reborn_core::{atomic_write, BluetoothPcm, RadioScan};
use reborn_core::{platform::Fact, CodecPreference};
use reborn_observability::{HealthState, Level, Observer};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Device {
    pub path: String,
    pub name: String,
    pub address: String,
    pub paired: bool,
    #[serde(default)]
    pub bonded: bool,
    #[serde(default)]
    pub trusted: bool,
    pub connected: bool,
    pub audio: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Status {
    pub available: bool,
    pub powered: bool,
    pub discovering: bool,
    pub bluealsa: bool,
    pub pcms: Vec<BluetoothPcm>,
    #[serde(default)]
    pub codec_policy: Option<crate::codecs::Session>,
    pub devices: Vec<Device>,
    pub pending: Option<Pairing>,
    pub error: Option<String>,
    #[serde(default)]
    pub scan: RadioScan,
    #[serde(skip)]
    bluez_owner: Option<String>,
    #[serde(skip)]
    bluealsa_owner: Option<String>,
}
impl Status {
    pub fn playback_pcm(&self, address: &str) -> Result<BluetoothPcm, String> {
        let device = self
            .devices
            .iter()
            .find(|d| d.address.eq_ignore_ascii_case(address) && d.connected && d.audio)
            .ok_or("connected A2DP peer required")?;
        if !self.bluealsa {
            return Err("BlueALSA unavailable".into());
        }
        self.pcms
            .iter()
            .find(|pcm| pcm.is_a2dp_playback_for(&device.path))
            .cloned()
            .ok_or("BlueALSA playback PCM not ready; retry after connection completes".into())
    }

    /// The codec actually negotiated for `path`'s playback transport.
    pub fn active_codec(&self, path: &str) -> Option<String> {
        self.pcms
            .iter()
            .find(|pcm| pcm.is_a2dp_playback_for(path))
            .and_then(|pcm| pcm.codec.clone())
            .filter(|c| !c.is_empty())
    }

    /// Codec preferences worth offering for the connected peer: only codecs
    /// the current policy enables *and* the peer can use. A single usable
    /// codec means there is nothing to choose and the row is hidden.
    pub fn codec_choices(&self) -> Vec<CodecPreference> {
        let Some(policy) = &self.codec_policy else {
            return vec![];
        };
        let usable = |name: &str| {
            policy.eligibility.get(name).is_some_and(|e| {
                e.runtime_enabled
                    && e.mutually_usable
                    && (e.auto_eligible || e.experimental_eligible || name == "SBC")
            })
        };
        let mut choices: Vec<_> = [
            ("SBC", CodecPreference::Sbc),
            ("AAC", CodecPreference::Aac),
            ("aptX", CodecPreference::Aptx),
            ("aptX-HD", CodecPreference::AptxHd),
            ("LDAC", CodecPreference::Ldac),
        ]
        .into_iter()
        .filter(|(name, _)| usable(name))
        .map(|(_, preference)| preference)
        .collect();
        if crate::codecs::experimental_xq_enabled() && usable("SBC") {
            choices.insert(1, CodecPreference::SbcXq);
        }
        if choices.len() < 2 {
            return vec![];
        }
        if policy
            .eligibility
            .values()
            .any(|e| e.auto_eligible || e.experimental_eligible)
        {
            choices.insert(0, CodecPreference::Auto);
        }
        choices
    }

    /// Transport observations for Diagnostics → Bluetooth.
    pub fn diagnostic_facts(&self) -> Vec<Fact> {
        let yes = |b: bool| if b { "Yes" } else { "No" };
        let mut facts = vec![
            Fact::new("Adapter", yes(self.available)),
            Fact::new("Powered", yes(self.powered)),
            Fact::new("Audio service", yes(self.bluealsa)),
        ];
        for d in self.devices.iter().filter(|d| d.paired || d.connected) {
            facts.push(Fact::new(
                d.name.clone(),
                format!(
                    "paired {} · bonded {} · trusted {} · connected {}",
                    yes(d.paired),
                    yes(d.bonded),
                    yes(d.trusted),
                    yes(d.connected)
                ),
            ));
        }
        for pcm in &self.pcms {
            facts.push(Fact::new(
                "Active codec",
                pcm.codec.clone().unwrap_or_else(|| "Not reported".into()),
            ));
            facts.push(Fact::new(
                "PCM",
                format!(
                    "{} · {} Hz · {} ch · {}",
                    pcm.negotiated_format()
                        .map(|f| f.as_str().to_owned())
                        .unwrap_or_else(|e| e),
                    pcm.rate
                        .map(|r| r.to_string())
                        .unwrap_or_else(|| "?".into()),
                    pcm.channels
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "?".into()),
                    if pcm.running == Some(true) {
                        "running"
                    } else {
                        "idle"
                    }
                ),
            ));
            facts.push(Fact::new(
                "Transport generation",
                pcm.transport_generation.to_string(),
            ));
        }
        if let Some(policy) = &self.codec_policy {
            facts.push(Fact::new("Codec preference", policy.preference.label()));
            facts.push(Fact::new("Codec negotiation", policy.state.clone()));
            if let Some(reason) = &policy.reason {
                facts.push(Fact::new("Codec reason", reason.clone()));
            }
        }
        facts
    }

    pub fn playback_rate(&self, address: &str) -> Result<u32, String> {
        let pcm = self.playback_pcm(address)?;
        if pcm.channels != Some(2) {
            return Err("Bluetooth PCM must negotiate stereo for Baseline 01".into());
        }
        pcm.negotiated_rate()
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Pairing {
    pub device: String,
    pub display: String,
}
pub enum Command {
    Power(bool),
    Scan(bool),
    Pair(String),
    Connect(String),
    Disconnect(String),
    Forget(String),
    Confirm(bool),
    Refresh,
    Codec {
        address: String,
        preference: reborn_core::CodecPreference,
    },
    Stop,
}
pub struct Bluetooth {
    pub commands: SyncSender<Command>,
    pub events: Receiver<Status>,
}
struct Pending {
    message: Message,
    display: Pairing,
    started: Instant,
}
struct Operation {
    id: u64,
    started: Instant,
    method: String,
    path: String,
    owner: String,
    lease: Option<std::fs::File>,
    pairing: Option<PairableWindow>,
}
type Operations = Arc<Mutex<HashMap<u32, Operation>>>;

// Pair() alone does not make Linux bondable: hci_io_capa_request_evt strips
// bonding authentication when Adapter1.Pairable is false. Keep a bounded
// window on the same BlueZ owner, then restore the owner's previous policy.
struct PairableWindow {
    owner: String,
    adapter: String,
    previous: bool,
    timeout: u32,
}
impl PairableWindow {
    fn begin(c: &Connection, owner: &str, adapter: &str) -> Result<Self, String> {
        let proxy = c.with_proxy(owner, adapter, Duration::from_secs(3));
        let window = Self {
            owner: owner.into(),
            adapter: adapter.into(),
            previous: proxy
                .get("org.bluez.Adapter1", "Pairable")
                .map_err(dbus_error)?,
            timeout: proxy
                .get("org.bluez.Adapter1", "PairableTimeout")
                .map_err(dbus_error)?,
        };
        // The daemon timer closes the window even if Reborn crashes. It is
        // longer than our operation deadline, never an indefinite permission.
        let result = proxy
            .set("org.bluez.Adapter1", "PairableTimeout", 90u32)
            .and_then(|()| proxy.set("org.bluez.Adapter1", "Pairable", true))
            .map_err(dbus_error);
        if let Err(error) = result {
            let _ = window.restore(c);
            return Err(error);
        }
        Ok(window)
    }
    fn restore(self, c: &Connection) -> Result<(), String> {
        // Unique name prevents a delayed cleanup from changing a new daemon.
        let proxy = c.with_proxy(self.owner, self.adapter, Duration::from_secs(3));
        proxy
            .set("org.bluez.Adapter1", "Pairable", self.previous)
            .map_err(dbus_error)?;
        proxy
            .set("org.bluez.Adapter1", "PairableTimeout", self.timeout)
            .map_err(dbus_error)
    }
}
impl Operation {
    fn restore_pairable(&mut self, c: &Connection) -> Result<(), String> {
        self.pairing
            .take()
            .map_or(Ok(()), |window| window.restore(c))
    }
    fn send(&self, c: &Connection) -> Result<u32, String> {
        let message =
            Message::new_method_call(&self.owner, &self.path, "org.bluez.Device1", &self.method)
                .map_err(|_| "invalid D-Bus path".to_string())?;
        c.send(message).map_err(|_| "D-Bus send failed".into())
    }
    fn reply(&mut self, c: &Connection, msg: &mut Message) -> Result<Option<u32>, String> {
        msg.as_result().map_err(dbus_error)?;
        if self.method == "Pair" {
            let proxy = c.with_proxy(&self.owner, &self.path, Duration::from_secs(3));
            let paired: bool = proxy
                .get("org.bluez.Device1", "Paired")
                .map_err(dbus_error)?;
            let bonded: bool = proxy
                .get("org.bluez.Device1", "Bonded")
                .map_err(dbus_error)?;
            if !paired || !bonded {
                return Err("Pairing did not create a saved bond; put the headset in pairing mode and retry".into());
            }
            proxy
                .set("org.bluez.Device1", "Trusted", true)
                .map_err(dbus_error)?;
            self.restore_pairable(c)?;
            if self.lease.is_some() {
                write_intent("connect_pending")?;
            }
            self.method = "Connect".into();
            self.started = Instant::now();
            return self.send(c).map(Some);
        }
        if self.method == "Connect" && self.lease.is_some() {
            write_intent("connect")?;
        }
        Ok(None)
    }
    fn cancel(&mut self, c: &Connection) {
        if self.method == "Pair" {
            let _ = c
                .with_proxy(&self.owner, &self.path, Duration::from_secs(2))
                .method_call::<(), _, _, _>("org.bluez.Device1", "CancelPairing", ());
        }
        let _ = self.restore_pairable(c);
        if self.lease.is_some() {
            let _ = write_intent("uncertain");
        }
    }
}
fn cancel_operations(c: &Connection, operations: &Operations) {
    if let Ok(mut ops) = operations.lock() {
        for (_, mut operation) in ops.drain() {
            operation.cancel(c);
        }
    }
}

fn user_intent(operation: &str) -> Result<Option<std::fs::File>, String> {
    if !Path::new("/etc/y2linux/platform-contract").exists() {
        return Ok(None);
    }
    use std::os::unix::fs::OpenOptionsExt;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open("/run/y2/bt-operation.lock")
        .map_err(|e| e.to_string())?;
    lock.try_lock().map_err(|_| {
        "Platform Bluetooth operation in progress; retry after it completes".to_string()
    })?;
    write_intent(match operation {
        "connect" => "connect_pending",
        "power_on" => "power_pending",
        _ => operation,
    })?;
    Ok(Some(lock))
}
fn write_intent(operation: &str) -> Result<(), String> {
    let boot =
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(|e| e.to_string())?;
    let now = crate::native::monotonic_seconds().ok_or("monotonic clock unavailable")?;
    let now = (now * 1e6) as u64;
    let text = format!(
        "[intent]\nversion=1\nboot_id={}\nsequence={now}\noperation={operation}\ndeadline_us={}\n",
        boot.trim(),
        now + 30_000_000
    );
    atomic_write(Path::new("/run/y2/bt-control.ini"), text.as_bytes()).map_err(|e| e.to_string())
}

type Objects = HashMap<DbusPath<'static>, HashMap<String, PropMap>>;
fn objects(c: &Connection) -> Result<Objects, String> {
    c.with_proxy("org.bluez", "/", Duration::from_secs(2))
        .get_managed_objects()
        .map_err(|e| {
            format!(
                "D-Bus org.bluez ObjectManager: {}",
                e.name().unwrap_or("unknown")
            )
        })
}
fn text(p: &PropMap, k: &str) -> String {
    p.get(k).and_then(|v| v.0.as_str()).unwrap_or("").into()
}
fn yes(p: &PropMap, k: &str) -> bool {
    p.get(k).and_then(|v| v.0.as_i64()).unwrap_or(0) != 0
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct PcmObservation {
    object: String,
    device: String,
    transport: String,
    mode: String,
    codec: Option<String>,
    format: Option<u16>,
    rate: Option<u32>,
    channels: Option<u8>,
}
impl From<&BluetoothPcm> for PcmObservation {
    fn from(pcm: &BluetoothPcm) -> Self {
        Self {
            object: pcm.object.clone(),
            device: pcm.device.clone(),
            transport: pcm.transport.clone(),
            mode: pcm.mode.clone(),
            codec: pcm.codec.clone(),
            format: pcm.format,
            rate: pcm.rate,
            channels: pcm.channels,
        }
    }
}

#[derive(Default)]
struct TransportEpochs {
    owners: Option<(String, String)>,
    observations: HashMap<String, (PcmObservation, u64)>,
}
static NEXT_TRANSPORT_EPOCH: AtomicU64 = AtomicU64::new(1);

fn next_transport_epoch() -> u64 {
    NEXT_TRANSPORT_EPOCH.fetch_add(1, Ordering::Relaxed).max(1)
}

impl TransportEpochs {
    fn refresh(
        &mut self,
        status: &mut Status,
        invalidated: &HashSet<String>,
        daemon_invalidated: bool,
    ) {
        let owners = (
            status.bluez_owner.clone().unwrap_or_default(),
            status.bluealsa_owner.clone().unwrap_or_default(),
        );
        if daemon_invalidated || self.owners.as_ref() != Some(&owners) {
            self.owners = Some(owners);
            self.observations.clear();
        }
        let present = status
            .pcms
            .iter()
            .map(|pcm| pcm.object.clone())
            .collect::<HashSet<_>>();
        self.observations
            .retain(|object, _| present.contains(object));
        for pcm in &mut status.pcms {
            let observation = PcmObservation::from(&*pcm);
            let unchanged = self
                .observations
                .get(&observation.object)
                .is_some_and(|(old, _)| {
                    old == &observation && !invalidated.contains(&observation.object)
                });
            let generation = if unchanged {
                self.observations[&observation.object].1
            } else {
                let generation = next_transport_epoch();
                self.observations
                    .insert(observation.object.clone(), (observation, generation));
                generation
            };
            pcm.transport_generation = generation;
        }
    }
}

fn name_owner(c: &Connection, name: &str) -> Option<String> {
    c.with_proxy(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        Duration::from_secs(1),
    )
    .method_call("org.freedesktop.DBus", "GetNameOwner", (name,))
    .map(|(owner,): (String,)| owner)
    .ok()
}

fn status(c: &Connection) -> Result<(String, Status), String> {
    let mut state = Status::default();
    let mut adapter = String::new();
    for (path, interfaces) in objects(c)? {
        if let Some(p) = interfaces.get("org.bluez.Adapter1") {
            if adapter.is_empty() {
                adapter = path.to_string();
                state.available = true;
                state.powered = yes(p, "Powered");
                state.discovering = yes(p, "Discovering");
            }
        }
        if let Some(p) = interfaces.get("org.bluez.Device1") {
            let audio = p
                .get("UUIDs")
                .and_then(|v| v.0.as_iter())
                .is_some_and(|mut a| {
                    a.any(|v| v.as_str().is_some_and(|s| s.starts_with("0000110b")))
                });
            state.devices.push(Device {
                path: path.to_string(),
                name: text(p, "Alias"),
                address: text(p, "Address"),
                paired: yes(p, "Paired"),
                bonded: yes(p, "Bonded"),
                trusted: yes(p, "Trusted"),
                connected: yes(p, "Connected"),
                audio,
            });
        }
    }
    state.bluez_owner = name_owner(c, "org.bluez");
    state.bluealsa_owner = name_owner(c, "org.bluealsa");
    state.bluealsa = state.bluealsa_owner.is_some();
    if let Some(owner) = state.bluealsa_owner.as_deref() {
        let marker = Path::new("/run/y2/bt-codec-uncertain.json");
        if let Ok(bytes) = std::fs::read(marker) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if value["bluealsa_owner"]
                    .as_str()
                    .is_some_and(|previous| previous != owner)
                {
                    let _ = std::fs::remove_file(marker);
                }
            }
        }
    }
    // BlueALSA 5 exposes PCM objects through ObjectManager. Its negotiated
    // properties are Rate and Format; Sampling was the incompatible v4 name.
    let pcms: Result<Objects, _> = c
        .with_proxy("org.bluealsa", "/org/bluealsa", Duration::from_secs(1))
        .get_managed_objects();
    if let Ok(pcms) = pcms {
        state.pcms = pcms
            .into_iter()
            .filter_map(|(path, mut interfaces)| {
                interfaces.remove("org.bluealsa.PCM1").map(|p| (path, p))
            })
            .take(16)
            .map(|(path, p)| {
                let object = path.to_string();
                BluetoothPcm {
                    object: object.clone(),
                    device: text(&p, "Device"),
                    transport: text(&p, "Transport"),
                    mode: text(&p, "Mode"),
                    codec: (!text(&p, "Codec").is_empty()).then(|| text(&p, "Codec")),
                    format: p
                        .get("Format")
                        .and_then(|v| v.0.as_u64())
                        .and_then(|value| u16::try_from(value).ok()),
                    rate: p
                        .get("Rate")
                        .and_then(|v| v.0.as_u64())
                        .and_then(|value| u32::try_from(value).ok()),
                    channels: p
                        .get("Channels")
                        .and_then(|v| v.0.as_u64())
                        .and_then(|value| u8::try_from(value).ok()),
                    running: p.get("Running").and_then(|v| v.0.as_i64()).map(|v| v != 0),
                    transport_generation: 0,
                }
            })
            .collect();
    }
    state.devices.sort_by(|a, b| a.name.cmp(&b.name));
    state.devices.truncate(128);
    Ok((adapter, state))
}
/// Hold a shared lease on the Bluetooth PCM for the life of an open sink.
/// The platform's codec switcher takes the lease exclusively, so playback and
/// codec renegotiation never overlap; an unknown codec outcome blocks reopen.
pub fn playback_pcm_lease() -> Result<Option<std::fs::File>, String> {
    if !Path::new("/etc/y2linux/platform-contract").exists() {
        return Ok(None);
    }
    pcm_lease_at(Path::new("/run/y2"))
}
fn pcm_lease_at(directory: &std::path::Path) -> Result<Option<std::fs::File>, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("bt-pcm.lock"))
        .map_err(|e| e.to_string())?;
    lock.try_lock_shared()
        .map_err(|_| "Bluetooth codec selection in progress; retry playback")?;
    if directory.join("bt-codec-uncertain.json").exists() {
        return Err("Bluetooth codec outcome unknown; restart BlueALSA before playback".into());
    }
    Ok(Some(lock))
}

fn dbus_error(e: dbus::Error) -> String {
    format!("D-Bus org.bluez: {}", e.name().unwrap_or("unknown"))
}
fn compatible_codecs(mutual: &HashMap<String, PropMap>) -> Vec<String> {
    mutual
        .iter()
        .filter_map(|(name, p)| {
            let stereo = p
                .get("Channels")
                .and_then(|v| v.0.as_iter())
                .is_some_and(|mut a| a.any(|v| v.as_u64() == Some(2)));
            let rate = p
                .get("Rates")
                .and_then(|v| v.0.as_iter())
                .is_some_and(|mut a| {
                    a.any(|v| v.as_u64().is_some_and(|n| n == 44100 || n == 48000))
                });
            (stereo && rate).then(|| name.clone())
        })
        .collect()
}

fn read_codec_inventory(path: &Path) -> Result<crate::codecs::Inventory, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 65536 {
        return Err("codec inventory too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid platform codec inventory".into())
}

/// Eligibility discovery is observation only. It neither acquires the PCM nor
/// invokes SelectCodec/Connect, and can run while a current stream is playing.
fn discover_codec_policy(
    c: &Connection,
    current: &Status,
    inventory_path: &Path,
    experimental: bool,
) -> Result<crate::codecs::Session, String> {
    let mut peers = current.devices.iter().filter(|d| d.connected && d.audio);
    let device = peers.next().ok_or("no connected audio peer")?;
    if peers.next().is_some() {
        return Err("ambiguous connected audio peers".into());
    }
    let pcm = current.playback_pcm(&device.address)?;
    if pcm.transport_generation == 0 {
        return Err("unobserved transport epoch".into());
    }
    let owner = current
        .bluealsa_owner
        .as_deref()
        .ok_or("BlueALSA owner missing")?;
    let bluez = current
        .bluez_owner
        .as_deref()
        .ok_or("BlueZ owner missing")?;
    let inventory = read_codec_inventory(inventory_path)?;
    let manager = c.with_proxy(owner, "/org/bluealsa", Duration::from_millis(350));
    let runtime: Vec<String> = manager
        .get("org.bluealsa.Manager1", "Codecs")
        .map_err(dbus_error)?;
    let runtime: Vec<String> = runtime
        .into_iter()
        .filter_map(|name| name.strip_prefix("a2dp-source:").map(str::to_owned))
        .collect();
    let proxy = c.with_proxy(owner, pcm.object.as_str(), Duration::from_millis(350));
    let (mutual,): (HashMap<String, PropMap>,) = proxy
        .method_call("org.bluealsa.PCM1", "GetCodecs", ())
        .map_err(dbus_error)?;
    let observed: PropMap = proxy.get_all("org.bluealsa.PCM1").map_err(dbus_error)?;
    if name_owner(c, "org.bluealsa").as_deref() != Some(owner)
        || name_owner(c, "org.bluez").as_deref() != Some(bluez)
        || text(&observed, "Device") != pcm.device
        || text(&observed, "Transport") != pcm.transport
        || text(&observed, "Mode") != pcm.mode
        || text(&observed, "Codec") != pcm.codec.as_deref().unwrap_or("")
        || observed.get("Rate").and_then(|v| v.0.as_u64()) != pcm.rate.map(u64::from)
        || observed.get("Format").and_then(|v| v.0.as_u64()) != pcm.format.map(u64::from)
    {
        return Err("codec capability observation changed".into());
    }
    let mut session = crate::codecs::Session::new_experimental(
        pcm.transport_generation,
        CodecPreference::Sbc,
        &inventory,
        &runtime,
        &compatible_codecs(&mutual),
        experimental,
    );
    session.state = "ObservedCapabilities".into();
    Ok(session)
}

fn refresh_codec_policy(
    previous: Option<&crate::codecs::Session>,
    current: &Status,
    discover: impl FnOnce() -> Result<crate::codecs::Session, String>,
) -> Option<crate::codecs::Session> {
    let peers: Vec<_> = current
        .devices
        .iter()
        .filter(|d| d.connected && d.audio)
        .collect();
    if peers.len() != 1 {
        return None;
    }
    let pcm = current.playback_pcm(&peers[0].address).ok()?;
    if let Some(previous) =
        previous.filter(|p| p.generation == pcm.transport_generation && p.generation != 0)
    {
        return Some(previous.clone());
    }
    discover().ok()
}

fn codec_request(
    c: &Connection,
    current: &Status,
    address: &str,
    preference: reborn_core::CodecPreference,
) -> Result<crate::codecs::Session, String> {
    use dbus::arg::Variant;
    let pcm = current.playback_pcm(address)?;
    if pcm.running != Some(false) || pcm.transport_generation == 0 {
        return Err("Release the Bluetooth PCM before codec selection".into());
    }
    let owner = current
        .bluealsa_owner
        .as_deref()
        .ok_or("BlueALSA owner missing")?;
    let bluez = current
        .bluez_owner
        .as_deref()
        .ok_or("BlueZ owner missing")?;
    let inventory_path = "/etc/y2linux/bluetooth-codecs.json";
    // An exclusive PCM lease excludes probes and live/paused app sink handles.
    use std::os::unix::fs::OpenOptionsExt;
    let pcm_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open("/run/y2/bt-pcm.lock")
        .map_err(|e| e.to_string())?;
    pcm_lock
        .try_lock()
        .map_err(|_| "Bluetooth PCM still open; stop playback and retry")?;
    let inventory = read_codec_inventory(Path::new(inventory_path))?;
    let manager = c.with_proxy(owner, "/org/bluealsa", Duration::from_secs(1));
    let runtime: Vec<String> = manager
        .get("org.bluealsa.Manager1", "Codecs")
        .map_err(dbus_error)?;
    let runtime: Vec<String> = runtime
        .iter()
        .filter_map(|s| s.strip_prefix("a2dp-source:").map(str::to_string))
        .collect();
    let proxy = c.with_proxy(owner, pcm.object.as_str(), Duration::from_secs(2));
    let (mutual,): (HashMap<String, PropMap>,) = proxy
        .method_call("org.bluealsa.PCM1", "GetCodecs", ())
        .map_err(dbus_error)?;
    let compatible = compatible_codecs(&mutual);
    let mut session = crate::codecs::Session::new_experimental(
        pcm.transport_generation,
        preference,
        &inventory,
        &runtime,
        &compatible,
        crate::codecs::experimental_enabled(),
    );
    session.apply_context(crate::codecs::context(address, pcm.codec.as_deref()));
    crate::codecs::history(address, pcm.codec.as_deref(), None);
    let sequence: u32 = proxy
        .get("org.bluealsa.PCM1", "Sequence")
        .map_err(dbus_error)?;
    let started = Instant::now();
    let mut operation_lease = None;
    while let Some(codec) = session.next(
        pcm.transport_generation,
        started.elapsed().as_millis() as u64,
    ) {
        if operation_lease.is_none() {
            operation_lease = user_intent("codec")?;
        }
        // Unique owners + connection sequence reject peer/daemon replacement,
        // even when the object path happens to be reused.
        if name_owner(c, "org.bluealsa").as_deref() != Some(owner)
            || name_owner(c, "org.bluez").as_deref() != Some(bluez)
        {
            session.fail("daemon_owner_changed");
            break;
        }
        let connected: Result<bool, _> = c
            .with_proxy(bluez, pcm.device.as_str(), Duration::from_secs(1))
            .get("org.bluez.Device1", "Connected");
        let properties: Result<PropMap, _> = proxy.get_all("org.bluealsa.PCM1");
        let valid = properties.as_ref().is_ok_and(|p| {
            p.get("Sequence").and_then(|v| v.0.as_u64()) == Some(sequence as u64)
                && p.get("Running").and_then(|v| v.0.as_i64()) == Some(0)
                && text(p, "Device") == pcm.device
                && text(p, "Mode") == "sink"
        });
        if !matches!(connected, Ok(true)) || !valid {
            session.fail("peer_or_pcm_disappeared_changed_or_busy");
            break;
        }
        let rates: Vec<u64> = mutual[&codec]
            .get("Rates")
            .and_then(|v| v.0.as_iter())
            .map(|a| a.filter_map(|v| v.as_u64()).collect())
            .unwrap_or_default();
        let rate = if rates.contains(&44100) {
            44100u32
        } else {
            48000u32
        };
        let mut props = PropMap::new();
        props.insert("Channels".into(), Variant(Box::new(2u8)));
        props.insert("Rate".into(), Variant(Box::new(rate)));
        let marker = Path::new("/run/y2/bt-codec-uncertain.json");
        if marker.exists() {
            session.fail("codec_outcome_unknown_restart_BlueALSA_before_retry");
            break;
        }
        atomic_write(
            marker,
            &serde_json::to_vec(&json!({"bluealsa_owner":owner,"sequence":sequence}))
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let result: Result<(), dbus::Error> =
            proxy.method_call("org.bluealsa.PCM1", "SelectCodec", (codec.as_str(), props));
        match result {
            Ok(()) => {
                let _ = std::fs::remove_file(marker);
                session.accepted(&codec);
                let _ = write_intent("connect");
                break;
            }
            Err(e)
                if matches!(
                    e.name(),
                    Some(
                        "org.freedesktop.DBus.Error.NotSupported"
                            | "org.freedesktop.DBus.Error.InvalidArgs"
                    )
                ) || (e.name() == Some("org.freedesktop.DBus.Error.Failed")
                    && matches!(
                        e.message(),
                        Some(
                            "SEP codec not available"
                                | "SEP codec not supported"
                                | "No BlueZ SEP support"
                        )
                    )) =>
            {
                let _ = std::fs::remove_file(marker);
                crate::codecs::history(address, None, Some(&codec));
            }
            Err(_) => {
                session.fail("codec_completion_unknown_no_retry");
                break;
            }
        }
    }
    Ok(session)
}
fn call(c: &Connection, path: &str, interface: &str, method: &str) -> Result<(), String> {
    let _: () = c
        .with_proxy("org.bluez", path, Duration::from_secs(5))
        .method_call(interface, method, ())
        .map_err(dbus_error)?;
    Ok(())
}
fn power(c: &Connection, adapter: &str, on: bool, persist: bool) -> Result<(), String> {
    power_at(
        c,
        adapter,
        on,
        persist,
        Path::new("/data/bluetooth/enabled"),
    )
}
fn power_at(
    c: &Connection,
    adapter: &str,
    on: bool,
    persist: bool,
    preference: &Path,
) -> Result<(), String> {
    let lease = user_intent(if on { "power_on" } else { "power_off" })?;
    let result = c
        .with_proxy("org.bluez", adapter, Duration::from_secs(15))
        .set("org.bluez.Adapter1", "Powered", on);
    if result.is_err() && lease.is_some() {
        let _ = write_intent("uncertain");
    }
    result.map_err(dbus_error)?;
    if lease.is_some() {
        write_intent(if on { "power_on" } else { "power_off" })?;
    }
    if persist {
        atomic_write(preference, if on { b"1\n" } else { b"0\n" }).map_err(|e| e.to_string())?;
    }
    Ok(())
}
// The same sequence is used by the worker and covered with a fallible fake
// adapter. Discovery is never attempted before successful radio activation.
fn start_scan(
    powered: bool,
    mut perform: impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    if !powered {
        perform("PowerOn")?;
    }
    perform("StartDiscovery")
}
struct Discovery {
    id: u64,
    until: Instant,
}
impl Discovery {
    fn new(id: u64, now: Instant) -> Self {
        Self {
            id,
            until: now + Duration::from_secs(15),
        }
    }
    fn expired(&self, now: Instant) -> bool {
        now >= self.until
    }
}
fn rejected(msg: &Message) -> Message {
    msg.error(
        &"org.bluez.Error.Rejected".into(),
        c"Pairing requires explicit user confirmation",
    )
}
impl Bluetooth {
    pub fn spawn(log: Observer) -> std::io::Result<Self> {
        Self::spawn_with(
            log,
            Connection::new_system,
            "/data/bluetooth/enabled".into(),
        )
    }
    fn spawn_with(
        log: Observer,
        connect: impl Fn() -> Result<Connection, dbus::Error> + Send + 'static,
        preference: PathBuf,
    ) -> std::io::Result<Self> {
        let (tx, rx) = sync_channel(8);
        let (et, er) = sync_channel(2);
        thread::Builder::new()
            .name("bluetooth".into())
            .spawn(move || {
                let mut connected: Vec<String> = vec![];
                loop {
                    let c = match connect() {
                        Ok(c) => c,
                        Err(_) => {
                            log.health_set(
                                "bluetooth",
                                HealthState::Unavailable,
                                false,
                                "system D-Bus unavailable",
                            );
                            let _ = et.try_send(Status {
                                error: Some("system D-Bus unavailable".into()),
                                ..Default::default()
                            });
                            match rx.recv_timeout(Duration::from_secs(3)) {
                                Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                                _ => continue,
                            }
                        }
                    };
                    let operations:Operations=Arc::new(Mutex::new(HashMap::new()));
                    let (completed_tx, completed_rx) = sync_channel::<Option<String>>(16);
                    for kind in [dbus::MessageType::MethodReturn,dbus::MessageType::Error] {
                        let ops=operations.clone();let observer=log.clone();let completed=completed_tx.clone();let mut rule=MatchRule::new();rule.msg_type=Some(kind);
                        c.start_receive(rule,Box::new(move|mut msg,connection|{
                            if let Some(serial)=msg.get_reply_serial(){if let Ok(mut ops)=ops.lock(){if let Some(mut operation)=ops.remove(&serial){
                                match operation.reply(connection, &mut msg) {
                                    Ok(Some(next)) => {
                                        observer.emit(Level::Info,"bluetooth","pair_bonded","Bond saved; audio connection requested",Some(operation.id),json!({"trusted":true}));
                                        ops.insert(next, operation);
                                    }
                                    Ok(None) => {
                                        observer.emit(Level::Info,"bluetooth","operation_completed","BlueZ operation completed",Some(operation.id),json!({"method":operation.method}));
                                        let _ = completed.try_send(None);
                                    }
                                    Err(mut error) => {
                                        if let Err(cleanup) = operation.restore_pairable(connection) { error.push_str(&format!("; Pairable restoration failed: {cleanup}")); }
                                        if operation.lease.is_some() { let _=write_intent("uncertain"); }
                                        observer.add("bluetooth_errors",1.);
                                        observer.emit(Level::Error,"bluetooth","operation_failed",&error,Some(operation.id),json!({"interface":"org.bluez.Device1","method":operation.method,"recovery_attempted":false}));
                                        let _ = completed.try_send(Some(error));
                                    }
                                }
                            }}}true
                        }));
                    }
                    let pending: Arc<Mutex<Option<Pending>>> = Arc::new(Mutex::new(None));
                    let handler = pending.clone();
                    let mut rule = MatchRule::new_method_call();
                    rule.interface = Some("org.bluez.Agent1".into());
                    rule.path = Some("/org/reborn/agent".into());
                    c.start_receive(
                        rule,
                        Box::new(move |msg, c| {
                            let member = msg.member().map(|s| s.to_string()).unwrap_or_default();
                            match member.as_str() {
                                "Release" | "Cancel" => {
                                    if let Ok(mut p) = handler.lock() {
                                        if let Some(old) = p.take() {
                                            let _ = c.send(rejected(&old.message));
                                        }
                                    }
                                    let _ = c.send(msg.method_return());
                                }
                                "RequestConfirmation"
                                | "RequestAuthorization"
                                | "AuthorizeService" => {
                                    let device = msg
                                        .read1::<DbusPath>()
                                        .map(|s| s.to_string())
                                        .unwrap_or_default();
                                    let display = if member == "RequestConfirmation" {
                                        msg.read2::<DbusPath, u32>()
                                            .map(|(_, n)| format!("Confirm {n:06}"))
                                            .unwrap_or_else(|_| "Confirm pairing".into())
                                    } else {
                                        "Allow audio device?".into()
                                    };
                                    if let Ok(mut p) = handler.lock() {
                                        if p.is_some() {
                                            let _ = c.send(rejected(&msg));
                                        } else {
                                            *p = Some(Pending {
                                                message: msg,
                                                display: Pairing { device, display },
                                                started: Instant::now(),
                                            });
                                        }
                                    }
                                }
                                // Baseline agent advertises DisplayYesNo; legacy PIN/keyboard requests are explicitly rejected.
                                _ => {
                                    let _ = c.send(rejected(&msg));
                                }
                            }
                            true
                        }),
                    );
                    let registered: Result<(), dbus::Error> = c
                        .with_proxy("org.bluez", "/org/bluez", Duration::from_secs(3))
                        .method_call(
                            "org.bluez.AgentManager1",
                            "RegisterAgent",
                            (DbusPath::from("/org/reborn/agent"), "DisplayYesNo"),
                        );
                    if let Err(e) = registered {
                        log.emit(
                            Level::Warn,
                            "bluetooth",
                            "agent_unavailable",
                            &dbus_error(e),
                            None,
                            json!({}),
                        );
                    }
                    let transport_invalidations = Arc::new(Mutex::new(HashSet::new()));
                    let invalidation_sink = transport_invalidations.clone();
                    let daemon_invalidated = Arc::new(AtomicBool::new(false));
                    let daemon_invalidation_sink = daemon_invalidated.clone();
                    let mut transport_watches_ready = true;
                    let mut owner_changed = MatchRule::new_signal(
                        "org.freedesktop.DBus",
                        "NameOwnerChanged",
                    );
                    owner_changed.sender = Some("org.freedesktop.DBus".into());
                    owner_changed.path = Some(DbusPath::from("/org/freedesktop/DBus"));
                    if let Err(error) = c.add_match::<(String, String, String), _>(
                        owner_changed,
                        move |(name, _, _), _, _| {
                            if name == "org.bluez" || name == "org.bluealsa" {
                                daemon_invalidation_sink.store(true, Ordering::Release);
                            }
                            true
                        },
                    ) {
                        transport_watches_ready = false;
                        log.emit(
                            Level::Warn,
                            "bluetooth",
                            "transport_owner_watch_failed",
                            &dbus_error(error),
                            None,
                            json!({"recovery":"property_and_owner_snapshot_only"}),
                        );
                    }
                    let mut pcm_removed = MatchRule::new_signal(
                        "org.freedesktop.DBus.ObjectManager",
                        "InterfacesRemoved",
                    );
                    pcm_removed.sender = Some("org.bluealsa".into());
                    pcm_removed.path = Some(DbusPath::from("/org/bluealsa"));
                    if let Err(error) = c.add_match::<(DbusPath, Vec<String>), _>(
                        pcm_removed,
                        move |(object, interfaces), _, _| {
                            if interfaces.iter().any(|name| name == "org.bluealsa.PCM1") {
                                if let Ok(mut invalidated) = invalidation_sink.lock() {
                                    invalidated.insert(object.to_string());
                                }
                            }
                            true
                        },
                    ) {
                        transport_watches_ready = false;
                        log.emit(
                            Level::Warn,
                            "bluetooth",
                            "transport_remove_watch_failed",
                            &dbus_error(error),
                            None,
                            json!({"recovery":"periodic_snapshot_only"}),
                        );
                    }
                    let property_invalidation_sink = transport_invalidations.clone();
                    let mut properties_changed = MatchRule::new_signal(
                        "org.freedesktop.DBus.Properties",
                        "PropertiesChanged",
                    );
                    properties_changed.sender = Some("org.bluealsa".into());
                    if let Err(error) = c.add_match::<(String, PropMap, Vec<String>), _>(
                        properties_changed,
                        move |(interface, changed, invalidated), _, message| {
                            let relevant = [
                                "Device", "Transport", "Mode", "Codec", "Format", "Rate",
                                "Channels",
                            ];
                            if interface == "org.bluealsa.PCM1"
                                && relevant.iter().any(|name| {
                                    changed.contains_key(*name)
                                        || invalidated.iter().any(|value| value == name)
                                })
                            {
                                if let (Some(path), Ok(mut objects)) = (
                                    message.path(),
                                    property_invalidation_sink.lock(),
                                ) {
                                    objects.insert(path.to_string());
                                }
                            }
                            true
                        },
                    ) {
                        transport_watches_ready = false;
                        log.emit(
                            Level::Warn,
                            "bluetooth",
                            "transport_property_watch_failed",
                            &dbus_error(error),
                            None,
                            json!({"recovery":"periodic_snapshot_only"}),
                        );
                    }
                    let mut transport_epochs = TransportEpochs::default();
                    let mut refresh = Instant::now() - Duration::from_secs(5);
                    let mut current = Status::default();
                    let mut adapter = String::new();
                    let mut discovery: Option<Discovery> = None;
                    let mut operation_error = None;
                    loop {
                        if c.process(Duration::from_millis(20)).is_err() {
                            cancel_operations(&c, &operations);
                            current.available = false;
                            current.error = Some("Bluetooth service disconnected".into());
                            if current.scan.active() { current.scan = RadioScan::Failed { message: "Bluetooth service disconnected".into() }; }
                            let _ = et.try_send(current);
                            break;
                        }
                        let mut changed = false;
                        if let Ok(mut ops)=operations.lock(){ops.retain(|_,operation|{if operation.started.elapsed()>Duration::from_secs(70){operation.cancel(&c);log.add("bluetooth_errors",1.);log.emit(Level::Error,"bluetooth","operation_timeout","BlueZ operation deadline exceeded",Some(operation.id),json!({"method":operation.method}));operation_error=Some("Bluetooth operation timed out".into());current.error=operation_error.clone();changed=true;false}else{true}});}
                        while let Ok(error) = completed_rx.try_recv() {
                            operation_error = error;
                            current.error = operation_error.clone();
                            refresh = Instant::now() - Duration::from_secs(5);
                            changed = true;
                        }
                        if refresh.elapsed() > Duration::from_secs(2) {
                            match status(&c) {
                                Ok((a, mut s)) => {
                                    if !transport_watches_ready {
                                        // A same-path daemon/object restart could otherwise
                                        // survive the periodic snapshot. Refuse to expose a
                                        // playable transport if its invalidation watches failed.
                                        s.pcms.clear();
                                    }
                                    let invalidated = transport_invalidations
                                        .lock()
                                        .map(|mut objects| std::mem::take(&mut *objects))
                                        .unwrap_or_default();
                                    transport_epochs.refresh(
                                        &mut s,
                                        &invalidated,
                                        daemon_invalidated.swap(false, Ordering::AcqRel),
                                    );
                                    adapter = a;
                                    s.codec_policy = refresh_codec_policy(current.codec_policy.as_ref(), &s, || {
                                        discover_codec_policy(&c, &s, Path::new("/etc/y2linux/bluetooth-codecs.json"), crate::codecs::experimental_enabled())
                                    });
                                    s.scan = current.scan.clone();
                                    s.error = operation_error.clone();
                                    current = s
                                }
                                Err(e) => {
                                    current.available = false;
                                    current.error = Some(e)
                                }
                            }
                            refresh = Instant::now();
                            changed = true;
                        }
                        if discovery.is_some() && (!current.available || !current.powered) {
                            let message = "Bluetooth radio became unavailable during scan".to_string();
                            if let Some(job) = discovery.take() {
                                log.add("bluetooth_errors", 1.);
                                log.emit(Level::Warn, "bluetooth", "scan_failed", &message, Some(job.id), json!({"recovery_attempted":false}));
                            }
                            current.scan = RadioScan::Failed { message: message.clone() };
                            current.error = Some(message.clone());
                            operation_error = Some(message);
                            changed = true;
                        }
                        if discovery.as_ref().is_some_and(|d| d.expired(Instant::now())) {
                            let job = discovery.take().unwrap();
                            // Stop only the discovery session held by this D-Bus connection.
                            match call(&c, &adapter, "org.bluez.Adapter1", "StopDiscovery") {
                                Ok(()) => {
                                    current.scan = RadioScan::Complete { found: current.devices.len() };
                                    current.discovering = false;
                                    log.emit(Level::Info, "bluetooth", "scan_complete", "Bluetooth scan completed", Some(job.id), json!({"count":current.devices.len()}));
                                }
                                Err(e) => {
                                    current.scan = RadioScan::Failed { message: e.clone() };
                                    current.error = Some(e.clone()); operation_error = Some(e.clone());
                                    log.add("bluetooth_errors", 1.);
                                    log.emit(Level::Warn, "bluetooth", "scan_failed", &e, Some(job.id), json!({"method":"StopDiscovery","recovery_attempted":false}));
                                }
                            }
                            refresh = Instant::now() - Duration::from_secs(5);
                            changed = true;
                        }
                        if let Ok(mut p) = pending.lock() {
                            if p.as_ref()
                                .is_some_and(|v| v.started.elapsed() > Duration::from_secs(60))
                            {
                                if let Some(v) = p.take() {
                                    let _ = c.send(rejected(&v.message));
                                }
                            }
                            current.pending = p.as_ref().map(|v| v.display.clone());
                        }
                        let command = match rx.try_recv() {
                            Ok(c) => Some(c),
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                cancel_operations(&c, &operations);
                                return;
                            }
                            Err(_) => None,
                        };
                        if let Some(cmd) = command {
                            if matches!(cmd, Command::Stop) {
                                cancel_operations(&c, &operations);
                                return;
                            }
                            let id = log.correlation();
                            let operation = match &cmd {
                                Command::Power(_) => "Powered",
                                Command::Scan(true) => "StartDiscovery",
                                Command::Scan(false) => "StopDiscovery",
                                Command::Pair(_) => "Pair",
                                Command::Connect(_) => "Connect",
                                Command::Disconnect(_) => "Disconnect",
                                Command::Forget(_) => "RemoveDevice",
                                Command::Confirm(_) => "ConfirmPairing",
                                Command::Codec { .. } => "SelectCodec",
                                _ => "Refresh",
                            };
                            // Repeated presses while starting/scanning do not cancel or
                            // restart the bounded operation.
                            if matches!(cmd, Command::Scan(true)) && discovery.is_some() { continue; }
                            let scan_command = matches!(cmd, Command::Scan(_));
                            operation_error = None;
                            current.error = None;
                            let result = match cmd {
                                Command::Stop | Command::Refresh => Ok(()),
                                Command::Codec { address, preference } => {
                                    match codec_request(&c, &current, &address, preference) {
                                        Ok(session) => { let error=session.reason.clone(); current.codec_policy=Some(session); error.map_or(Ok(()),Err) },
                                        Err(error) => Err(error),
                                    }
                                },
                                Command::Power(on) => {
                                    if discovery.is_none() { current.scan = RadioScan::Idle; }
                                    if !on {
                                        if discovery.take().is_some() { let _ = call(&c, &adapter, "org.bluez.Adapter1", "StopDiscovery"); }
                                        current.scan = RadioScan::Idle;
                                    }
                                    power_at(&c, &adapter, on, true, &preference)
                                }
                                Command::Scan(true) => {
                                    current.scan = RadioScan::Starting;
                                    let _ = et.try_send(current.clone());
                                    log.emit(Level::Info, "bluetooth", "scan_start", "Bluetooth scan requested", Some(id), json!({"enable_if_off":true,"seconds":15}));
                                    let result = start_scan(current.powered, |method| {
                                        if adapter.is_empty() { return Err("Bluetooth is still starting; try again".into()); }
                                        if method == "PowerOn" { power_at(&c, &adapter, true, true, &preference) }
                                        else { call(&c, &adapter, "org.bluez.Adapter1", method) }
                                    });
                                    if result.is_ok() {
                                        current.powered = true;
                                        current.discovering = true;
                                        current.scan = RadioScan::Scanning;
                                        discovery = Some(Discovery::new(id, Instant::now()));
                                        log.emit(Level::Info, "bluetooth", "scan_started", "Bluetooth discovery active", Some(id), json!({}));
                                    }
                                    result
                                }
                                Command::Scan(false) => {
                                    discovery = None;
                                    current.scan = RadioScan::Idle;
                                    call(&c, &adapter, "org.bluez.Adapter1", "StopDiscovery")
                                }
                                Command::Confirm(accept) => {
                                    if let Ok(mut p) = pending.lock() {
                                        if let Some(v) = p.take() {
                                            let _ = c.send(if accept {
                                                v.message.method_return()
                                            } else {
                                                rejected(&v.message)
                                            });
                                        }
                                    }
                                    Ok(())
                                }
                                Command::Pair(path)
                                | Command::Connect(path)
                                | Command::Disconnect(path)
                                | Command::Forget(path) => {
                                    let member = operation;
                                    if !current.devices.iter().any(|d| d.path == path) {
                                        Err("unknown discovered Bluetooth device".into())
                                    } else if member == "RemoveDevice" {
                                        let r = user_intent("forget").and_then(|_lease| {
                                        let r: Result<(), _> = c
                                            .with_proxy(
                                                "org.bluez",
                                                adapter.as_str(),
                                                Duration::from_secs(3),
                                            )
                                            .method_call(
                                                "org.bluez.Adapter1",
                                                member,
                                                (DbusPath::from(path),),
                                            );
                                        r.map_err(dbus_error)
                                        }); r
                                    } else {
                                        // Pair and subsequent Connect share the Agent's
                                        // connection and operation lease. Pairable is
                                        // bounded, restored on completion/cancel/error.
                                        (|| {
                                            if operations.lock().map_err(|_| "Bluetooth operation lock poisoned")?.len() >= 16 {
                                                return Err("Too many Bluetooth operations".into());
                                            }
                                            let lease=user_intent(match member {"Pair"=>"pair","Disconnect"=>"disconnect",_=>"connect"})?;
                                            let owner=current.bluez_owner.as_deref().ok_or("BlueZ owner unavailable")?;
                                            let mut operation=Operation { id, started:Instant::now(), method:member.into(), path:path.clone(), owner:owner.into(), lease,
                                                pairing: if member == "Pair" { Some(PairableWindow::begin(&c,owner,&adapter)?) } else { None } };
                                            match operation.send(&c) {
                                                Ok(serial) => { operations.lock().map_err(|_| "Bluetooth operation lock poisoned")?.insert(serial,operation); Ok(()) }
                                                Err(error) => { operation.cancel(&c); Err(error) }
                                            }
                                        })()
                                    }
                                }
                            };
                            if let Err(e) = result {
                                current.error = Some(e.clone());
                                operation_error = Some(e.clone());
                                if scan_command { current.scan = RadioScan::Failed { message: e.clone() }; }
                                log.add("bluetooth_errors", 1.);
                                log.emit(
                                    Level::Warn,
                                    "bluetooth",
                                    "operation_failed",
                                    &e,
                                    Some(id),
                                    json!({"method":operation,"recovery_attempted":false}),
                                );
                            } else {
                                log.emit(
                                    Level::Info,
                                    "bluetooth",
                                    "operation_requested",
                                    "BlueZ operation sent",
                                    Some(id),
                                    json!({"method":operation}),
                                );
                            }
                            changed = true;
                            refresh = Instant::now() - Duration::from_secs(5);
                        }
                        let now = current
                            .devices
                            .iter()
                            .filter(|d| d.connected)
                            .map(|d| d.path.clone())
                            .collect::<Vec<_>>();
                        for p in &now {
                            if !connected.contains(p) {
                                log.add("bluetooth_connects", 1.);
                                changed = true
                            }
                        }
                        for p in &connected {
                            if !now.contains(p) {
                                log.add("bluetooth_disconnects", 1.);
                                changed = true
                            }
                        }
                        connected = now;
                        log.health_set(
                            "bluetooth",
                            if current.available {
                                if current.error.is_some() { HealthState::Degraded } else { HealthState::Ok }
                            } else {
                                HealthState::Unavailable
                            },
                            false,
                            if current.available {
                                current.error.as_deref().unwrap_or(if current.powered { "BlueZ adapter powered" } else { "Bluetooth off" })
                            } else {
                                "BlueZ unavailable"
                            },
                        );
                        if changed
                            || refresh.elapsed() < Duration::from_millis(80)
                            || current.pending.is_some()
                        {
                            let _ = et.try_send(current.clone());
                        }
                    }
                }
            })?;
        Ok(Self {
            commands: tx,
            events: er,
        })
    }
}
pub fn service_test() -> Result<serde_json::Value, String> {
    let c = Connection::new_system().map_err(dbus_error)?;
    let (_, s) = status(&c)?;
    Ok(
        json!({"passed":s.available,"adapter":s.available,"bluealsa":s.bluealsa,"connected":s.devices.iter().filter(|d|d.connected).count()}),
    )
}
pub fn scan_test(seconds: u64) -> Result<serde_json::Value, String> {
    if !(1..=15).contains(&seconds) {
        return Err("scan duration 1..15 seconds".into());
    }
    let c = Connection::new_system().map_err(dbus_error)?;
    let (adapter, before) = status(&c)?;
    let result = (|| {
        if !before.powered {
            power(&c, &adapter, true, false)?
        }
        if !before.discovering {
            call(&c, &adapter, "org.bluez.Adapter1", "StartDiscovery")?
        }
        thread::sleep(Duration::from_secs(seconds));
        let (_, s) = status(&c)?;
        Ok(json!({"passed":true,"count":s.devices.len()}))
    })();
    if !before.discovering {
        let _ = call(&c, &adapter, "org.bluez.Adapter1", "StopDiscovery");
    }
    if !before.powered {
        let _ = power(&c, &adapter, false, false);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_codec_choices_are_read_only_epoch_bound_and_do_not_need_a_request() {
        use dbus::{arg::Variant, channel::Channel};
        use std::io::{BufRead, BufReader};
        use std::process::{Command as Process, Stdio};
        let mut bus = Process::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let connect = || {
            let mut channel = Channel::open_private(address.trim()).unwrap();
            channel.register().unwrap();
            Connection::from(channel)
        };
        let server = connect();
        for name in ["org.bluez", "org.bluealsa"] {
            server.request_name(name, false, true, false).unwrap();
        }
        let owner = server.unique_name().to_string();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let received = calls.clone();
        server.start_receive(
            MatchRule::new_method_call(),
            Box::new(move |msg, conn| {
                let method = msg.member().unwrap().to_string();
                received.lock().unwrap().push(method.clone());
                let reply = match method.as_str() {
                    "Get" => {
                        let (interface, property): (String, String) = msg.read2().unwrap();
                        assert_eq!(
                            (interface.as_str(), property.as_str()),
                            ("org.bluealsa.Manager1", "Codecs")
                        );
                        msg.method_return().append1(Variant(vec![
                            "a2dp-source:SBC".to_owned(),
                            "a2dp-source:AAC".to_owned(),
                            "a2dp-source:LDAC".to_owned(),
                        ]))
                    }
                    "GetCodecs" => {
                        let mut codecs = HashMap::<String, PropMap>::new();
                        for name in ["SBC", "AAC"] {
                            let mut props = PropMap::new();
                            props.insert("Channels".into(), Variant(Box::new(vec![2_u8])));
                            props.insert("Rates".into(), Variant(Box::new(vec![44100_u32, 48000])));
                            codecs.insert(name.into(), props);
                        }
                        msg.method_return().append1(codecs)
                    }
                    "GetAll" => {
                        let mut props = PropMap::new();
                        for (key, value) in [
                            ("Device", "/org/bluez/hci0/dev_01_02_03_04_05_06"),
                            ("Transport", "A2DP-source"),
                            ("Mode", "sink"),
                            ("Codec", "SBC"),
                        ] {
                            props.insert(key.into(), Variant(Box::new(value.to_owned())));
                        }
                        props.insert("Rate".into(), Variant(Box::new(44100_u32)));
                        props.insert("Format".into(), Variant(Box::new(0x8210_u16)));
                        msg.method_return().append1(props)
                    }
                    _ => panic!("eligibility discovery must never mutate radio: {method}"),
                };
                conn.send(reply).unwrap();
                true
            }),
        );
        let quit = Arc::new(AtomicBool::new(false));
        let ending = quit.clone();
        let thread = thread::spawn(move || {
            while !ending.load(Ordering::Relaxed) {
                server.process(Duration::from_millis(10)).unwrap();
            }
        });
        let directory = std::env::temp_dir().join(format!("reborn-codec-discovery-{}", bus.id()));
        std::fs::create_dir_all(&directory).unwrap();
        let inventory = directory.join("inventory.json");
        std::fs::write(&inventory, serde_json::to_vec(&json!({"schema":1,"codecs":{
            "SBC":{"compiled_locally":true,"distribution_approved":true,"platform_qualified":false},
            "AAC":{"compiled_locally":true,"distribution_approved":false,"platform_qualified":false,"owner_private_experiment":true},
            "LDAC":{"compiled_locally":true,"distribution_approved":false,"platform_qualified":false,"owner_private_experiment":true}
        }})).unwrap()).unwrap();
        let mut current = Status {
            available: true,
            powered: true,
            bluealsa: true,
            bluez_owner: Some(owner.clone()),
            bluealsa_owner: Some(owner),
            devices: vec![Device {
                path: "/org/bluez/hci0/dev_01_02_03_04_05_06".into(),
                address: "01:02:03:04:05:06".into(),
                connected: true,
                audio: true,
                ..Default::default()
            }],
            pcms: vec![BluetoothPcm {
                object: "/org/bluealsa/hci0/dev_01_02_03_04_05_06/a2dp".into(),
                device: "/org/bluez/hci0/dev_01_02_03_04_05_06".into(),
                transport: "A2DP-source".into(),
                mode: "sink".into(),
                codec: Some("SBC".into()),
                rate: Some(44100),
                format: Some(0x8210),
                channels: Some(2),
                running: Some(true),
                transport_generation: 17,
            }],
            ..Default::default()
        };
        let client = connect();
        current.codec_policy = refresh_codec_policy(None, &current, || {
            discover_codec_policy(&client, &current, &inventory, true)
        });
        assert_eq!(
            current.codec_choices(),
            vec![
                CodecPreference::Auto,
                CodecPreference::Sbc,
                CodecPreference::Aac
            ]
        );
        let policy = current.codec_policy.as_ref().unwrap();
        assert!(
            policy.requested.is_none() && policy.selected.is_none() && policy.visited.is_empty()
        );
        assert_eq!(policy.state, "ObservedCapabilities");
        assert_eq!(*calls.lock().unwrap(), vec!["Get", "GetCodecs", "GetAll"]);
        assert!(refresh_codec_policy(Some(policy), &current, || panic!(
            "unchanged epoch is cached"
        ))
        .is_some());
        let previous = current.codec_policy.clone();
        current.pcms[0].transport_generation += 1;
        assert!(
            refresh_codec_policy(previous.as_ref(), &current, || Err("peer changed".into()))
                .is_none()
        );
        current.devices[0].connected = false;
        assert!(refresh_codec_policy(previous.as_ref(), &current, || panic!(
            "disconnected peer must not be queried"
        ))
        .is_none());
        quit.store(true, Ordering::Relaxed);
        thread.join().unwrap();
        bus.kill().unwrap();
        bus.wait().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn codec_rows_only_offer_policy_enabled_codecs_the_peer_supports() {
        use crate::codecs::{Capability, Inventory, Session};
        let mut inventory = Inventory {
            schema: 1,
            ..Default::default()
        };
        for (name, approved) in [("SBC", true), ("AAC", false), ("LDAC", false)] {
            inventory.codecs.insert(
                name.into(),
                Capability {
                    compiled_locally: true,
                    distribution_approved: approved,
                    platform_qualified: approved,
                    owner_private_experiment: !approved,
                },
            );
        }
        let sbc = vec!["SBC".to_string()];
        let all = vec!["SBC".into(), "AAC".into(), "LDAC".into()];
        let mut status = Status {
            codec_policy: Some(Session::new(
                1,
                CodecPreference::Sbc,
                &inventory,
                &sbc,
                &sbc,
            )),
            ..Default::default()
        };
        assert!(status.codec_choices().is_empty(), "SBC only: no dead row");
        // Optional codecs built but runtime-disabled stay hidden.
        status.codec_policy = Some(Session::new(
            1,
            CodecPreference::Sbc,
            &inventory,
            &sbc,
            &all,
        ));
        assert!(status.codec_choices().is_empty());
        // Explicit experiment enabled at runtime and supported by the peer.
        status.codec_policy = Some(Session::new_experimental(
            1,
            CodecPreference::Sbc,
            &inventory,
            &all,
            &["SBC".into(), "AAC".into()],
            true,
        ));
        assert_eq!(
            status.codec_choices(),
            vec![
                CodecPreference::Auto,
                CodecPreference::Sbc,
                CodecPreference::Aac
            ]
        );
    }
    #[test]
    fn worker_discovery_publishes_devices_stops_and_retains_failure() {
        use dbus::{arg::Variant, channel::Channel};
        use std::{
            fs,
            io::{BufRead, BufReader},
            process::{Child, Command as Process, Stdio},
            sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        // A private bus and fake adapter: no host system bus or real radio access.
        struct Bus(Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut bus = Bus(Process::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap());
        let mut address = String::new();
        BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        fn connect(address: &str) -> Result<Connection, dbus::Error> {
            let mut channel = Channel::open_private(address.trim())?;
            channel.register()?;
            Ok(channel.into())
        }
        let service = connect(&address).unwrap();
        service
            .request_name("org.bluez", false, true, false)
            .unwrap();
        service
            .request_name("org.bluealsa", false, true, false)
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let ending = stop.clone();
        let starts = Arc::new(AtomicUsize::new(0));
        let starts_out = starts.clone();
        let stops = Arc::new(AtomicUsize::new(0));
        let stops_out = stops.clone();
        let fail = Arc::new(AtomicBool::new(false));
        let fail_scan = fail.clone();
        let pcm_rate = Arc::new(AtomicUsize::new(44_100));
        let observed_pcm_rate = pcm_rate.clone();
        let pcm_rate_for_method = pcm_rate.clone();
        let recreate_pcm = Arc::new(AtomicBool::new(false));
        let recreate_pcm_out = recreate_pcm.clone();
        let pair_mode = Arc::new(AtomicUsize::new(0));
        let pair_mode_out = pair_mode.clone();
        let pairable = Arc::new(AtomicBool::new(false));
        let pairable_out = pairable.clone();
        let pair_timeout = Arc::new(AtomicUsize::new(180));
        let pair_timeout_out = pair_timeout.clone();
        let connects = Arc::new(AtomicUsize::new(0));
        let connects_out = connects.clone();
        let cancelled = Arc::new(AtomicUsize::new(0));
        let cancelled_out = cancelled.clone();
        let (mut paired, mut bonded, mut trusted, mut peer_connected) =
            (false, false, false, false);
        let mut powered = false;
        let mut discovering = false;
        let mut found = false;
        service.start_receive(
            MatchRule::new_method_call(),
            Box::new(move |msg, c| {
                let member = msg.member().unwrap().to_string();
                let mut reply = msg.method_return();
                match member.as_str() {
                    "GetManagedObjects" => {
                        let mut objects = Objects::new();
                        if msg.path().unwrap() == "/org/bluealsa" {
                            let mut properties = PropMap::new();
                            properties.insert(
                                "Device".into(),
                                Variant(Box::new(
                                    "/org/bluez/hci0/dev_01_02_03_04_05_06".to_string(),
                                )),
                            );
                            properties.insert(
                                "Transport".into(),
                                Variant(Box::new("A2DP-source".to_string())),
                            );
                            properties.insert("Mode".into(), Variant(Box::new("sink".to_string())));
                            properties.insert("Codec".into(), Variant(Box::new("SBC".to_string())));
                            properties.insert("Format".into(), Variant(Box::new(0x8210u64)));
                            properties.insert(
                                "Rate".into(),
                                Variant(Box::new(
                                    pcm_rate_for_method.load(Ordering::Relaxed) as u32
                                )),
                            );
                            properties.insert("Channels".into(), Variant(Box::new(2u64)));
                            objects.insert(
                                DbusPath::from("/org/bluealsa/hci0/dev_01_02_03_04_05_06/a2dp"),
                                HashMap::from([("org.bluealsa.PCM1".into(), properties)]),
                            );
                        } else {
                            let mut properties = PropMap::new();
                            properties.insert("Powered".into(), Variant(Box::new(powered)));
                            properties.insert("Discovering".into(), Variant(Box::new(discovering)));
                            objects.insert(
                                DbusPath::from("/org/bluez/hci0"),
                                HashMap::from([("org.bluez.Adapter1".into(), properties)]),
                            );
                            if found {
                                let mut properties = PropMap::new();
                                properties.insert(
                                    "Alias".into(),
                                    Variant(Box::new("Test headphones".to_string())),
                                );
                                properties.insert(
                                    "Address".into(),
                                    Variant(Box::new("01:02:03:04:05:06".to_string())),
                                );
                                properties.insert("Paired".into(), Variant(Box::new(paired)));
                                properties.insert("Bonded".into(), Variant(Box::new(bonded)));
                                properties.insert("Trusted".into(), Variant(Box::new(trusted)));
                                properties
                                    .insert("Connected".into(), Variant(Box::new(peer_connected)));
                                properties.insert(
                                    "UUIDs".into(),
                                    Variant(Box::new(vec![
                                        "0000110b-0000-1000-8000-00805f9b34fb".to_string()
                                    ])),
                                );
                                objects.insert(
                                    DbusPath::from("/org/bluez/hci0/dev_01_02_03_04_05_06"),
                                    HashMap::from([("org.bluez.Device1".into(), properties)]),
                                );
                            }
                        }
                        reply = reply.append1(objects);
                    }
                    "Set" => {
                        let (_, property, value): (String, String, Variant<Box<dyn RefArg>>) =
                            msg.read3().unwrap();
                        match property.as_str() {
                            "Powered" => powered = value.0.as_i64() == Some(1),
                            "Pairable" => {
                                pairable_out.store(value.0.as_i64() == Some(1), Ordering::Relaxed)
                            }
                            "PairableTimeout" => pair_timeout_out
                                .store(value.0.as_u64().unwrap() as usize, Ordering::Relaxed),
                            "Trusted" => trusted = value.0.as_i64() == Some(1),
                            _ => panic!("unexpected property {property}"),
                        }
                    }
                    "Get" => {
                        let (_, property): (String, String) = msg.read2().unwrap();
                        reply = match property.as_str() {
                            "Pairable" => {
                                reply.append1(Variant(pairable_out.load(Ordering::Relaxed)))
                            }
                            "PairableTimeout" => reply
                                .append1(Variant(pair_timeout_out.load(Ordering::Relaxed) as u32)),
                            "Paired" => reply.append1(Variant(paired)),
                            "Bonded" => reply.append1(Variant(bonded)),
                            _ => panic!("unexpected property {property}"),
                        };
                    }
                    "Pair" => {
                        assert!(pairable_out.load(Ordering::Relaxed));
                        assert_eq!(pair_timeout_out.load(Ordering::Relaxed), 90);
                        paired = false;
                        bonded = false;
                        trusted = false;
                        peer_connected = false;
                        match pair_mode_out.load(Ordering::Relaxed) {
                            0 => {
                                paired = true;
                                bonded = true;
                            }
                            1 => paired = true, // successful Pair reply without persistent keys
                            2 => {
                                reply = msg.error(
                                    &"org.bluez.Error.AuthenticationRejected".into(),
                                    c"injected pairing rejection",
                                )
                            }
                            3 => return true, // owner cancels an unanswered Pair
                            _ => unreachable!(),
                        }
                    }
                    "Connect" => {
                        assert!(paired && bonded && trusted);
                        assert!(!pairable_out.load(Ordering::Relaxed));
                        assert_eq!(pair_timeout_out.load(Ordering::Relaxed), 180);
                        peer_connected = true;
                        connects_out.fetch_add(1, Ordering::Relaxed);
                    }
                    "CancelPairing" => {
                        cancelled_out.fetch_add(1, Ordering::Relaxed);
                    }
                    "StartDiscovery" => {
                        assert!(powered, "must power on before discovery");
                        starts_out.fetch_add(1, Ordering::Relaxed);
                        if fail_scan.load(Ordering::Relaxed) {
                            reply = msg.error(
                                &"org.bluez.Error.Failed".into(),
                                c"injected adapter failure",
                            );
                        } else {
                            discovering = true;
                            found = true;
                        }
                    }
                    "StopDiscovery" => {
                        discovering = false;
                        stops_out.fetch_add(1, Ordering::Relaxed);
                    }
                    "RegisterAgent" => {}
                    _ => panic!("unexpected method {member}"),
                }
                c.send(reply).unwrap();
                true
            }),
        );
        let server = thread::spawn(move || {
            let mut last_rate = 44_100;
            while !ending.load(Ordering::Relaxed) {
                service.process(Duration::from_millis(10)).unwrap();
                let rate = observed_pcm_rate.load(Ordering::Relaxed) as u32;
                if rate != last_rate {
                    let mut changed = PropMap::new();
                    changed.insert("Rate".into(), Variant(Box::new(rate)));
                    service
                        .send(
                            Message::new_signal(
                                "/org/bluealsa/hci0/dev_01_02_03_04_05_06/a2dp",
                                "org.freedesktop.DBus.Properties",
                                "PropertiesChanged",
                            )
                            .unwrap()
                            .append3(
                                "org.bluealsa.PCM1",
                                changed,
                                Vec::<String>::new(),
                            ),
                        )
                        .unwrap();
                    last_rate = rate;
                }
                if recreate_pcm_out.swap(false, Ordering::AcqRel) {
                    let object = DbusPath::from("/org/bluealsa/hci0/dev_01_02_03_04_05_06/a2dp");
                    service
                        .send(
                            Message::new_signal(
                                "/org/bluealsa",
                                "org.freedesktop.DBus.ObjectManager",
                                "InterfacesRemoved",
                            )
                            .unwrap()
                            .append2(object.clone(), vec!["org.bluealsa.PCM1".to_string()]),
                        )
                        .unwrap();
                    let mut properties = PropMap::new();
                    properties.insert(
                        "Device".into(),
                        Variant(Box::new(
                            "/org/bluez/hci0/dev_01_02_03_04_05_06".to_string(),
                        )),
                    );
                    properties.insert(
                        "Transport".into(),
                        Variant(Box::new("A2DP-source".to_string())),
                    );
                    properties.insert("Mode".into(), Variant(Box::new("sink".to_string())));
                    properties.insert("Codec".into(), Variant(Box::new("SBC".to_string())));
                    properties.insert("Format".into(), Variant(Box::new(0x8210u64)));
                    properties.insert("Rate".into(), Variant(Box::new(rate)));
                    properties.insert("Channels".into(), Variant(Box::new(2u64)));
                    service
                        .send(
                            Message::new_signal(
                                "/org/bluealsa",
                                "org.freedesktop.DBus.ObjectManager",
                                "InterfacesAdded",
                            )
                            .unwrap()
                            .append2(
                                object,
                                HashMap::from([("org.bluealsa.PCM1".to_string(), properties)]),
                            ),
                        )
                        .unwrap();
                }
            }
        });
        let directory = std::env::temp_dir().join(format!("rb-bluez-{}", bus.0.id()));
        fs::create_dir_all(&directory).unwrap();
        let log = Observer::new(&directory.join("logs")).unwrap();
        let bt = Bluetooth::spawn_with(
            log.clone(),
            move || connect(&address),
            directory.join("enabled"),
        )
        .unwrap();
        fn wait(events: &Receiver<Status>, matches: impl Fn(&Status) -> bool) -> Status {
            let end = Instant::now() + Duration::from_secs(20);
            loop {
                let s = events
                    .recv_timeout(end.saturating_duration_since(Instant::now()))
                    .unwrap();
                if matches(&s) {
                    return s;
                }
            }
        }
        wait(&bt.events, |s| s.available && !s.powered);
        let initial_pcm = wait(&bt.events, |s| !s.pcms.is_empty());
        let initial_epoch = initial_pcm.pcms[0].transport_generation;
        pcm_rate.store(48_000, Ordering::Release);
        let changed_pcm = wait(&bt.events, |s| {
            s.pcms.first().is_some_and(|pcm| {
                pcm.rate == Some(48_000) && pcm.transport_generation != initial_epoch
            })
        });
        let changed_epoch = changed_pcm.pcms[0].transport_generation;
        recreate_pcm.store(true, Ordering::Release);
        let recreated_pcm = wait(&bt.events, |s| {
            s.pcms.first().is_some_and(|pcm| {
                pcm.rate == Some(48_000) && pcm.transport_generation != changed_epoch
            })
        });
        assert_ne!(recreated_pcm.pcms[0].transport_generation, initial_epoch);
        bt.commands.send(Command::Scan(true)).unwrap();
        wait(&bt.events, |s| s.scan == RadioScan::Starting);
        let scanning = wait(&bt.events, |s| {
            s.scan == RadioScan::Scanning && !s.devices.is_empty()
        });
        assert_eq!(scanning.devices[0].name, "Test headphones");
        assert!(scanning.devices[0].audio);
        assert_eq!(
            fs::read_to_string(directory.join("enabled")).unwrap(),
            "1\n"
        );
        bt.commands.send(Command::Scan(true)).unwrap(); // same operation stays active
        let complete = wait(&bt.events, |s| matches!(s.scan, RadioScan::Complete { .. }));
        assert_eq!(complete.scan, RadioScan::Complete { found: 1 });
        assert!(!complete.discovering);
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        assert_eq!(stops.load(Ordering::Relaxed), 1);
        let peer = complete.devices[0].path.clone();
        pair_mode.store(1, Ordering::Relaxed);
        bt.commands.send(Command::Pair(peer.clone())).unwrap();
        wait(&bt.events, |s| {
            s.error.as_deref().is_some_and(|e| e.contains("saved bond"))
        });
        assert_eq!(connects.load(Ordering::Relaxed), 0);
        assert!(!pairable.load(Ordering::Relaxed));
        assert_eq!(pair_timeout.load(Ordering::Relaxed), 180);
        pair_mode.store(2, Ordering::Relaxed);
        bt.commands.send(Command::Pair(peer.clone())).unwrap();
        wait(&bt.events, |s| {
            s.error
                .as_deref()
                .is_some_and(|e| e.contains("AuthenticationRejected"))
        });
        assert_eq!(connects.load(Ordering::Relaxed), 0);
        assert!(!pairable.load(Ordering::Relaxed));
        pair_mode.store(0, Ordering::Relaxed);
        bt.commands.send(Command::Pair(peer.clone())).unwrap();
        let connected = wait(&bt.events, |s| {
            s.devices
                .iter()
                .any(|d| d.bonded && d.trusted && d.connected)
        });
        assert!(connected.error.is_none());
        assert_eq!(connects.load(Ordering::Relaxed), 1);
        fail.store(true, Ordering::Relaxed);
        bt.commands.send(Command::Scan(true)).unwrap();
        wait(&bt.events, |s| matches!(s.scan, RadioScan::Failed { .. }));
        // A periodic successful ObjectManager read must not erase the operation error.
        let failed = wait(&bt.events, |s| matches!(s.scan, RadioScan::Failed { .. }));
        assert!(failed.error.unwrap().contains("org.bluez.Error.Failed"));
        bt.commands.send(Command::Power(false)).unwrap();
        let off = wait(&bt.events, |s| !s.powered);
        assert_eq!(off.scan, RadioScan::Idle);
        assert!(off.error.is_none());
        bt.commands.send(Command::Power(true)).unwrap();
        wait(&bt.events, |s| s.powered);
        pair_mode.store(3, Ordering::Relaxed);
        bt.commands.send(Command::Pair(peer)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !pairable.load(Ordering::Relaxed) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(pairable.load(Ordering::Relaxed));
        bt.commands.send(Command::Stop).unwrap();
        while bt.events.recv_timeout(Duration::from_secs(2)).is_ok() {}
        assert_eq!(cancelled.load(Ordering::Relaxed), 1);
        assert!(!pairable.load(Ordering::Relaxed));
        assert_eq!(pair_timeout.load(Ordering::Relaxed), 180);
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        drop(bt);
        drop(log);
        let _ = fs::remove_dir_all(directory);
    }
    #[test]
    fn scan_powers_on_before_discovery_and_preserves_errors() {
        let mut calls = vec![];
        start_scan(false, |method| {
            calls.push(method.to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, ["PowerOn", "StartDiscovery"]);
        calls.clear();
        start_scan(true, |method| {
            calls.push(method.to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, ["StartDiscovery"]);
        calls.clear();
        let error = start_scan(false, |method| {
            calls.push(method.to_string());
            Err("org.bluez.Error.NotReady".into())
        })
        .unwrap_err();
        assert_eq!(calls, ["PowerOn"]);
        assert_eq!(error, "org.bluez.Error.NotReady");
        assert!(start_scan(true, |_| Err("org.bluez.Error.Failed".into())).is_err());
    }
    #[test]
    fn discovery_has_a_fixed_deadline() {
        let now = Instant::now();
        let scan = Discovery::new(7, now);
        assert!(!scan.expired(now + Duration::from_secs(14)));
        assert!(scan.expired(now + Duration::from_secs(15)));
    }
    fn connected() -> Status {
        Status {
            bluealsa: true,
            devices: vec![Device {
                path: "/org/bluez/hci0/dev_12_34_56_78_90_AB".into(),
                address: "12:34:56:78:90:AB".into(),
                connected: true,
                audio: true,
                ..Default::default()
            }],
            pcms: vec![BluetoothPcm {
                device: "/org/bluez/hci0/dev_12_34_56_78_90_AB".into(),
                transport: "A2DP-source".into(),
                mode: "sink".into(),
                format: Some(0x8210),
                rate: Some(44100),
                channels: Some(2),
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    #[test]
    fn selects_negotiated_rate_for_exact_peer_and_direction() {
        let mut s = connected();
        assert_eq!(s.playback_rate("12:34:56:78:90:ab").unwrap(), 44100);
        s.pcms[0].rate = Some(48000);
        assert_eq!(s.playback_rate("12:34:56:78:90:AB").unwrap(), 48000);
        s.pcms[0].device = "/org/bluez/hci0/dev_other".into();
        assert!(s.playback_rate("12:34:56:78:90:AB").is_err());
        s = connected();
        s.pcms[0].mode = "source".into();
        assert!(s.playback_rate("12:34:56:78:90:AB").is_err());
        s = connected();
        s.pcms[0].transport = "HFP-AG".into();
        assert!(s.playback_rate("12:34:56:78:90:AB").is_err());
    }
    #[test]
    fn unavailable_and_unsupported_pcm_never_silently_change_rate() {
        for (rate, channels) in [(Some(96000), Some(2)), (Some(44100), Some(1))] {
            let mut s = connected();
            s.pcms[0].rate = rate;
            s.pcms[0].channels = channels;
            assert!(s.playback_rate("12:34:56:78:90:AB").is_err());
        }
        let mut s = connected();
        s.pcms.clear();
        assert!(s.playback_rate("12:34:56:78:90:AB").is_err());
        s = connected();
        s.devices[0].connected = false;
        assert!(s.playback_rate("12:34:56:78:90:AB").is_err());
    }
    #[test]
    fn bluealsa_pcm_format_is_typed_and_unknown_is_not_sbc() {
        let mut s = connected();
        assert_eq!(
            s.pcms[0].negotiated_format().unwrap(),
            reborn_core::PcmFormat::S16LE
        );
        s.pcms[0].format = Some(0x8418);
        assert_eq!(
            s.pcms[0].negotiated_format().unwrap(),
            reborn_core::PcmFormat::S24LE
        );
        s.pcms[0].format = None;
        assert!(s.pcms[0].negotiated_format().is_err());
    }

    fn observed_transport() -> Status {
        Status {
            bluez_owner: Some(":1.20".into()),
            bluealsa_owner: Some(":1.21".into()),
            pcms: vec![BluetoothPcm {
                object: "/org/bluealsa/hci0/dev_12_34_56_78_90_AB/a2dp".into(),
                device: "/org/bluez/hci0/dev_12_34_56_78_90_AB".into(),
                transport: "A2DP-source".into(),
                mode: "sink".into(),
                codec: Some("SBC".into()),
                format: Some(0x8210),
                rate: Some(44_100),
                channels: Some(2),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn transport_epoch_changes_for_same_path_reuse_properties_and_daemon_sessions() {
        let object = "/org/bluealsa/hci0/dev_12_34_56_78_90_AB/a2dp";
        let mut status = observed_transport();
        let mut epochs = TransportEpochs::default();
        epochs.refresh(&mut status, &HashSet::new(), false);
        let initial = status.pcms[0].transport_generation;
        assert_ne!(initial, 0);

        // A remove-and-recreate between polls may present the same object path
        // and properties. The ObjectManager removal signal still invalidates it.
        epochs.refresh(&mut status, &HashSet::from([object.to_string()]), false);
        let recreated = status.pcms[0].transport_generation;
        assert_ne!(recreated, initial);

        let changes: [fn(&mut BluetoothPcm); 8] = [
            |pcm| pcm.device.push_str("_replacement"),
            |pcm| pcm.transport.push_str("_replacement"),
            |pcm| pcm.mode.push_str("_replacement"),
            |pcm| pcm.codec = Some("other".into()),
            |pcm| pcm.format = Some(0x8420),
            |pcm| pcm.rate = Some(48_000),
            |pcm| pcm.channels = Some(1),
            |pcm| pcm.object.push_str("_replacement"),
        ];
        for change in changes {
            let mut changed = observed_transport();
            let mut tracker = TransportEpochs::default();
            tracker.refresh(&mut changed, &HashSet::new(), false);
            let before = changed.pcms[0].transport_generation;
            change(&mut changed.pcms[0]);
            tracker.refresh(&mut changed, &HashSet::new(), false);
            assert_ne!(changed.pcms[0].transport_generation, before);
        }

        // NameOwnerChanged invalidation catches a daemon restart even if it
        // completed before the next GetNameOwner snapshot.
        let mut restarted = observed_transport();
        let mut tracker = TransportEpochs::default();
        tracker.refresh(&mut restarted, &HashSet::new(), false);
        let before = restarted.pcms[0].transport_generation;
        tracker.refresh(&mut restarted, &HashSet::new(), true);
        assert_ne!(restarted.pcms[0].transport_generation, before);

        let mut new_worker = TransportEpochs::default();
        let mut after_worker_restart = observed_transport();
        new_worker.refresh(&mut after_worker_restart, &HashSet::new(), false);
        assert_ne!(
            after_worker_restart.pcms[0].transport_generation, initial,
            "worker restart must not reuse a process-local stale sink epoch"
        );
    }
}

#[cfg(test)]
mod pcm_lease_tests {
    #[test]
    fn open_pcm_excludes_codec_change_and_unknown_outcome_blocks_reopen() {
        let dir = std::env::temp_dir().join(format!("reborn-pcm-lease-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let opened = super::pcm_lease_at(&dir).unwrap();
        let exclusive = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("bt-pcm.lock"))
            .unwrap();
        assert!(exclusive.try_lock().is_err());
        drop(opened);
        // Concurrent tests in this binary fork subprocesses; a child holds a
        // duplicate descriptor only until its exec closes it (O_CLOEXEC).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while exclusive.try_lock().is_err() {
            assert!(std::time::Instant::now() < deadline, "lease not released");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(super::pcm_lease_at(&dir).is_err());
        std::fs::write(dir.join("bt-codec-uncertain.json"), b"{}").unwrap();
        drop(exclusive);
        assert!(super::pcm_lease_at(&dir).is_err());
        std::fs::remove_file(dir.join("bt-codec-uncertain.json")).unwrap();
        // The exclusive descriptor can be inherited across the same concurrent
        // fork interval. Require its real release within the same bounded wait.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if super::pcm_lease_at(&dir).is_ok() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "exclusive lease not released"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
