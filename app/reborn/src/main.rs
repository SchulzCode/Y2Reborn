#![forbid(unsafe_code)]
mod diagnostics;
mod playback;
use reborn_control::{Command, PlaybackAction, Response};
use reborn_core::{AppModel, AudioOutput, Event, PlaybackState, Screen, Source, Track};
use reborn_graphics::Renderer;
use reborn_library::{Database, Filter, Scanner};
use reborn_observability::{HealthState, Level, Observer};
use reborn_platform::{bluetooth, input, power, storage, wifi};
use reborn_ui::{Effect, Item, Ui};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        mpsc::{Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
struct Runtime {
    model: AppModel,
    ui: Ui,
    tracks: Vec<Track>,
    playback: playback::Playback,
    log: Observer,
    db: Database,
    scanner: Scanner,
    wifi: Option<wifi::Wifi>,
    bluetooth: Option<bluetooth::Bluetooth>,
    wifi_state: wifi::Status,
    bt_state: bluetooth::Status,
    graphics: Option<Renderer>,
    power: Value,
    scan_state: Value,
    art: bool,
    dirty: bool,
    root: PathBuf,
    headless: bool,
    last_activity: Instant,
    query: Option<Receiver<Result<Vec<Track>, String>>>,
}
impl Runtime {
    fn fail(&mut self, sub: &str, error: String) {
        match self.model.screen {
            Screen::Wifi if sub == "ui" => self.ui.wifi.failed(error.clone()),
            Screen::Bluetooth if sub == "ui" => self.ui.bluetooth.failed(error.clone()),
            _ => self.ui.notice = error.clone(),
        }
        self.log.emit(
            Level::Error,
            sub,
            "operation_failed",
            &error,
            None,
            json!({"state":self.model.playback,"recovery_attempted":false}),
        );
        self.dirty = true;
    }
    fn checkpoint(&self) {
        if let Err(e) = self.model.checkpoint(&self.root.join("state/session.json")) {
            self.log.emit(
                Level::Warn,
                "core",
                "checkpoint_failed",
                &e.to_string(),
                None,
                json!({}),
            );
        }
    }
    fn load(&mut self) -> Result<(), String> {
        let track = self.model.current().cloned().ok_or("queue is empty")?;
        if !track.path.is_file() {
            return Err("track source unavailable".into());
        }
        let requested_rate = match &self.model.output {
            AudioOutput::Wired if track.sample_rate >= 8_000 => track.sample_rate,
            _ => self.output_rate()?,
        };
        let (spec, _planned) = match reborn_audio::AlsaSink::plan(
            &self.model.output,
            requested_rate,
            self.log.clone(),
            self.log.correlation(),
        ) {
            Ok(plan) => plan,
            Err(error)
                if matches!(&self.model.output, AudioOutput::Wired) && requested_rate != 44_100 =>
            {
                self.log.emit(
                    Level::Warn,
                    "audio",
                    "qualified_rate_fallback",
                    "Source rate is not in the current wired qualification profile",
                    Some(self.log.correlation()),
                    json!({"source_rate":requested_rate,"selected_rate":44100,"reason":error}),
                );
                reborn_audio::AlsaSink::plan(
                    &self.model.output,
                    44_100,
                    self.log.clone(),
                    self.log.correlation(),
                )?
            }
            Err(error) => return Err(error),
        };
        let queue = self
            .model
            .queue
            .iter()
            .skip(self.model.queue_position + 1)
            .cloned()
            .collect();
        let dsp = reborn_media::DspConfig {
            volume: self.model.settings.volume,
            replay_gain: self.model.settings.replay_gain,
            eq_enabled: self.model.settings.eq_enabled,
            eq_bands: self.model.settings.eq_bands.clone(),
            crossfade_ms: self.model.settings.crossfade_ms,
        };
        self.model.invalidate();
        self.model.playback = PlaybackState::Buffering;
        self.art = false;
        self.dirty = true;
        self.playback.load(
            track,
            queue,
            self.model.position_ms,
            spec,
            self.model.generation,
            dsp,
            self.log.correlation(),
        )
    }
    fn output_rate(&self) -> Result<u32, String> {
        match &self.model.output {
            AudioOutput::Wired => Ok(44100),
            AudioOutput::Bluetooth(address) => self.bt_state.playback_rate(address),
        }
    }
    fn pause(&mut self) {
        self.log.emit(
            Level::Info,
            "playback",
            "paused",
            "Playback paused and sink released",
            None,
            json!({"position_ms":self.model.position_ms}),
        );
        self.model.invalidate();
        self.playback.stop(self.model.generation);
        self.model.playback = PlaybackState::Paused;
        self.checkpoint();
        self.dirty = true;
    }
    fn toggle(&mut self) -> Result<(), String> {
        if matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            self.pause();
            Ok(())
        } else {
            self.load()
        }
    }
    fn switch(&mut self, out: AudioOutput) -> Result<(), String> {
        if let AudioOutput::Bluetooth(address) = &out {
            self.bt_state.playback_rate(address)?;
        }
        let active = matches!(
            self.model.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        self.model.output = out;
        self.log.emit(
            Level::Info,
            "audio",
            "output_switch",
            "Output preference changed",
            Some(self.log.correlation()),
            json!({"output":self.model.output}),
        );
        if active {
            self.load()?
        }
        self.checkpoint();
        self.dirty = true;
        Ok(())
    }
    fn effect(&mut self, e: Effect) -> Result<(), String> {
        self.dirty = true;
        match e {
            Effect::None => {}
            Effect::Play(index) => {
                let t = self.tracks.get(index).ok_or("invalid selection")?;
                if !t.online {
                    return Err("media source offline".into());
                }
                self.model.replace_queue(self.tracks.clone(), index)?;
                self.load()?;
            }
            Effect::Toggle => self.toggle()?,
            Effect::Next | Effect::Previous => {
                let delta = if matches!(e, Effect::Next) { 1 } else { -1 };
                if self.model.step(delta) {
                    self.load()?
                } else {
                    self.model.invalidate();
                    self.playback.stop(self.model.generation);
                }
            }
            Effect::Seek(delta) => {
                self.model.position_ms = self.model.position_ms.saturating_add_signed(delta).min(
                    self.model
                        .current()
                        .map(|t| t.duration_ms.saturating_sub(1))
                        .unwrap_or(0),
                );
                if self.model.playback == PlaybackState::Playing {
                    self.load()?
                }
            }
            Effect::Volume(delta) => {
                self.model.settings.volume =
                    (self.model.settings.volume as i16 + delta as i16).clamp(0, 100) as u8;
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                self.ui.notice = format!("Volume {}", self.model.settings.volume);
            }
            Effect::Scan => {
                self.scanner.scan(self.model.sources.clone())?;
                self.scan_state = json!({"state":"scanning"});
                self.ui.notice = "Scanning music".into();
            }
            Effect::WifiPower => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi service unavailable")?
                .commands
                .try_send(wifi::Command::Power(!self.wifi_state.enabled))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiScan => {
                if self.ui.wifi.scan.active() {
                    return Ok(());
                }
                self.wifi
                    .as_ref()
                    .ok_or("Wi-Fi unavailable")?
                    .commands
                    .try_send(wifi::Command::Scan)
                    .map_err(|_| "Wi-Fi busy")?;
                self.ui.wifi.requested();
                self.wifi_state.scan = reborn_core::RadioScan::Starting;
            }
            Effect::WifiConnect { ssid, password } => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Connect { ssid, password })
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiSaved(id) => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Saved(id))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::WifiForget(id) => self
                .wifi
                .as_ref()
                .ok_or("Wi-Fi unavailable")?
                .commands
                .try_send(wifi::Command::Forget(id))
                .map_err(|_| "Wi-Fi busy")?,
            Effect::BluetoothPower => self
                .bluetooth
                .as_ref()
                .ok_or("Bluetooth unavailable")?
                .commands
                .try_send(bluetooth::Command::Power(!self.bt_state.powered))
                .map_err(|_| "Bluetooth busy")?,
            Effect::BluetoothScan => {
                if self.ui.bluetooth.scan.active() {
                    return Ok(());
                }
                self.bluetooth
                    .as_ref()
                    .ok_or("Bluetooth unavailable")?
                    .commands
                    .try_send(bluetooth::Command::Scan(true))
                    .map_err(|_| "Bluetooth busy")?;
                self.ui.bluetooth.requested();
                self.bt_state.scan = reborn_core::RadioScan::Starting;
            }
            Effect::BluetoothDevice { path, operation } => {
                if operation == "output" {
                    let address = self
                        .bt_state
                        .devices
                        .iter()
                        .find(|d| d.path == path)
                        .ok_or("device not found")?
                        .address
                        .clone();
                    self.switch(AudioOutput::Bluetooth(address))?;
                } else {
                    let cmd = match operation.as_str() {
                        "pair" => bluetooth::Command::Pair(path),
                        "connect" => bluetooth::Command::Connect(path),
                        "disconnect" => bluetooth::Command::Disconnect(path),
                        "forget" => bluetooth::Command::Forget(path),
                        _ => return Err("unsupported Bluetooth action".into()),
                    };
                    self.bluetooth
                        .as_ref()
                        .ok_or("Bluetooth unavailable")?
                        .commands
                        .try_send(cmd)
                        .map_err(|_| "Bluetooth busy")?;
                }
            }
            Effect::Output(o) => self.switch(o)?,
            Effect::ConfirmPairing(ok) => self
                .bluetooth
                .as_ref()
                .ok_or("Bluetooth unavailable")?
                .commands
                .try_send(bluetooth::Command::Confirm(ok))
                .map_err(|_| "Bluetooth busy")?,
            Effect::ScreenToggle => {
                if !self.headless {
                    power::blank(!self.model.screen_off)?;
                }
                self.model.screen_off = !self.model.screen_off;
                self.log.emit(
                    Level::Info,
                    "power",
                    if self.model.screen_off {
                        "screen_blank"
                    } else {
                        "screen_wake"
                    },
                    "Display power changed",
                    None,
                    json!({"playback":self.model.playback}),
                );
            }
            Effect::Checkpoint => self.checkpoint(),
        };
        Ok(())
    }
    fn status(&self) -> Value {
        json!({"version":reborn_core::VERSION,"build_id":option_env!("REBORN_BUILD_ID").unwrap_or("development"),"session":self.log.session(),"uptime_seconds":self.log.uptime(),"current_screen":self.model.screen,"screen_off":self.model.screen_off,"playback":{"state":self.model.playback,"track_id":self.model.current().map(|t|t.id),"position_ms":self.model.position_ms,"duration_ms":self.model.current().map(|t|t.duration_ms),"queue_length":self.model.queue.len(),"queue_position":self.model.queue_position,"generation":self.model.generation},"output":self.model.output,"audio":self.playback.audio_state(),"library":{"tracks_loaded":self.tracks.len(),"schema":reborn_library::SCHEMA_VERSION},"scanner":self.scan_state,"wifi":self.wifi_state,"bluetooth":self.bt_state,"storage":self.model.sources,"power":self.power,"graphics":{"available":self.graphics.is_some(),"renderer":self.graphics.as_ref().map(|g|&g.info),"headless":self.headless},"decoder":{"ffmpeg":reborn_media::version(),"runtime":reborn_media::runtime_components().ok()},"buffers":{"frames":self.log.metrics()["audio_buffer_frames"],"milliseconds":self.log.metrics()["audio_buffer_ms"]}})
    }
    fn snapshot(&self) -> Value {
        json!({"status":self.status(),"health":self.log.health(),"metrics":self.log.metrics(),"recent_errors":self.log.events(20,None,Some(Level::Warn),None),"resource_usage":fs::read_to_string("/proc/self/status").unwrap_or_default(),"kernel_events":kernel_events()})
    }
    fn play_control(&mut self, a: PlaybackAction) -> Result<(), String> {
        self.dirty = true;
        match a {
            PlaybackAction::Play(id) => {
                let index = self
                    .tracks
                    .iter()
                    .position(|t| t.id == id)
                    .ok_or("track id not in library")?;
                self.effect(Effect::Play(index))
            }
            PlaybackAction::Pause => {
                self.pause();
                Ok(())
            }
            PlaybackAction::Resume => self.load(),
            PlaybackAction::Stop => {
                self.model.invalidate();
                self.playback.stop(self.model.generation);
                self.model.playback = PlaybackState::Stopped;
                self.model.position_ms = 0;
                self.checkpoint();
                Ok(())
            }
            PlaybackAction::Next => self.effect(Effect::Next),
            PlaybackAction::Previous => self.effect(Effect::Previous),
            PlaybackAction::Seek(ms) => {
                self.model.position_ms = ms.min(
                    self.model
                        .current()
                        .map(|t| t.duration_ms.saturating_sub(1))
                        .unwrap_or(0),
                );
                if self.model.playback == PlaybackState::Playing {
                    self.load()
                } else {
                    Ok(())
                }
            }
            PlaybackAction::Wired => self.switch(AudioOutput::Wired),
            PlaybackAction::Bluetooth(s) => self.switch(AudioOutput::Bluetooth(s)),
            PlaybackAction::Volume(v) => {
                self.model.settings.volume = v.min(100);
                if matches!(
                    self.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ) {
                    self.load()?;
                }
                Ok(())
            }
        }
    }
}
fn kernel_events() -> Vec<String> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let Ok(mut f) = fs::OpenOptions::new()
        .read(true)
        .custom_flags(0o4000)
        .open("/dev/kmsg")
    else {
        return vec![];
    };
    let mut events = std::collections::VecDeque::new();
    for _ in 0..2048 {
        let mut buf = [0; 4096];
        let Ok(n) = f.read(&mut buf) else { break };
        let line = String::from_utf8_lossy(&buf[..n]);
        let l = line.to_lowercase();
        if [
            "lima", "drm", "gpu", "alsa", "snd_", "mmc", "ext4", "suspend",
        ]
        .iter()
        .any(|s| l.contains(s))
            && ["error", "fault", "hang", "timeout", "corrupt"]
                .iter()
                .any(|s| l.contains(s))
            && !l.contains("calibration")
        {
            events.push_back(line.chars().take(512).collect());
            if events.len() > 32 {
                events.pop_front();
            }
        }
    }
    events.into()
}
fn secure_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = fs::symlink_metadata(path) {
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(format!(
                "persistent path is not a real directory: {}",
                path.display()
            ));
        }
    }
    fs::create_dir_all(path).map_err(|e| e.to_string())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}
fn option(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
fn run() -> Result<(), String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|s| s == "--version") {
        let _ = writeln!(std::io::stdout(), "{}", reborn_core::VERSION);
        return Ok(());
    }
    if args.iter().any(|s| s == "--help") {
        let _ = writeln!(
            std::io::stdout(),
            "reborn [--headless --data-dir DIR --socket PATH --music-dir DIR --fixtures DIR]"
        );
        return Ok(());
    }
    let headless = args.iter().any(|s| s == "--headless");
    if !headless && !storage::data_ready() {
        return Err("Y2DATA is not ready; refusing mutable data on Y2ROOT".into());
    }
    let root = PathBuf::from(option(&args, "--data-dir").unwrap_or("/data/reborn".into()));
    if !headless && root != Path::new("/data/reborn") {
        return Err("production data directory must be /data/reborn".into());
    }
    for p in [
        root.clone(),
        root.join("state"),
        root.join("cache"),
        root.join("logs"),
        root.join("diagnostics"),
    ] {
        secure_dir(&p)?
    }
    let log = Observer::new(&root.join("logs")).map_err(|e| e.to_string())?;
    log.install_panic_hook(reborn_core::VERSION);
    reborn_platform::install_signals();
    reborn_media::initialize_logging(log.clone());
    reborn_audio::initialize_logging(log.clone());
    log.emit(Level::Info,"startup","starting","Reborn Baseline 01",None,json!({"version":reborn_core::VERSION,"ffmpeg":reborn_media::version(),"headless":headless}));
    let mut model = AppModel::restore(&root.join("state/session.json")).unwrap_or_default();
    if let Some(m) = option(&args, "--music-dir") {
        model.settings.music_directory = m.into();
    }
    if !headless && !model.settings.music_directory.starts_with("/data/") {
        return Err("internal music must live under Y2DATA".into());
    }
    fs::create_dir_all(&model.settings.music_directory).map_err(|e| e.to_string())?;
    if !headless && storage::sd_present() {
        let _ = storage::mount_sd();
    }
    model.sources = if headless {
        vec![Source {
            id: "internal".into(),
            kind: reborn_core::MediaSource::Internal,
            root: model.settings.music_directory.clone(),
            online: true,
            mount: "host-test".into(),
        }]
    } else {
        storage::sources(&model.settings.music_directory)
    };
    let db = Database::spawn(root.join("library.db"), log.clone())?;
    db.sources(model.sources.clone())?;
    let scanner = Scanner::spawn(db.clone(), log.clone())?;
    scanner.scan(model.sources.clone())?;
    let graphics = if headless {
        log.health_set(
            "graphics",
            HealthState::Unavailable,
            true,
            "explicit headless mode",
        );
        None
    } else {
        match Renderer::open(&reborn_ui::font_atlas(), log.clone()) {
            Ok(g) => Some(g),
            Err(e) => {
                log.health_set("graphics", HealthState::Failed, true, &e);
                log.emit(
                    Level::Error,
                    "graphics",
                    "startup_failed",
                    &e,
                    None,
                    json!({}),
                );
                None
            }
        }
    };
    let inputs = if headless {
        None
    } else {
        Some(input::Input::open())
    };
    log.health_set(
        "input",
        if inputs.as_ref().is_some_and(|i| i.count() > 0) {
            HealthState::Ok
        } else {
            HealthState::Unavailable
        },
        true,
        "evdev mapping",
    );
    log.health_set("storage", HealthState::Ok, true, "Y2DATA ready");
    log.health_set(
        "audio",
        if reborn_audio::wired_device().is_ok() {
            HealthState::Ok
        } else {
            HealthState::Unavailable
        },
        true,
        "ALSA card discovery",
    );
    let playback = playback::Playback::spawn(log.clone(), root.join("cache"))?;
    let wifi = if headless {
        None
    } else {
        Some(wifi::Wifi::spawn(log.clone()).map_err(|e| e.to_string())?)
    };
    let bluetooth = if headless {
        None
    } else {
        Some(bluetooth::Bluetooth::spawn(log.clone()).map_err(|e| e.to_string())?)
    };
    let socket =
        PathBuf::from(option(&args, "--socket").unwrap_or("/run/reborn/control.sock".into()));
    let server = reborn_control::Server::spawn(&socket, log.clone()).map_err(|e| e.to_string())?;
    let diagnostics = diagnostics::Diagnostics::spawn(
        log.clone(),
        db.clone(),
        PathBuf::from(option(&args, "--fixtures").unwrap_or("/usr/share/reborn/fixtures".into())),
        root.join("diagnostics"),
    )
    .map_err(|e| e.to_string())?;
    let mut rt = Runtime {
        model,
        ui: Ui::default(),
        tracks: vec![],
        playback,
        log: log.clone(),
        db,
        scanner,
        wifi,
        bluetooth,
        wifi_state: Default::default(),
        bt_state: Default::default(),
        graphics,
        power: power::status(),
        scan_state: json!({"state":"scanning"}),
        art: false,
        dirty: true,
        root: root.clone(),
        headless,
        last_activity: Instant::now(),
        query: None,
    };
    let last_snapshot = Arc::new(Mutex::new(json!({"starting":true})));
    let watcher = last_snapshot.clone();
    let wl = log.clone();
    let diagnostic_root = root.join("diagnostics");
    thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || {
            let mut last_failed = false;
            loop {
                thread::sleep(Duration::from_secs(1));
                let health = wl.health();
                let failed = health["overall"] == "failed";
                if failed && !last_failed {
                    wl.emit(
                        Level::Error,
                        "diagnostics",
                        "worker_or_subsystem_failed",
                        "Health transitioned to failed",
                        None,
                        json!({"health":health}),
                    );
                    if let Ok(s) = watcher.try_lock() {
                        let _ = wl.diagnostic(&diagnostic_root, s.clone(), true);
                    }
                }
                last_failed = failed;
                if reborn_platform::stop_requested() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let mut inputs = inputs;
    let mut first_frame_presented = false;
    let (mut periodic, mut checkpoint, mut render_time) = (
        Instant::now(),
        Instant::now(),
        Instant::now() - Duration::from_secs(1),
    );
    let mut monitor: Option<(Instant, SyncSender<Response>, u64, Vec<Value>)> = None;
    let mut last_sd = storage::sd_present();
    let mut xrun_bundle = 0.;
    while !reborn_platform::stop_requested() {
        log.heartbeat("ui", 10);
        if let Some(input) = &mut inputs {
            for (device, action) in input.poll() {
                if let Some((_, _, _, events)) = &mut monitor {
                    if events.len() < 512 {
                        events.push(
                            json!({"device":device,"action":action,"mono_ms":log.uptime()*1000}),
                        );
                    }
                }
                log.emit(
                    Level::Debug,
                    "input",
                    "action",
                    "Input action",
                    None,
                    json!({"device":device,"action":action}),
                );
                rt.last_activity = Instant::now();
                if rt.model.screen_off {
                    let _ = rt.effect(Effect::ScreenToggle);
                    continue;
                }
                let effect = rt.ui.action(&mut rt.model, &rt.tracks, action);
                if let Err(e) = rt.effect(effect) {
                    rt.fail("ui", e)
                }
            }
        }
        if monitor.as_ref().is_some_and(|m| Instant::now() > m.0) {
            let (_, reply, id, events) = monitor.take().unwrap();
            let _ = reply.try_send(Response::ok(
                id,
                json!({"events":events,"count":events.len()}),
            ));
        }
        while let Ok(e) = rt.playback.events.try_recv() {
            match e {
                playback::PlaybackEvent::Artwork { generation, bytes } => {
                    if generation == rt.model.generation {
                        if let Some(g) = &mut rt.graphics {
                            if g.artwork(&bytes).is_ok() {
                                rt.art = true;
                                rt.dirty = true;
                            }
                        }
                    }
                }
                playback::PlaybackEvent::Core(event) => {
                    if let Event::Position { generation, ms } = event {
                        if generation == rt.model.generation
                            && ms / 1000 != rt.model.position_ms / 1000
                        {
                            rt.dirty = true
                        }
                        rt.model.apply(Event::Position { generation, ms });
                        continue;
                    }
                    let error = matches!(&event,Event::PlaybackError{generation,..}if *generation==rt.model.generation);
                    rt.model.apply(event);
                    rt.dirty = true;
                    if error {
                        rt.model.invalidate();
                        rt.playback.stop(rt.model.generation);
                        let _ = log.diagnostic(&root.join("diagnostics"), rt.snapshot(), true);
                    }
                }
            }
        }
        if let Ok(result) = rt.scanner.results.try_recv() {
            match result {
                Ok(stats) => {
                    rt.scan_state = json!({"state":"idle","metrics":stats});
                    rt.query = rt
                        .db
                        .list(Filter {
                            limit: 20000,
                            ..Default::default()
                        })
                        .ok();
                    rt.ui.notice =
                        format!("Scan: {} tracks, {} reused", stats.discovered, stats.reused);
                }
                Err(e) => {
                    rt.scan_state = json!({"state":"failed","error":e});
                    log.health_set("scanner", HealthState::Failed, true, &e);
                    rt.fail("scanner", e);
                }
            }
            rt.dirty = true;
        }
        if let Some(rx) = &rt.query {
            if let Ok(value) = rx.try_recv() {
                match value {
                    Ok(t) => rt.tracks = t,
                    Err(e) => rt.fail("database", e),
                }
                rt.query = None;
                rt.dirty = true;
            }
        }
        if let Some(service) = &rt.wifi {
            while let Ok(s) = service.events.try_recv() {
                rt.ui.wifi = reborn_ui::RadioView {
                    available: s.available,
                    powered: s.enabled,
                    scan: s.scan.clone(),
                    error: s.error.clone(),
                    connection: if s.state == "COMPLETED" {
                        format!("Connected: {}", s.ssid)
                    } else if s.state == "STARTING" {
                        "Starting Wi-Fi...".into()
                    } else {
                        String::new()
                    },
                    count: s.networks.len(),
                };
                rt.ui.networks = s
                    .networks
                    .iter()
                    .map(|n| Item {
                        label: format!(
                            "{} {} dBm {}",
                            n.ssid,
                            n.signal,
                            if n.security.contains("WPA") {
                                "secure"
                            } else {
                                "open"
                            }
                        ),
                        key: n.ssid.clone(),
                    })
                    .collect();
                rt.ui.saved_networks = s
                    .saved
                    .iter()
                    .map(|n| Item {
                        label: format!("Saved: {} (Left: forget)", n.ssid),
                        key: format!("saved:{}", n.saved_id.unwrap_or(0)),
                    })
                    .collect();
                rt.wifi_state = s;
                rt.dirty |= rt.model.screen == Screen::Wifi;
            }
        }
        let mut bt_lost = false;
        if let Some(service) = &rt.bluetooth {
            while let Ok(s) = service.events.try_recv() {
                if let AudioOutput::Bluetooth(address) = &rt.model.output {
                    bt_lost = rt
                        .bt_state
                        .devices
                        .iter()
                        .any(|d| &d.address == address && d.connected)
                        && !s
                            .devices
                            .iter()
                            .any(|d| &d.address == address && d.connected);
                }
                rt.ui.bluetooth = reborn_ui::RadioView {
                    available: s.available,
                    powered: s.powered,
                    scan: s.scan.clone(),
                    error: s.error.clone(),
                    connection: if s.discovering {
                        "Bluetooth discovery active".into()
                    } else {
                        String::new()
                    },
                    count: s.devices.len(),
                };
                rt.ui.bluetooth_devices = s
                    .devices
                    .iter()
                    .map(|d| Item {
                        label: format!(
                            "{} {}",
                            d.name,
                            if d.connected {
                                "connected"
                            } else if d.paired {
                                "paired"
                            } else {
                                "available"
                            }
                        ),
                        key: d.path.clone(),
                    })
                    .collect();
                rt.ui.pairing = s.pending.as_ref().map(|p| p.display.clone());
                rt.dirty |= rt.model.screen == Screen::Bluetooth || rt.ui.pairing.is_some();
                rt.bt_state = s;
            }
        }
        if bt_lost {
            rt.pause();
            rt.ui.notice = "Bluetooth disconnected; playback paused".into();
            log.emit(
                Level::Warn,
                "bluetooth",
                "transport_lost",
                "Selected audio peer disconnected",
                None,
                json!({"recovery":"paused"}),
            );
        }
        while let Ok(env) = server.requests.try_recv() {
            let req = env.request;
            let id = req.id;
            let reply = env.reply;
            let result: Result<Value, String> = match req.command.clone() {
                Command::Status => Ok(rt.status()),
                Command::Audio => Ok(
                    json!({"audio":rt.playback.audio_state(),"ffmpeg":reborn_media::runtime_components()?}),
                ),
                Command::Health => Ok(log.health()),
                Command::Metrics => Ok(log.metrics()),
                Command::Snapshot => Ok(rt.snapshot()),
                Command::Events { last } => Ok(json!(log.events(last, None, None, None))),
                Command::Logs {
                    last,
                    subsystem,
                    level,
                    since_ms,
                } => Ok(json!(log.events(
                    last,
                    subsystem.as_deref(),
                    level,
                    since_ms
                ))),
                Command::LogLevel { subsystem, level } => log.level(subsystem.as_deref(), level),
                Command::Tests => Ok(
                    json!({"tests":reborn_control::TESTS.iter().map(|(n,a)|json!({"name":n,"audible":a,"bounded":true})).collect::<Vec<_>>()}),
                ),
                Command::Scan => rt.effect(Effect::Scan).map(|_| json!({"accepted":true})),
                Command::Radio { radio, action } => {
                    use reborn_control::{Radio, RadioAction};
                    // Scan uses exactly the button/UI path, not an independent
                    // diagnostic implementation which could hide UI-worker bugs.
                    let r = match (radio, action) {
                        (Radio::Wifi, RadioAction::Scan) => rt.effect(Effect::WifiScan),
                        (Radio::Bluetooth, RadioAction::Scan) => rt.effect(Effect::BluetoothScan),
                        (Radio::Wifi, RadioAction::On | RadioAction::Off) => rt
                            .wifi
                            .as_ref()
                            .ok_or("Wi-Fi service unavailable".to_string())
                            .and_then(|w| {
                                w.commands
                                    .try_send(wifi::Command::Power(matches!(
                                        action,
                                        RadioAction::On
                                    )))
                                    .map_err(|_| "Wi-Fi busy".into())
                            }),
                        (Radio::Bluetooth, RadioAction::On | RadioAction::Off) => rt
                            .bluetooth
                            .as_ref()
                            .ok_or("Bluetooth service unavailable".to_string())
                            .and_then(|w| {
                                w.commands
                                    .try_send(bluetooth::Command::Power(matches!(
                                        action,
                                        RadioAction::On
                                    )))
                                    .map_err(|_| "Bluetooth busy".into())
                            }),
                    };
                    r.map(|_| json!({"accepted":true,"radio":radio,"action":action}))
                }
                Command::Playback { action } => rt.play_control(action).map(|_| rt.status()),
                Command::InputMonitor { seconds } => {
                    if monitor.is_some() {
                        Err("input monitor busy".into())
                    } else {
                        monitor = Some((
                            Instant::now() + Duration::from_secs(seconds),
                            reply,
                            id,
                            vec![],
                        ));
                        continue;
                    }
                }
                Command::Diagnose | Command::Test { .. } => {
                    let graphics = if matches!(
                        req.command,
                        Command::Test {
                            name: reborn_control::Test::Baseline | reborn_control::Test::Graphics,
                            ..
                        }
                    ) {
                        match &mut rt.graphics {
                            Some(g) => match g.test() {
                                Ok(()) => {
                                    json!({"passed":true,"renderer":g.info,"offscreen_readback":true})
                                }
                                Err(e) => json!({"passed":false,"error":e}),
                            },
                            None => json!({"passed":false,"error":"graphics unavailable"}),
                        }
                    } else {
                        json!({})
                    };
                    let job = diagnostics::Job {
                        request: req,
                        reply: reply.clone(),
                        snapshot: rt.snapshot(),
                        sources: rt.model.sources.clone(),
                        graphics,
                        output: rt.model.output.clone(),
                        output_rate: rt.output_rate(),
                        audio_busy: matches!(
                            rt.model.playback,
                            PlaybackState::Playing | PlaybackState::Buffering
                        ),
                    };
                    if diagnostics.sender.try_send(job).is_err() {
                        Err("diagnostic worker busy".into())
                    } else {
                        continue;
                    }
                }
            };
            let response = match result {
                Ok(v) => Response::ok(id, v),
                Err(e) => Response::error(id, e),
            };
            let _ = reply.try_send(response);
        }
        if periodic.elapsed() > Duration::from_secs(2) {
            periodic = Instant::now();
            let next_power = power::status();
            if next_power["supplies"] != rt.power["supplies"] {
                log.emit(
                    Level::Debug,
                    "power",
                    "supply_changed",
                    "Power supply state changed",
                    None,
                    next_power.clone(),
                );
            }
            rt.power = next_power;
            if !headless {
                let sd = storage::sd_present();
                if sd && !last_sd {
                    let _ = storage::mount_sd();
                }
                if !sd && last_sd {
                    rt.pause();
                    let _ = storage::unmount_sd();
                }
                last_sd = sd;
                let sources = storage::sources(&rt.model.settings.music_directory);
                if sources != rt.model.sources {
                    let old = rt.model.sources.clone();
                    for s in &sources {
                        if !old.iter().any(|v| v.id == s.id) {
                            log.add("sd_insertions", 1.);
                            log.emit(
                                Level::Info,
                                "storage",
                                "source_inserted",
                                "Media source mounted",
                                None,
                                json!({"source":s}),
                            );
                        }
                    }
                    for s in &old {
                        if !sources.iter().any(|v| v.id == s.id) {
                            log.add("sd_removals", 1.);
                            log.emit(
                                Level::Info,
                                "storage",
                                "source_removed",
                                "Media source offline",
                                None,
                                json!({"source_id":s.id}),
                            );
                        }
                    }
                    let g = rt.model.generation;
                    rt.model.apply(Event::SourceChanged(sources.clone()));
                    if g != rt.model.generation {
                        rt.playback.stop(rt.model.generation)
                    }
                    let _ = rt.db.sources(sources);
                    let _ = rt.scanner.scan(rt.model.sources.clone());
                    rt.dirty = true;
                }
            }
            if !headless
                && !rt.model.screen_off
                && rt.last_activity.elapsed().as_secs()
                    > rt.model.settings.screen_timeout_seconds as u64
            {
                let _ = rt.effect(Effect::ScreenToggle);
            }
            if let Ok(mut s) = last_snapshot.lock() {
                *s = rt.snapshot();
                log.context(rt.status());
            }
            let xruns = log.metrics()["audio_xruns"].as_f64().unwrap_or(0.);
            if xruns - xrun_bundle >= 3. {
                xrun_bundle = xruns;
                let _ = log.diagnostic(&root.join("diagnostics"), rt.snapshot(), true);
            }
        }
        if checkpoint.elapsed() > Duration::from_secs(15) {
            rt.checkpoint();
            checkpoint = Instant::now();
        }
        if rt.dirty && !rt.model.screen_off && render_time.elapsed() > Duration::from_millis(34) {
            let draw = rt.ui.draw(
                &rt.model,
                &rt.tracks,
                log.health()["overall"].as_str().unwrap_or("unknown"),
                rt.art,
            );
            if let Some(g) = &mut rt.graphics {
                if let Err(e) = g.render(&draw) {
                    rt.fail("graphics", e);
                    rt.graphics = None;
                    // One bounded recreation attempt. Further attempts wait for an explicit wake.
                    if let Ok(g) = Renderer::open(&reborn_ui::font_atlas(), log.clone()) {
                        rt.graphics = Some(g);
                    }
                    let _ = log.diagnostic(&root.join("diagnostics"), rt.snapshot(), true);
                } else if !first_frame_presented {
                    first_frame_presented = true;
                    log.emit(
                        Level::Info,
                        "startup",
                        "ready",
                        "First Reborn frame presented",
                        None,
                        json!({"display_handoff":"explicit KMS presentation"}),
                    );
                    // Early splash evidence lives on the /run mount carried out
                    // of initramfs; retain this bounded record alongside app logs.
                    if let Ok(bytes) = fs::read("/run/reborn-splash/events.jsonl") {
                        if bytes.len() <= 8192 {
                            let _ = reborn_core::atomic_write(
                                &root.join("logs/splash-boot.jsonl"),
                                &bytes,
                            );
                        }
                    }
                }
            }
            rt.dirty = false;
            render_time = Instant::now();
        }
        thread::sleep(Duration::from_millis(15));
    }
    rt.model.invalidate();
    if let Err(e) = rt.playback.shutdown(rt.model.generation) {
        log.emit(
            Level::Error,
            "audio",
            "shutdown_failed",
            &e,
            None,
            json!({}),
        );
    }
    rt.model.playback = PlaybackState::Paused;
    rt.checkpoint();
    rt.scanner.stop();
    rt.db.stop();
    if let Some(w) = rt.wifi {
        let _ = w.commands.try_send(wifi::Command::Stop);
    }
    if let Some(b) = rt.bluetooth {
        let _ = b.commands.try_send(bluetooth::Command::Stop);
    }
    let _ = fs::remove_file(socket);
    log.emit(
        Level::Info,
        "core",
        "shutdown",
        "Reborn stopped",
        None,
        json!({}),
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        let record = json!({"level":"ERROR","subsystem":"startup","event":"fatal","message":error});
        let _ = serde_json::to_writer(std::io::stderr(), &record);
        let _ = std::io::stderr().write_all(b"\n");
        std::process::exit(1)
    }
}
