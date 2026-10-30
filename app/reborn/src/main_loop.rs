//! Startup and the main loop: `run` builds the runtime and drives input, playback, the platform and the screen.
use crate::*;
use std::ops::ControlFlow;

pub(crate) fn run() -> Result<(), String> {
    let process_started = Instant::now();
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let performance = performance::Trace::new(option(&args, "--performance-trace"));
    if args.iter().any(|s| s == "--default-settings") {
        let value = json!({"session_schema":reborn_core::SESSION_SCHEMA_VERSION,
            "settings":reborn_core::Settings::default()});
        writeln!(std::io::stdout(), "{value}").map_err(|e| e.to_string())?;
        return Ok(());
    }
    if args.iter().any(|s| s == "--version") {
        let _ = writeln!(std::io::stdout(), "{}", reborn_core::BUILD_LABEL);
        return Ok(());
    }
    if args.iter().any(|s| s == "--help") {
        let _ = writeln!(
            std::io::stdout(),
            "reborn [--headless --data-dir DIR --socket PATH --music-dir DIR --fixtures DIR] [--performance-trace PATH]"
        );
        return Ok(());
    }
    let headless = args.iter().any(|s| s == "--headless");
    if !headless && reborn_platform::contract::maintenance_pending() {
        return Err("owner maintenance is incomplete; resume through y2-platform reset".into());
    }
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
    log.install_panic_hook(reborn_core::BUILD_LABEL);
    reborn_platform::install_signals();
    reborn_media::initialize_logging(log.clone());
    reborn_audio::initialize_logging(log.clone());
    log.emit(Level::Info,"startup","starting","Reborn Baseline 01",None,json!({"version":reborn_core::BUILD_LABEL,"ffmpeg":reborn_media::version(),"headless":headless,"process_setup_ms":process_started.elapsed().as_millis()}));
    let session_path = root.join("state/session.json");
    let mut model = match AppModel::restore(&session_path) {
        Ok(model) => model,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppModel::default(),
        Err(error) => {
            // Keep the refused file (a newer Reborn may read it again after the
            // update is installed once more) and every setting this one knows.
            let settings = recover_settings(&session_path);
            let kept = session_path.with_file_name(format!(
                "session.json.rejected-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            ));
            let quarantined = fs::rename(&session_path, &kept).is_ok();
            log.emit(
                Level::Warn,
                "core",
                "restore_failed",
                "Saved session was rejected; starting with its readable settings",
                None,
                json!({"path":session_path,"error":error.to_string(),"settings_recovered":settings.is_some(),"kept":quarantined.then_some(&kept)}),
            );
            AppModel {
                settings: settings.unwrap_or_default(),
                ..AppModel::default()
            }
        }
    };
    startup_phase(&log, process_started, "model_restored");
    if !headless {
        // A previous instance may have ended on the dark shutdown frame.
        let _ = power::blank(false);
        model.platform.brightness_available = power::brightness_available();
        if model.platform.brightness_available {
            let _ = power::set_brightness(model.settings.brightness);
        }
        model.platform.battery = power::battery(&power::status());
        // The wired level and DAC filter, before anything plays.
        if let Err(e) =
            reborn_audio::wired_configure(model.settings.hp_level, model.settings.dac_filter)
        {
            log.emit(
                Level::Warn,
                "audio",
                "wired_configure_failed",
                &e,
                None,
                json!({}),
            );
        }
    }
    // Open the display as soon as the process is alive. The first bounded
    // frame below hands KMS to Reborn while storage and library workers start.
    let graphics = if headless {
        log.health_set(
            "graphics",
            HealthState::Unavailable,
            true,
            "explicit headless mode",
        );
        None
    } else {
        match Renderer::open(&reborn_ui::atlases(), log.clone()) {
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
    startup_phase(&log, process_started, "graphics_ready");
    // The player animates (Beta 7.2); previews and tests draw still frames.
    let mut ui = Ui::default();
    ui.ui_motion = true;
    // The early splash keeps the display, showing real startup progress, until
    // the first complete UI frame below is ready. That frame is the hand-off:
    // it is presented under a full boot-screen cover that dissolves at once.
    let first_frame_presented = false;
    let first_interactive_presented = false;
    let initial_library_ready = false;
    let initial_platform_ready = false;
    if let Some(m) = option(&args, "--music-dir") {
        model.settings.music_directory = m.into();
    } else if !headless && storage::platform_manages_media() {
        // The platform decides where internal music lives (Y2Media once mounted, else Y2DATA),
        // whatever an older settings file recorded.
        model.settings.music_directory = storage::internal_music_directory();
    }
    if !headless
        && !model.settings.music_directory.starts_with("/data/")
        && !model
            .settings
            .music_directory
            .starts_with("/media/internal/")
    {
        return Err("internal music must live under Y2DATA or Y2Media".into());
    }
    fs::create_dir_all(&model.settings.music_directory).map_err(|e| e.to_string())?;
    if !headless && storage::sd_present() && !storage::platform_manages_media() {
        let _ = storage::mount_sd();
    }
    model.sources = if headless {
        vec![Source {
            id: "internal".into(),
            kind: reborn_core::MediaSource::Internal,
            root: model.settings.music_directory.clone(),
            online: true,
            mount: "host-test".into(),
            mount_id: None,
        }]
    } else {
        storage::sources(&model.settings.music_directory)
    };
    startup_phase(&log, process_started, "storage_ready");
    let db = Database::spawn(root.join("library.db"), log.clone())?;
    log.emit(
        Level::Info,
        "startup",
        "database_ready",
        "Database open completed",
        None,
        json!({"elapsed_ms":process_started.elapsed().as_millis()}),
    );
    db.sources(model.sources.clone())?;
    let initial_catalog = db.catalog()?;
    let scanner = Scanner::spawn(db.clone(), log.clone())?;
    scanner.scan(model.sources.clone())?;
    startup_phase(&log, process_started, "library_workers_ready");
    let inputs = if headless {
        None
    } else {
        Some(input::InputManager::open())
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
    startup_phase(&log, process_started, "core_services_ready");
    let playback = playback::Playback::spawn(log.clone(), root.join("cache"))?;
    startup_phase(&log, process_started, "audio_ready");
    // Before the radios: Bluetooth's status wakes the UI's wait through it.
    let socket =
        PathBuf::from(option(&args, "--socket").unwrap_or("/run/reborn/control.sock".into()));
    let server = reborn_control::Server::spawn(&socket, log.clone()).map_err(|e| e.to_string())?;
    let wifi = if headless {
        None
    } else {
        Some(wifi::Wifi::spawn(log.clone()).map_err(|e| e.to_string())?)
    };
    let bluetooth = if headless {
        None
    } else {
        let wake = server.notifier().map_err(|e| e.to_string())?;
        Some(bluetooth::Bluetooth::spawn_notified(log.clone(), wake).map_err(|e| e.to_string())?)
    };
    startup_phase(&log, process_started, "radio_workers_ready");
    let diagnostics = diagnostics::Diagnostics::spawn(
        log.clone(),
        db.clone(),
        PathBuf::from(option(&args, "--fixtures").unwrap_or("/usr/share/reborn/fixtures".into())),
        root.join("diagnostics"),
    )
    .map_err(|e| e.to_string())?;
    let background = background::Gate::new(&model.settings.music_directory, &model.sources);
    let mut rt = Runtime {
        queued_platform: None,
        update_idle_since: None,
        zone_lookup_again: false,
        chapter_reader: if headless {
            None
        } else {
            books::ChapterReader::spawn().ok()
        },
        chapters_asked: HashSet::new(),
        shelf_written: Default::default(),
        paused_at: None,
        sleep_fading: false,
        sleep_minutes_shown: None,
        headphones: None,
        jack_pending: None,
        lost_peer: None,
        playback_hints: reborn_platform::workload::Hints::default(),
        interaction_hints: reborn_platform::workload::Hints::default(),
        model,
        ui,
        platform: if headless {
            None
        } else {
            Some(client::Client::spawn().map_err(|e| e.to_string())?)
        },
        platform_refresh_pending: false,
        platform_observed: false,
        announced_update: None,
        announced_setup: false,
        asked_reports: false,
        boot_fade: reborn_ui::BOOT_FADE_FRAMES,
        thumbs: if headless {
            None
        } else {
            Some(artwork::Worker::spawn().map_err(|e| e.to_string())?)
        },
        playback,
        log: log.clone(),
        db,
        scanner,
        wifi,
        bluetooth,
        wifi_state: Default::default(),
        bt_state: Default::default(),
        bt_volume: BtVolume::default(),
        graphics,
        graphics_recovery: graphics_recovery::Recovery::default(),
        woke_at: None,
        power: power::status(),
        kernel_events: Arc::new(Mutex::new(Vec::new())),
        background,
        art: false,
        history_seen: None,
        history_track: None,
        dirty: DirtyState {
            render: true,
            persistence: false,
        },
        root: root.clone(),
        radio_favorites: reborn_platform::radio::load_favorites(&root.join("state/radio.json")),
        radio_current: None,
        radio_sequence: 0,
        radio_replies: std::sync::mpsc::channel(),
        radio_directory: Arc::new(Mutex::new(None)),
        radio_live: None,
        radio_list: vec![],
        scrobbles: reborn_core::scrobble::Tracker::default(),
        scrobbles_unstamped: vec![],
        scrobble_queue: reborn_platform::scrobble::Queue::new(root.join("state/scrobbles.jsonl")),
        clock_trusted: None,
        lastfm: scrobbling::LastFm::load(&root),
        radio_logos: if headless {
            None
        } else {
            LogoFetcher::spawn(root.join("cache"))
        },
        radio_art: None,
        radio_art_pending: None,
        playlist_worker: playlists::Worker::spawn(Some(root.join("cache/playlists.json"))),
        headless,
        last_activity: Instant::now(),
        query: Some(initial_catalog),
        action_depth: 0,
        action_before: None,
        action_dirty_before: None,
        pending_reconfiguration: None,
        held_since: None,
        loaded_dsp: None,
        seek_until: None,
        seek_run: None,
        reconfiguration_attempted: false,
        sink_release_started: false,
        transition_generation: None,
        active_transport_generation: None,
        input_resume: false,
        sleep_prepared: false,
        sleep_prepared_since: None,
        dark_since: None,
        sleep_attempt: None,
        sleep_refusals: 0,
        sleep_refused_for_charger: false,
        sleep_requested_at: None,
        headset_paused: None,
        wifi_idle_off: false,
        wifi_idle_failed: false,
    };
    rt.model.library.shelf = books::load_shelf(&root.join("state/books.json"));
    rt.shelf_written = rt.model.library.shelf.clone();
    rt.ui.radio.favorites = rt
        .radio_favorites
        .iter()
        .map(|s| s.key().to_owned())
        .collect();
    rt.lastfm_view();
    if !rt.model.settings.theme.is_empty() {
        let id = rt.model.settings.theme.clone();
        // A skin on a missing SD card or a refused skin falls back to Reborn's
        // own look for this session; the choice stays for the next start.
        if let Err(e) = rt.apply_theme(&id, false) {
            rt.log.emit(
                Level::Warn,
                "ui",
                "skin_unavailable",
                &e,
                None,
                json!({"skin":id}),
            );
        }
    }
    startup_phase(&log, process_started, "runtime_ready");
    let last_snapshot = Arc::new(Mutex::new(json!({"starting":true})));
    let watcher = last_snapshot.clone();
    let kernel_history = rt.kernel_events.clone();
    let observation_authority = rt.background.authority();
    let (observation_requests, observation_rx) =
        std::sync::mpsc::sync_channel::<background::Request>(1);
    let (observation_tx, observations) = std::sync::mpsc::sync_channel::<Observation>(1);
    let wl = log.clone();
    let diagnostic_root = root.join("diagnostics");
    // Set by the main loop: dark and quiet, the watchdog's pass is longer.
    let dark_quiet = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog_dark = dark_quiet.clone();
    Watchdog {
        wl,
        watcher,
        kernel_history,
        watchdog_dark,
        observation_rx,
        observation_authority,
        observation_tx,
        diagnostic_root,
        headless,
    }
    .spawn()?;
    let inputs = inputs;
    // #28: the headphone jack, when the kernel reports it.
    let jack = if headless {
        None
    } else {
        reborn_platform::jack::Jack::open().ok()
    };
    if let Some(j) = &jack {
        rt.headphones = j.current();
        rt.model.speaker_active = rt.speaker_route();
    }
    // #28: the vibration motor, when the kernel has one (Linux with VIBR).
    let seek_ticked: Option<Instant> = None;
    let motor = if headless {
        None
    } else {
        reborn_platform::haptics::Motor::open()
            .map_err(|e| {
                log.emit(
                    Level::Info,
                    "input",
                    "haptics_unavailable",
                    &e.to_string(),
                    None,
                    json!({}),
                )
            })
            .ok()
    };
    let workload_tick = Instant::now();
    // The last observation request and snapshot (`dark` spaces them out),
    // and whether the snapshot was taken dark and quiet.
    let observed = Instant::now();
    let (snapshot_taken, snapshot_quiet) = (Instant::now(), false);
    let was_dark = rt.model.screen_off;
    let (periodic, checkpoint, render_time) = (
        Instant::now(),
        Instant::now(),
        Instant::now() - Duration::from_secs(1),
    );
    let monitor: Option<(Instant, SyncSender<Response>, u64, Vec<Value>)> = None;
    let action_router = input::ActionRouter::default();
    let input_screen_off = rt.model.screen_off;
    let last_sd = storage::sd_present();
    let usb_refresh = storage::TransferRefresh::default();
    let observation_pending = None;
    let xrun_bundle = 0.;
    let shutdown_intent = None;
    let power_poll = Instant::now() - Duration::from_secs(1);
    let player = if headless {
        None
    } else {
        let wake = server.notifier().map_err(|e| e.to_string())?;
        avrcp::Player::spawn_notified(log.clone(), move || {
            use std::io::Write;
            let _ = (&wake).write(&[1]);
        })
        .ok()
    };
    let player_publish = Instant::now() - Duration::from_secs(1);
    let platform_refresh = Instant::now() - Duration::from_secs(30);
    let usb_poll = Instant::now();
    // Automatic time zone (Beta 7.1): one lookup per Wi-Fi network, in the
    // background; a failed one is tried again after ten minutes.
    let (zone_tx, zone_rx) = std::sync::mpsc::channel::<(String, Result<String, String>)>();
    let zone_found_on: Option<String> = None;
    let zone_tried: Option<Instant> = None;
    let zone_in_flight = false;
    let download_poll = Instant::now();
    let last_platform_screen = rt.model.screen;
    let codec_reconnect = CodecReconnect::default();
    let auto_output = AutoOutput::default();
    let mut main_loop = MainLoop {
        log,
        process_started,
        performance,
        server,
        diagnostics,
        inputs,
        jack,
        motor,
        player,
        observation_requests,
        observations,
        last_snapshot,
        dark_quiet,
        zone_tx,
        zone_rx,
        first_frame_presented,
        first_interactive_presented,
        initial_library_ready,
        initial_platform_ready,
        seek_ticked,
        workload_tick,
        observed,
        snapshot_taken,
        snapshot_quiet,
        was_dark,
        periodic,
        checkpoint,
        render_time,
        monitor,
        action_router,
        input_screen_off,
        last_sd,
        usb_refresh,
        observation_pending,
        xrun_bundle,
        shutdown_intent,
        power_poll,
        player_publish,
        platform_refresh,
        usb_poll,
        zone_found_on,
        zone_tried,
        zone_in_flight,
        download_poll,
        last_platform_screen,
        codec_reconnect,
        auto_output,
    };
    while !reborn_platform::stop_requested() {
        if main_loop.pass(&mut rt).is_break() {
            break;
        }
    }
    let MainLoop {
        log,
        shutdown_intent,
        ..
    } = main_loop;
    let farewell = shut_down(&mut rt, &log, shutdown_intent.as_ref());
    // Keep the DRM device (and its dark frame) open until the process exits;
    // the platform powers the panel down after the acknowledgement.
    let _keep_scanout = farewell;
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

/// The watchdog thread: the kernel's event history, the battery and media observations the main loop
/// asks for (a request ends its wait at once; dark and quiet, a pass is longer), and a diagnostic
/// bundle when health turns failed.
struct Watchdog {
    wl: Observer,
    /// The main loop's last snapshot, for that bundle.
    watcher: Arc<Mutex<Value>>,
    kernel_history: Arc<Mutex<Vec<String>>>,
    watchdog_dark: Arc<std::sync::atomic::AtomicBool>,
    observation_rx: std::sync::mpsc::Receiver<background::Request>,
    observation_authority: background::Authority,
    observation_tx: SyncSender<Observation>,
    diagnostic_root: PathBuf,
    headless: bool,
}

impl Watchdog {
    fn spawn(self) -> Result<(), String> {
        let Watchdog {
            wl,
            watcher,
            kernel_history,
            watchdog_dark,
            observation_rx,
            observation_authority,
            observation_tx,
            diagnostic_root,
            headless,
        } = self;
        thread::Builder::new()
            .name("watchdog".into())
            .spawn(move || {
                let mut last_failed = false;
                let mut kernel_reader = kernel_events::KernelEvents::default();
                loop {
                    if let Some(events) = kernel_reader.poll() {
                        if let Ok(mut history) = kernel_history.lock() {
                            *history = events;
                        }
                    }
                    // A request (the main loop's observation) ends the wait at once.
                    let pass = if watchdog_dark.load(std::sync::atomic::Ordering::Relaxed) {
                        dark::WATCHDOG
                    } else {
                        Duration::from_secs(1)
                    };
                    match observation_rx.recv_timeout(pass) {
                        Ok(request) => {
                            let started = Instant::now();
                            if let Some((power, media)) =
                                observation_authority.run(&request, || {
                                    (
                                        power::status(),
                                        (!headless).then(|| {
                                            (
                                                storage::sd_present(),
                                                storage::sources(&request.music),
                                            )
                                        }),
                                    )
                                })
                            {
                                let _ = observation_tx.try_send(Observation {
                                    request,
                                    power,
                                    media,
                                    elapsed_ms: started.elapsed().as_secs_f64() * 1000.,
                                });
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    let failed = wl.failed();
                    if failed && !last_failed {
                        let health = wl.health();
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
            .map(drop)
            .map_err(|e| e.to_string())
    }
}

/// The display, its last UI frame, the closing label and the transition plan, kept open while the
/// process ends.
type Farewell = (
    Renderer,
    Vec<reborn_graphics::Quad>,
    &'static str,
    reborn_ui::ShutdownPlan,
);

/// Platform shutdown contract: present the transition, save state, close audio and the database, end
/// on a dark frame with the backlight off, then acknowledge. The platform's own deadline bounds all of
/// this; it proceeds without us if Reborn hangs or crashes. The display stays with the caller.
fn shut_down(
    rt: &mut Runtime,
    log: &Observer,
    shutdown_intent: Option<&reborn_core::platform::ShutdownIntent>,
) -> Option<Farewell> {
    let mut farewell = shutdown_intent.and_then(|intent| {
        rt.model.platform.shutting_down = Some(intent.clone());
        rt.model.navigation.modal = None;
        let last = rt.ui.draw(&rt.model, &rt.model.library.tracks, rt.art);
        let closing = reborn_ui::closing_label(intent.restart, intent.low_battery);
        let plan = reborn_ui::shutdown_plan(intent.low_battery);
        rt.graphics.take().map(|g| (g, last, closing, plan))
    });
    // The UI dissolves into the shutdown screen, which says "Saving" while the
    // real save below runs; its status then becomes the closing label.
    if let Some((g, last, closing, plan)) = farewell.as_mut() {
        for frame in 0..plan.close_frame {
            let started = Instant::now();
            let _ = g.render(&reborn_ui::shutdown_frame(last, frame, closing, *plan));
            thread::sleep(Duration::from_millis(34).saturating_sub(started.elapsed()));
        }
    }
    rt.model.invalidate();
    let mut shutdown_ready = true;
    if let Err(e) = rt.playback.shutdown(rt.model.generation) {
        shutdown_ready = false;
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
    rt.pending_reconfiguration = None;
    rt.checkpoint();
    shutdown_ready &= !rt.dirty.persistence;
    rt.scanner.stop();
    if let Err(error) = rt.db.shutdown(Duration::from_secs(2)) {
        shutdown_ready = false;
        log.emit(
            Level::Error,
            "database",
            "shutdown_failed",
            &error,
            None,
            json!({}),
        );
    }
    if let Some((g, last, closing, plan)) = farewell.as_mut() {
        for frame in plan.close_frame..plan.frames {
            let started = Instant::now();
            let _ = g.render(&reborn_ui::shutdown_frame(last, frame, closing, *plan));
            thread::sleep(Duration::from_millis(34).saturating_sub(started.elapsed()));
        }
        // Final dark frame first, then the backlight: the panel never shows a
        // white, stale or console framebuffer while it is lit. One refresh
        // lets the black frame reach the panel before its light goes out.
        let _ = g.render(&reborn_ui::black_frame());
        thread::sleep(Duration::from_millis(34));
        if let Err(error) = power::blank(true) {
            log.emit(
                Level::Warn,
                "power",
                "backlight_off_failed",
                &error,
                None,
                json!({}),
            );
        }
        // The screen is off for good: the DAC need not wait for it.
        hold_dac(log, false);
        log.emit(
            Level::Info,
            "power",
            "shutdown_presented",
            "Shutdown transition presented",
            None,
            json!({"frames":farewell.as_ref().map_or(0, |f| f.3.frames) + 1}),
        );
    }
    if let Some(intent) = shutdown_intent {
        if let Err(error) = power::acknowledge_shutdown(&intent.id, shutdown_ready) {
            log.emit(Level::Warn, "power", "ack_failed", &error, None, json!({}));
        }
    }
    farewell
}

/// What the main loop keeps from one pass to the next besides the runtime: the devices and
/// workers only it reads, its channels, timers and flags.
pub(crate) struct MainLoop {
    log: Observer,
    process_started: Instant,
    performance: performance::Trace,
    server: reborn_control::Server,
    diagnostics: diagnostics::Diagnostics,
    inputs: Option<input::InputManager>,
    jack: Option<reborn_platform::jack::Jack>,
    motor: Option<reborn_platform::haptics::Motor>,
    player: Option<avrcp::Player>,
    observation_requests: SyncSender<background::Request>,
    observations: std::sync::mpsc::Receiver<Observation>,
    last_snapshot: Arc<Mutex<Value>>,
    dark_quiet: Arc<std::sync::atomic::AtomicBool>,
    zone_tx: std::sync::mpsc::Sender<(String, Result<String, String>)>,
    zone_rx: std::sync::mpsc::Receiver<(String, Result<String, String>)>,
    first_frame_presented: bool,
    first_interactive_presented: bool,
    initial_library_ready: bool,
    initial_platform_ready: bool,
    seek_ticked: Option<Instant>,
    workload_tick: Instant,
    observed: Instant,
    snapshot_taken: Instant,
    snapshot_quiet: bool,
    was_dark: bool,
    periodic: Instant,
    checkpoint: Instant,
    render_time: Instant,
    monitor: Option<(Instant, SyncSender<Response>, u64, Vec<Value>)>,
    action_router: input::ActionRouter,
    input_screen_off: bool,
    last_sd: bool,
    usb_refresh: storage::TransferRefresh,
    observation_pending: Option<background::Request>,
    xrun_bundle: f64,
    shutdown_intent: Option<reborn_core::platform::ShutdownIntent>,
    power_poll: Instant,
    player_publish: Instant,
    platform_refresh: Instant,
    usb_poll: Instant,
    zone_found_on: Option<String>,
    zone_tried: Option<Instant>,
    zone_in_flight: bool,
    download_poll: Instant,
    last_platform_screen: Screen,
    codec_reconnect: CodecReconnect,
    auto_output: AutoOutput,
}

impl MainLoop {
    /// One pass of the main loop; `Break` when the platform asked to shut down.
    fn pass(&mut self, rt: &mut Runtime) -> ControlFlow<()> {
        self.workers(rt);
        self.usb_and_time_zone(rt);
        self.platform(rt);
        self.remote_and_shutdown(rt)?;
        self.ui_and_jack(rt);
        self.input(rt);
        self.playback_events(rt);
        self.scan_results(rt);
        self.query_results(rt);
        self.wifi_and_replies(rt);
        self.bluetooth(rt);
        self.transport_epoch(rt);
        self.control_requests(rt);
        self.observations(rt);
        self.periodic(rt);
        self.render(rt);
        self.wait(rt);
        ControlFlow::Continue(())
    }

    /// The control socket's wakeup, the background gate, chapters, the sleep timer and the cover and logo workers.
    fn workers(&mut self, rt: &mut Runtime) {
        // Drain before inspecting any producer queue: a later event keeps the
        // descriptor readable until the next loop and cannot be lost in wait.
        self.server.drain_wake();
        if rt.background.update(
            &rt.model.settings.music_directory,
            &rt.model.sources,
            rt.sleep_prepared,
        ) {
            self.observation_pending = None;
        }
        rt.receive_chapters();
        rt.sleep_timer_tick();
        if let Some(worker) = &mut rt.thumbs {
            // Upload finished thumbnails (a few per pass, so a burst never
            // stalls a frame), then queue what the last frame could not show.
            for _ in 0..3 {
                let Some(reply) = worker.try_reply() else {
                    break;
                };
                if self.performance.enabled() {
                    self.performance.record("artwork", json!({"decode_ms":reply.decode_ms,"cache_hit":reply.cache_hit,"pixels":reply.rgba.as_ref().map(Vec::len)}));
                }
                if reply.full && rt.radio_art == Some(reply.thumb) {
                    // The station's logo as Now Playing's picture and colour.
                    if reply.rgba.is_some() {
                        rt.radio_art_pending = None;
                    }
                    if let (Some(rgba), Some(renderer)) = (reply.rgba, rt.graphics.as_mut()) {
                        rt.ui.accent = reborn_ui::art::accent_from_rgba(&rgba);
                        let mut rounded = rgba;
                        reborn_ui::art::round_corners(&mut rounded, reborn_graphics::ART_SIZE);
                        if renderer.artwork(&rounded).is_ok() {
                            rt.art = true;
                            rt.dirty.mark_render();
                        }
                    }
                    continue;
                }
                if reply.full {
                    // Cover Flow's centre cover at full resolution.
                    if let (Some(mut rgba), Some(renderer)) = (reply.rgba, rt.graphics.as_mut()) {
                        reborn_ui::art::round_corners(&mut rgba, reborn_graphics::ART_SIZE);
                        if renderer.collection_artwork(&rgba).is_ok() {
                            rt.ui.collection_art.set(Some(reply.thumb));
                            rt.dirty.mark_render();
                        }
                    }
                    continue;
                }
                match (reply.rgba, rt.graphics.as_mut()) {
                    (Some(rgba), Some(renderer)) => {
                        if renderer.upload_thumb(reply.thumb, &rgba).is_ok() {
                            rt.dirty.mark_render();
                        }
                    }
                    (None, _) => {
                        worker.record_failure(reply.thumb);
                        // A logo that does not decode is not asked for again.
                        if reply.thumb >> 63 == 1 {
                            if let Some(fetcher) = &mut rt.radio_logos {
                                fetcher.settle(reply.thumb);
                            }
                        }
                    }
                    _ => {}
                }
            }
            let wanted = rt.ui.take_wanted_thumbs();
            if !rt.model.screen_off && !rt.sleep_prepared && rt.graphics.is_some() {
                let tracks = &rt.model.library.tracks;
                let jobs = wanted
                    .into_iter()
                    .filter(|(key, _)| {
                        rt.graphics
                            .as_ref()
                            .is_some_and(|g| !g.thumb_resident(*key))
                    })
                    .filter_map(|(key, index)| {
                        let track = tracks.get(index)?;
                        Some(artwork::Job {
                            thumb: key,
                            art: artwork::Key::new(
                                track,
                                rt.model
                                    .sources
                                    .iter()
                                    .find(|s| s.id == track.source_id)
                                    .and_then(|s| s.mount_id),
                                &rt.model.settings.music_directory,
                            )
                            .into(),
                            cache: rt.root.join("cache"),
                        })
                    })
                    .collect::<Vec<_>>();
                let mut jobs = jobs;
                // Station logos: decode the ones on disk, download the others.
                let cache = rt.root.join("cache");
                if rt.radio_logos.as_mut().is_some_and(|f| f.finished() > 0) {
                    rt.dirty.mark_render();
                }
                for (key, url) in rt.ui.take_wanted_logos() {
                    if rt.graphics.as_ref().is_some_and(|g| g.thumb_resident(key))
                        || rt.radio_logos.as_ref().is_some_and(|f| f.is_settled(key))
                    {
                        continue;
                    }
                    let file = reborn_platform::radio::logo_file(&cache, key);
                    if file.exists() {
                        jobs.push(artwork::Job {
                            thumb: key,
                            art: artwork::Art::Logo(file),
                            cache: cache.clone(),
                        });
                    } else if rt.ui.radio.online {
                        if let Some(fetcher) = &mut rt.radio_logos {
                            fetcher.want(key, &url);
                        }
                    }
                }
                if !jobs.is_empty() {
                    worker.want(jobs);
                }
                // The playing station's logo becomes Now Playing's picture.
                if !rt.art {
                    if let Some((key, url)) = rt
                        .ui
                        .radio
                        .now
                        .as_ref()
                        .and_then(|s| Some((s.logo_key()?, s.favicon.clone())))
                    {
                        let file = reborn_platform::radio::logo_file(&cache, key);
                        if file.exists() {
                            if rt.radio_art_pending != Some(key) {
                                rt.radio_art = Some(key);
                                rt.radio_art_pending = Some(key);
                                worker.want_cover(artwork::Job {
                                    thumb: key,
                                    art: artwork::Art::Logo(file),
                                    cache: cache.clone(),
                                });
                            }
                        } else if rt.ui.radio.online {
                            if let Some(fetcher) = &mut rt.radio_logos {
                                fetcher.want(key, &url);
                            }
                        }
                    }
                }
                if let Some((key, index)) = rt.ui.take_cover_request() {
                    if let Some(track) = tracks.get(index) {
                        worker.want_cover(artwork::Job {
                            thumb: key,
                            art: artwork::Key::new(
                                track,
                                rt.model
                                    .sources
                                    .iter()
                                    .find(|s| s.id == track.source_id)
                                    .and_then(|s| s.mount_id),
                                &rt.model.settings.music_directory,
                            )
                            .into(),
                            cache: rt.root.join("cache"),
                        });
                    }
                }
            }
        }
    }

    /// The USB cable and transfer receipt, and the automatic time zone lookup.
    fn usb_and_time_zone(&mut self, rt: &mut Runtime) {
        // The cable and the transfer receipt, twice a second on every screen
        // (once a second with the screen off): the computer screen appears
        // within half a second instead of with the next Settings refresh.
        let usb_every = Duration::from_millis(if rt.model.screen_off { 1000 } else { 500 });
        if self.usb_poll.elapsed() >= usb_every {
            self.usb_poll = Instant::now();
            let usb = client::usb_now();
            let was = rt.model.platform.snapshot.usb;
            if usb != was && usb != reborn_core::platform::UsbTransfer::Unknown {
                rt.model.platform.snapshot.usb = usb;
                use reborn_core::platform::UsbTransfer::{Copying, Ready};
                // A computer opening the drives lights the screen to say so.
                if rt.model.screen_off && usb == Ready && !matches!(was, Ready | Copying) {
                    if let Err(error) = rt.effect(Effect::ScreenWake) {
                        rt.ui.flash(error);
                    }
                }
                rt.dirty.mark_render();
            }
        }
        if std::mem::take(&mut rt.zone_lookup_again) {
            self.zone_found_on = None;
            self.zone_tried = None;
        }
        if let WifiStatus::Connected(network) = &rt.ui.wifi.status {
            if rt.model.settings.zone_auto()
                && !self.zone_in_flight
                && self.zone_found_on.as_ref() != Some(network)
                && self
                    .zone_tried
                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(600))
            {
                self.zone_in_flight = true;
                self.zone_tried = Some(Instant::now());
                let (tx, network) = (self.zone_tx.clone(), network.clone());
                let _ = thread::Builder::new()
                    .name("time-zone".into())
                    .spawn(move || {
                        let _ = tx.send((network, reborn_platform::geo::zone()));
                    });
            }
        }
        if let Ok((network, result)) = self.zone_rx.try_recv() {
            self.zone_in_flight = false;
            match result {
                Ok(zone) => {
                    self.zone_found_on = Some(network);
                    if rt.model.settings.zone_auto() && zone != rt.model.settings.time_zone {
                        self.log.emit(
                            Level::Info,
                            "clock",
                            "zone_found",
                            "Time zone set automatically",
                            None,
                            json!({"zone": zone}),
                        );
                        rt.model.settings.time_zone = zone;
                        rt.checkpoint();
                        rt.dirty.mark_render();
                    }
                }
                Err(error) => self.log.emit(
                    Level::Warn,
                    "clock",
                    "zone_lookup_failed",
                    "Automatic time zone lookup failed",
                    None,
                    json!({"error": error}),
                ),
            }
        }
    }

    /// The platform snapshot and tasks, and the live update download and report records.
    fn platform(&mut self, rt: &mut Runtime) {
        rt.poll_platform();
        if rt.platform_observed && !self.initial_platform_ready {
            self.initial_platform_ready = true;
            self.log.emit(
                Level::Info,
                "startup",
                "platform_ready",
                "First platform snapshot published",
                None,
                json!({"elapsed_ms":self.process_started.elapsed().as_millis()}),
            );
        }
        rt.ask_about_reports();
        rt.platform_tick();
        // Platform screens observe on demand; one observation after start
        // (20 s in, after the boot work) also on any other screen, so the
        // error report question comes without opening Settings.
        if !rt.model.screen_off
            && !rt.platform_refresh_pending
            && rt.model.platform.busy.is_none()
            && (platform_backed(rt.model.screen)
                || (!rt.platform_observed
                    && self.process_started.elapsed() >= Duration::from_secs(20)))
            && (rt.model.screen != self.last_platform_screen
                || self.platform_refresh.elapsed() >= platform_refresh_interval(rt.model.screen))
            && self.platform_refresh.elapsed() >= Duration::from_secs(2)
        {
            if let Some(worker) = &rt.platform {
                if worker
                    .commands
                    .try_send(reborn_core::PlatformTask::Refresh)
                    .is_ok()
                {
                    rt.platform_refresh_pending = true;
                    self.platform_refresh = Instant::now();
                }
            }
        }
        self.last_platform_screen = rt.model.screen;
        // The download task answers only when it is finished: show its live
        // progress from the platform's RAM file meanwhile (OTA-48).
        if (rt.model.platform.busy == Some(reborn_core::PlatformTask::UpdateStage)
            || rt.model.platform.snapshot.update.phase
                == reborn_core::platform::UpdatePhase::Downloading)
            && !rt.model.screen_off
            && self.download_poll.elapsed() >= Duration::from_millis(200)
        {
            self.download_poll = Instant::now();
            let live = client::download_progress();
            if live != rt.model.platform.download {
                rt.model.platform.download = live;
                rt.dirty.mark_render();
            }
        }
        // A report the owner started answers only when it is done: show the
        // platform's live record meanwhile, like the update download.
        if matches!(
            rt.model.platform.busy,
            Some(reborn_core::PlatformTask::ReportSend | reborn_core::PlatformTask::ReportSave(_))
        ) && !rt.model.screen_off
            && self.download_poll.elapsed() >= Duration::from_millis(200)
        {
            self.download_poll = Instant::now();
            let live = client::report_progress();
            if live != rt.model.platform.report {
                rt.model.platform.report = live;
                rt.dirty.mark_render();
            }
        }
    }

    /// AVRCP requests and the now-playing publish; a platform shutdown intent ends the loop.
    fn remote_and_shutdown(&mut self, rt: &mut Runtime) -> ControlFlow<()> {
        if let Some(player) = &self.player {
            for request in player.actions.try_iter().take(8) {
                if let Some(action) = request.semantic(&rt.model) {
                    let effect = rt.ui.model_action(&mut rt.model, action);
                    if let Err(error) = rt.effect(effect) {
                        rt.fail("avrcp", error);
                    }
                }
            }
            if self.player_publish.elapsed() >= Duration::from_millis(250) {
                player.publish(&rt.model);
                self.player_publish = Instant::now();
            }
        }
        if !rt.headless && self.power_poll.elapsed() >= Duration::from_millis(250) {
            self.power_poll = Instant::now();
            self.shutdown_intent = power::shutdown_intent();
            if self.shutdown_intent.is_some() {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }

    /// The UI's own ticks, the pending reconfiguration and seek, and the headphone jack.
    fn ui_and_jack(&mut self, rt: &mut Runtime) {
        rt.wifi_idle_tick();
        self.log.heartbeat("ui", 10);
        if rt.ui.observe(&rt.model) | rt.ui.expire_notice() | rt.ui.animating() {
            rt.dirty.mark_render();
        }
        // #28: a computer connecting ticks with its message (it lights the
        // screen); never in the dark.
        if rt.ui.take_connect_tick() && !rt.model.screen_off {
            haptic_tick(&mut self.motor, rt.model.settings.haptics, &self.log);
        }
        // A glide continues after the last wheel detent; keep the short
        // interactive CPU lease alive until it rests. The playing wave keeps
        // the frame rate (`gliding`) but not the lease (Beta 7.4).
        if rt.ui.responding() && !rt.model.screen_off {
            rt.interaction_hints.interactive();
        }
        if let Some(effect) = rt.ui.power_hold_complete() {
            if let Err(error) = rt.effect(effect) {
                rt.fail("power", error);
            }
        }
        rt.poll_pending_reconfiguration();
        rt.apply_seek();
        if rt
            .held_since
            .is_some_and(|since| since.elapsed() >= HOLD_LIMIT)
        {
            rt.release_hold();
        }
        if rt.model.screen_off {
            rt.interaction_hints.clear();
        }
        if std::mem::take(&mut rt.input_resume) {
            if let Some(input) = &mut self.inputs {
                input.resumed();
            }
            self.action_router.reset();
        }
        if let Some(plugged) = self
            .jack
            .as_mut()
            .and_then(reborn_platform::jack::Jack::poll)
        {
            rt.jack_pending = Some(plugged);
        }
        // As a user action: the reconfiguration it starts is polled to its
        // end (a bare load() left the new output prepared and never fed:
        // "Starting" for good after headphones went in, test build 157).
        if rt.pending_reconfiguration.is_none() {
            if let Some(plugged) = rt.jack_pending.take() {
                if let Err(e) = rt.with_model_action(|runtime| runtime.jack_changed(plugged)) {
                    rt.fail("audio", e);
                }
            }
        }
    }

    /// Buttons and the wheel, and an input capture the control socket asked for.
    fn input(&mut self, rt: &mut Runtime) {
        if let Some(input) = &mut self.inputs {
            if self.input_screen_off != rt.model.screen_off {
                input.reset_wheel();
                self.input_screen_off = rt.model.screen_off;
            }
            self.action_router.begin_batch();
            for event in input.poll() {
                // The first frame is revealed under the boot cover; input only
                // counts once the finished UI is fully visible.
                if rt.boot_fade > 0 || rt.sleep_prepared {
                    continue;
                }
                if !rt.model.screen_off {
                    rt.interaction_hints.interactive();
                }
                self.log.emit(
                    Level::Debug,
                    "input",
                    "normalized_event",
                    "Normalized physical input",
                    None,
                    json!({"device":event.device,"input":event.input}),
                );
                // Per event: an action earlier in this batch may have opened
                // or closed the keyboard.
                self.action_router.set_keyboard(rt.ui.keyboard.is_some());
                self.action_router.set_keyboard_wheel(
                    rt.ui
                        .keyboard
                        .as_ref()
                        .is_some_and(reborn_ui::keyboard::Keyboard::live),
                );
                self.action_router
                    .set_screen_off_buttons(rt.model.settings.screen_off_buttons);
                let lit = !rt.model.screen_off;
                let seek_run = rt.seek_run;
                let actions = self.action_router.route(&event, !rt.model.screen_off);
                for &action in &actions {
                    let action_started = Instant::now();
                    if self.performance.enabled() {
                        self.performance.record("input", json!({"action":action,"screen":rt.model.screen,"focus":rt.model.navigation.focus,"kernel_mono_us":event.kernel_mono_us}));
                    }

                    self.log.emit(
                        Level::Debug,
                        "input",
                        "action",
                        "Semantic application action",
                        None,
                        json!({"action":action}),
                    );
                    if !rt.model.screen_off || matches!(action, reborn_core::Action::ScreenWake) {
                        rt.last_activity = Instant::now();
                    }
                    let effect = rt.ui.model_action(&mut rt.model, action);
                    if let Err(e) = rt.effect(effect) {
                        rt.fail("ui", e)
                    }
                    self.log.gauge(
                        "ui_action_ms",
                        action_started.elapsed().as_secs_f64() * 1000.,
                    );
                    if self.performance.enabled() {
                        self.performance.record("state", json!({"action":action,"screen":rt.model.screen,"focus":rt.model.navigation.focus,"action_ms":action_started.elapsed().as_secs_f64()*1000.,"playback":rt.model.playback,"volume":rt.model.settings.volume}));
                    }
                }
                // #28: as stock, one tick per button press on a lit screen
                // (not the press that wakes it), none for wheel steps; a held
                // Previous/Next ticks on its seek steps, at most every
                // SEEK_TICK_GAP (steps come every 90 ms: one 60 ms tick each
                // would be a buzz, as the rejected wheel ticks were). The
                // setting is read after the action, so choosing a strength
                // ticks at it.
                let seeked = rt.seek_run != seek_run
                    && self
                        .seek_ticked
                        .is_none_or(|at| at.elapsed() >= SEEK_TICK_GAP);
                if input_ticks(lit, event.input, &actions, seeked) {
                    if seeked {
                        self.seek_ticked = Some(Instant::now());
                    }
                    haptic_tick(&mut self.motor, rt.model.settings.haptics, &self.log);
                }
                if let Some((_, _, _, events)) = &mut self.monitor {
                    if events.len() < 512 {
                        events.push(json!({"device":event.device,"normalized":event.input,
                                "kernel_mono_us":event.kernel_mono_us,"actions":actions,
                                "state_applied_mono_us":input::monotonic_us(),
                                "screen":rt.model.screen,"screen_off":rt.model.screen_off,
                                "focus":rt.model.navigation.focus,"scroll":rt.model.navigation.scroll,
                                "modal_focus":rt.model.navigation.modal_focus,"volume":rt.model.settings.volume,
                                "pending_frame":!actions.is_empty() && !rt.model.screen_off && rt.dirty.render,
                                "present_return_mono_us":null}));
                    }
                }
                if self.input_screen_off != rt.model.screen_off {
                    input.reset_wheel();
                    self.input_screen_off = rt.model.screen_off;
                }
            }
        }
        if self.monitor.as_ref().is_some_and(|m| Instant::now() > m.0) {
            let (_, reply, id, events) = self.monitor.take().unwrap();
            let _ = reply.try_send(Response::ok(
                id,
                json!({"events":events,"count":events.len(),
                        "raw":self.inputs.as_mut().and_then(input::InputManager::finish_capture),
                        "clock":"CLOCK_MONOTONIC","frame_boundary":"renderer present returned"}),
            ));
        }
    }

    /// What the playback worker reports.
    fn playback_events(&mut self, rt: &mut Runtime) {
        while let Ok(e) = rt.playback.events.try_recv() {
            match e {
                playback::PlaybackEvent::Artwork {
                    generation,
                    entry_id,
                    track_id,
                    bytes,
                } => {
                    if artwork_matches_current(&rt.model, generation, entry_id, track_id) {
                        rt.ui.accent = reborn_ui::art::accent_from_rgba(&bytes);
                        if let Some(g) = &mut rt.graphics {
                            let mut rounded = bytes.to_vec();
                            reborn_ui::art::round_corners(&mut rounded, reborn_graphics::ART_SIZE);
                            if g.artwork(&rounded).is_ok() {
                                rt.art = true;
                                rt.dirty.mark_render();
                            }
                        }
                    }
                }
                playback::PlaybackEvent::Core(event) => {
                    if let Event::Position { generation, ms } = event {
                        // A pending seek's target is the position: a late
                        // report of the held stream would undo it (Previous
                        // after 3 s kept playing on, 8.30 device test).
                        if rt.seek_until.is_some() {
                            continue;
                        }
                        if generation == rt.model.generation && ms != rt.model.position_ms {
                            rt.dirty.mark_persistence();
                            if ms / 1000 != rt.model.position_ms / 1000
                                && rt.model.screen == Screen::NowPlaying
                                && !rt.model.screen_off
                            {
                                rt.dirty.mark_render();
                            }
                        }
                        rt.model.apply(Event::Position { generation, ms });
                        continue;
                    }
                    let persists = matches!(
                        &event,
                        Event::TrackBoundary { .. }
                            | Event::PlaybackError { .. }
                            | Event::SourceChanged(_)
                            | Event::BluetoothDisconnected(_)
                    );
                    let error = matches!(&event,Event::PlaybackError{generation,..}if *generation==rt.model.generation);
                    let ended = matches!(
                        &event,
                        Event::TrackEnded { generation } if *generation == rt.model.generation
                    );
                    if matches!(
                        &event,
                        Event::TrackBoundary { generation, .. }
                            if *generation == rt.model.generation
                    ) {
                        rt.art = false;
                        if rt.model.screen == Screen::NowPlaying {
                            rt.model.navigation.modal = None;
                            rt.model.navigation.context_target = None;
                            rt.model.navigation.context_key = None;
                        }
                    }
                    let boundary = matches!(
                        &event,
                        Event::TrackBoundary { generation, .. } if *generation == rt.model.generation
                    );
                    rt.model.apply(event);
                    if boundary {
                        // The decoder knows `UPCOMING` songs ahead: top it up.
                        rt.upcoming_changed();
                    }
                    rt.dirty.mark_render();
                    if persists {
                        rt.dirty.mark_persistence();
                    }
                    if error {
                        rt.model.invalidate();
                        rt.playback.stop(rt.model.generation);
                        let _ =
                            self.log
                                .diagnostic(&rt.root.join("diagnostics"), rt.snapshot(), true);
                    }
                    if ended {
                        if let Err(e) = rt.finish_track() {
                            rt.fail("playback", e);
                        }
                    }
                }
            }
        }
    }

    /// The library scanner's results.
    fn scan_results(&mut self, rt: &mut Runtime) {
        if let Ok(result) = rt.scanner.results.try_recv() {
            // A no-op scan must not rematerialize the resident catalog or
            // invalidate its cached navigation rows. Deletions alone count.
            let refresh_catalog = match &result {
                Ok(stats) => stats.rescanned != 0 || stats.removed != 0,
                Err(_) => true,
            };
            // The playlist files the scan met: Playlists need not walk the
            // storages again.
            if let (Ok(stats), Some(worker)) = (&result, &rt.playlist_worker) {
                if !stats.playlists.is_empty() {
                    worker.send(playlists::Job::Known(stats.playlists.clone()));
                }
            }
            match result {
                Ok(stats) if stats.complete => {
                    rt.model
                        .apply(Event::LibraryScanFinished(reborn_core::ScanSummary {
                            discovered: stats.discovered,
                            reused: stats.reused,
                            elapsed_ms: stats.elapsed_ms,
                            unreadable: stats.unreadable,
                        }));
                    // Files that could not be read are counted on Settings →
                    // Library, not announced after every start.
                    let songs = stats.discovered.saturating_sub(stats.unreadable);
                    rt.ui.flash(match songs {
                        _ if stats.rescanned > 0 && songs > 1 => {
                            format!("{} new songs added", stats.rescanned.min(songs))
                        }
                        0 => "Scan finished. No music found.".to_owned(),
                        1 => "Scan finished. 1 song in your library.".to_owned(),
                        n => format!("Scan finished. {n} songs in your library."),
                    });
                }
                Ok(stats) => {
                    let error = format!(
                        "scan incomplete; retained existing entries ({} traversal failures)",
                        stats.failures
                    );
                    rt.model.apply(Event::LibraryScanFailed(error.clone()));
                    self.log
                        .health_set("scanner", HealthState::Degraded, true, &error);
                    rt.fail("scanner", error);
                }
                Err(e) => {
                    rt.model.apply(Event::LibraryScanFailed(e.clone()));
                    self.log
                        .health_set("scanner", HealthState::Failed, true, &e);
                    rt.fail("scanner", e);
                }
            }
            // An incomplete scan still publishes validated rows to SQLite,
            // while intentionally refusing pruning. Show those usable rows;
            // keep the failed/incomplete scan state visible and truthful.
            if refresh_catalog {
                rt.query = rt.db.catalog().ok();
            }
            rt.dirty.mark_render();
        }
    }

    /// Library query results.
    fn query_results(&mut self, rt: &mut Runtime) {
        if let Some(rx) = &rt.query {
            if let Ok(value) = rx.try_recv() {
                match value {
                    Ok(t) => {
                        if matches!(
                            rt.model.screen,
                            Screen::Tracks
                                | Screen::Album
                                | Screen::Artist
                                | Screen::Folders
                                | Screen::TrackInfo
                        ) {
                            rt.model.navigation.modal = None;
                            rt.model.navigation.context_target = None;
                            rt.model.navigation.context_key = None;
                            if rt.model.screen == Screen::TrackInfo {
                                rt.model.screen = Screen::Tracks;
                                rt.model.navigation.filter.clear();
                            }
                        }
                        rt.model.library.tracks = t;
                        // A restored queue names its songs: their details now.
                        rt.model.rehydrate_queue();
                        // Playlists are read again with the library (files
                        // copied over USB, the heart's Favorites), but not
                        // while a scan walks the same storage: its own
                        // publish when it ends comes next.
                        if !rt.model.library.scanning {
                            if let Err(e) = rt.playlist(reborn_core::PlaylistRequest::Refresh) {
                                rt.fail("playlists", e);
                            }
                        }
                        if !self.initial_library_ready {
                            self.initial_library_ready = true;
                            self.log.emit(Level::Info, "startup", "library_ready", "Initial library published", None, json!({"elapsed_ms":self.process_started.elapsed().as_millis(),"tracks":rt.model.library.tracks.len()}));
                        }
                        rt.ui.invalidate_catalog();
                        if let Some(worker) = &mut rt.thumbs {
                            worker.forget_failures();
                        }
                    }
                    Err(e) => rt.fail("database", e),
                }
                rt.query = None;
                rt.dirty.mark_render();
            }
        }
    }

    /// Wi-Fi events, playlist and radio replies, the radio, scrobbling and history ticks.
    fn wifi_and_replies(&mut self, rt: &mut Runtime) {
        if let Some(service) = &rt.wifi {
            while let Ok(s) = service.events.try_recv() {
                let view = wifi_view(&s);
                if view != rt.ui.wifi {
                    rt.ui.wifi = view;
                    rt.ui.wifi_join_changed();
                    rt.dirty.mark_render();
                }
                rt.wifi_state = s;
            }
        }
        // From the current Wi-Fi view every pass, not only when an event arrives
        // (Beta 4 refused a station right after start while Wi-Fi was up).
        rt.ui.radio.online = matches!(
            rt.ui.wifi.status,
            WifiStatus::Connected(_) | WifiStatus::NoInternet(_)
        );
        let playlist_replies: Vec<playlists::Reply> = rt
            .playlist_worker
            .as_ref()
            .map(|w| w.replies.try_iter().collect())
            .unwrap_or_default();
        for reply in playlist_replies {
            rt.playlist_reply(reply);
        }
        let radio_replies: Vec<RadioReply> = rt.radio_replies.1.try_iter().collect();
        for reply in radio_replies {
            rt.radio_reply(reply);
        }
        rt.radio_tick();
        rt.lost_peer_tick();
        rt.scrobble_tick();
        rt.note_history();
        rt.lastfm_tick();
    }

    /// Bluetooth events, a lost peer, the Bluetooth volume and the automatic output.
    fn bluetooth(&mut self, rt: &mut Runtime) {
        let mut bt_lost = false;
        if let Some(service) = &rt.bluetooth {
            while let Ok(s) = service.events.try_recv() {
                if let Some(address) = self.codec_reconnect.next(
                    &s,
                    rt.model.playback,
                    rt.pending_reconfiguration.is_some(),
                ) {
                    if service
                        .commands
                        .try_send(bluetooth::Command::Codec {
                            address: address.clone(),
                            preference: rt.model.settings.codec_preference,
                        })
                        .is_ok()
                    {
                        self.codec_reconnect.sent(address);
                    }
                }
                // Several snapshots can arrive in one tick (after a wake, or a
                // burst when a peer drops): a later one must not hide an earlier
                // disconnect (reports 61caa81e/297f1032: output stayed Bluetooth
                // and every Play failed until a restart).
                if let AudioOutput::Bluetooth(address) = &rt.model.output {
                    bt_lost |= rt
                        .bt_state
                        .devices
                        .iter()
                        .any(|d| &d.address == address && d.connected)
                        && !s
                            .devices
                            .iter()
                            .any(|d| &d.address == address && d.connected);
                }
                let view = bluetooth_view(&s);
                let facts = s.diagnostic_facts();
                if view != rt.ui.bluetooth || facts != rt.model.platform.bluetooth_facts {
                    rt.ui.bluetooth = view;
                    rt.model.platform.bluetooth_facts = facts;
                    rt.dirty.mark_render();
                }
                let pairing = s.pending.as_ref().map(|p| {
                    let name = s
                        .devices
                        .iter()
                        .find(|d| d.path == p.device)
                        .map(|d| d.name.as_str())
                        .unwrap_or("Bluetooth device");
                    format!(
                        "{name} shows code {}. Pair only if the code matches.",
                        p.display
                    )
                });
                if pairing != rt.ui.pairing {
                    rt.ui.pairing_focus = 1;
                    rt.dirty.mark_render();
                }
                rt.ui.pairing = pairing;
                rt.bt_state = s;
            }
        }
        if bt_lost {
            rt.bluetooth_peer_lost("paused");
        }
        rt.sync_bt_volume();
        if rt.pending_reconfiguration.is_none() {
            if let Some(address) = self.auto_output.next(&rt.bt_state, &rt.model.output) {
                let name = rt
                    .bt_state
                    .devices
                    .iter()
                    .find(|d| d.address == address)
                    .map(|d| d.name.clone())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| "Bluetooth headphones".into());
                // One attempt per connection, also when it fails: no retry loop.
                self.auto_output.routed(address.clone());
                match rt
                    .with_model_action(|runtime| runtime.switch(AudioOutput::Bluetooth(address)))
                {
                    Ok(()) => {
                        rt.ui.flash(format!("Audio switched to {name}"));
                        self.log.emit(
                            Level::Info,
                            "audio",
                            "output_automatic",
                            "Bluetooth audio peer ready; output follows it",
                            None,
                            json!({"reason":"bluetooth_connected"}),
                        );
                    }
                    Err(error) => {
                        self.log.emit(
                            Level::Warn,
                            "audio",
                            "output_automatic_failed",
                            "Automatic switch to Bluetooth failed",
                            None,
                            json!({"error":error}),
                        );
                    }
                }
                rt.dirty.mark_render();
            }
        }
    }

    /// The audio transport's epoch (a sink that changed underneath).
    fn transport_epoch(&mut self, rt: &mut Runtime) {
        match transport_epoch_state(
            &rt.model.output,
            rt.model.playback,
            rt.active_transport_generation,
            &rt.bt_state,
        ) {
            TransportEpochState::Current => {}
            TransportEpochState::Changed => {
                if rt.pending_reconfiguration.is_some() {
                    rt.fail_active_transport(
                        "Bluetooth transport changed while a new sink was opening",
                    );
                    rt.ui
                        .flash("Bluetooth audio transport changed; playback stopped");
                    rt.dirty.mark_render();
                } else if let Err(error) = rt.with_model_action(|runtime| runtime.load()) {
                    rt.ui
                        .flash(format!("Bluetooth audio transport changed: {error}"));
                    rt.dirty.mark_render();
                }
            }
            TransportEpochState::Unavailable => {
                rt.fail_active_transport(
                    "Bluetooth playback PCM disappeared or lost its negotiated identity",
                );
                rt.ui
                    .flash("Bluetooth audio transport changed; playback stopped");
                rt.dirty.mark_render();
            }
        }
    }

    /// Requests on the control socket.
    fn control_requests(&mut self, rt: &mut Runtime) {
        while let Ok(env) = self.server.requests.try_recv() {
            let req = env.request;
            let id = req.id;
            let reply = env.reply;
            if rt.sleep_prepared && !sleep_control_allowed(&req.command) {
                let _ = reply.try_send(Response::error(id, "sleep_operation_in_progress"));
                continue;
            }
            let result: Result<Value, String> = match req.command.clone() {
                Command::Status => Ok(rt.status()),
                Command::SleepPrepare => rt.prepare_sleep(),
                Command::SleepRestore { dark } => rt.restore_sleep(dark),
                Command::Audio => Ok(
                    json!({"audio":rt.playback.audio_state(),"ffmpeg":reborn_media::runtime_components_if_loaded()}),
                ),
                Command::Health => Ok(self.log.health()),
                Command::Metrics => Ok(self.log.metrics()),
                Command::Snapshot => Ok(rt.snapshot()),
                Command::Events { last } => Ok(json!(self.log.events(last, None, None, None))),
                Command::Logs {
                    last,
                    subsystem,
                    level,
                    since_ms,
                } => Ok(json!(self.log.events(
                    last,
                    subsystem.as_deref(),
                    level,
                    since_ms
                ))),
                Command::LogLevel { subsystem, level } => {
                    self.log.level(subsystem.as_deref(), level)
                }
                Command::Tests => Ok(
                    json!({"tests":reborn_control::TESTS.iter().map(|(n,a)|json!({"name":n,"audible":a,"bounded":true})).collect::<Vec<_>>()}),
                ),
                Command::Scan => rt
                    .effect(Effect::ScanLibrary)
                    .map(|_| json!({"accepted":true})),
                Command::BluetoothCodec {
                    address,
                    preference,
                } => {
                    if !codec_change_allowed(
                        rt.model.playback,
                        rt.pending_reconfiguration.is_some(),
                    ) {
                        Err("Pause music before changing codec".into())
                    } else {
                        let result = rt
                            .bluetooth
                            .as_ref()
                            .ok_or("Bluetooth service unavailable".into())
                            .and_then(|worker| {
                                worker
                                    .commands
                                    .try_send(bluetooth::Command::Codec {
                                        address,
                                        preference,
                                    })
                                    .map(|_| json!({"accepted":true,"negotiated":false}))
                                    .map_err(|_| "Bluetooth busy".into())
                            });
                        if result.is_ok() {
                            rt.model.settings.codec_preference = preference;
                            rt.dirty.mark_both();
                        }
                        result
                    }
                }
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
                Command::Jack { plugged } => {
                    // Taken where a kernel report is: the next loop acts on it.
                    if plugged.is_some() {
                        rt.jack_pending = plugged;
                    }
                    Ok(json!({"headphones":rt.headphones,"pending":rt.jack_pending,
                            "speaker_active":rt.model.speaker_active,"speaker":rt.model.settings.speaker,
                            "playback":rt.model.playback,"output":rt.model.output}))
                }
                Command::Action { action } => {
                    // As the router would hand it over (device tests).
                    rt.last_activity = Instant::now();
                    let effect = rt.ui.model_action(&mut rt.model, action);
                    rt.dirty.mark_render();
                    rt.effect(effect).map(|_| {
                            json!({"screen":rt.model.screen,"focus":rt.model.navigation.focus,
                                "modal":rt.model.navigation.modal.is_some(),
                                "modal_focus":rt.model.navigation.modal_focus,
                                "queue_position":rt.model.queue_position,"queue":rt.model.queue.len(),
                                "queued":rt.model.queued_count(),"notice":rt.ui.notice})
                        })
                }
                Command::Theme { id } => {
                    let applied = match id {
                        Some(id) => rt.effect(Effect::SetTheme(id)),
                        None => rt.effect(Effect::ScanThemes),
                    };
                    applied.map(|_| {
                            json!({"theme":rt.model.settings.theme,
                                "active":rt.ui.skin.as_ref().map(|s| json!({"id":s.id,"name":s.name,"layouts":s.layout_names(),"warnings":s.warnings,"atlas":[s.atlas_width,s.atlas_height]})),
                                "notice":rt.ui.notice,
                                "installed":rt.ui.themes.iter().map(|t| json!({"id":t.id,"name":t.name,"author":t.author,"version":t.version,"dir":t.dir,"problem":t.problem})).collect::<Vec<_>>()})
                        })
                }
                Command::InputMonitor { seconds } => {
                    if self.monitor.is_some() {
                        Err("input monitor busy".into())
                    } else {
                        if let Some(input) = &mut self.inputs {
                            input.begin_capture();
                        }
                        self.monitor = Some((
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
                        audio_busy: matches!(
                            rt.model.playback,
                            PlaybackState::Playing | PlaybackState::Buffering
                        ),
                    };
                    if self.diagnostics.sender.try_send(job).is_err() {
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
    }

    /// The workload meter and the watchdog's battery and media observations.
    fn observations(&mut self, rt: &mut Runtime) {
        if self.workload_tick.elapsed() >= Duration::from_secs(1) {
            self.workload_tick = Instant::now();
            rt.playback_hints
                .publish(reborn_platform::workload::playback_class(
                    matches!(
                        rt.model.playback,
                        PlaybackState::Playing | PlaybackState::Buffering
                    ),
                    rt.model.settings.playback_heavy(),
                    false, // The scanner owns its independent scoped lease.
                ));
        }
        if rt.background.update(
            &rt.model.settings.music_directory,
            &rt.model.sources,
            rt.sleep_prepared,
        ) {
            self.observation_pending = None;
        }
        if let Ok(observation) = self.observations.try_recv().and_then(|o| {
            if rt.background.accepts(&o.request) {
                Ok(o)
            } else {
                Err(std::sync::mpsc::TryRecvError::Empty)
            }
        }) {
            self.observation_pending = None;
            if self.performance.enabled() {
                self.performance.record(
                    "background_observation",
                    json!({"elapsed_ms":observation.elapsed_ms}),
                );
            }
            let next_power = observation.power;
            if next_power["supplies"] != rt.power["supplies"] {
                self.log.emit(
                    Level::Debug,
                    "power",
                    "supply_changed",
                    "Power supply state changed",
                    None,
                    next_power.clone(),
                );
            }
            rt.present_battery(power::battery(&next_power));
            rt.power = next_power;
            if let Some((sd, sources)) = observation
                .media
                .filter(|_| observation.request.music == rt.model.settings.music_directory)
            {
                if sd && !self.last_sd && !storage::platform_manages_media() {
                    let _ = storage::mount_sd();
                }
                if !sd && self.last_sd {
                    rt.pause();
                    if !storage::platform_manages_media() {
                        let _ = storage::unmount_sd();
                    }
                }
                self.last_sd = sd;
                if sources != rt.model.sources {
                    rt.abandon_pending_reconfiguration();
                    let old = rt.model.sources.clone();
                    for s in &sources {
                        if !old.iter().any(|v| v.id == s.id) {
                            self.log.add("sd_insertions", 1.);
                            self.log.emit(
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
                            if matches!(s.kind, reborn_core::MediaSource::SdCard(_)) {
                                rt.ui.flash("SD card removed");
                            }
                            self.log.add("sd_removals", 1.);
                            self.log.emit(
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
                    rt.ui.invalidate_catalog();
                    if let Some(worker) = &mut rt.thumbs {
                        worker.forget_failures();
                    }
                    if g != rt.model.generation {
                        rt.playback.stop(rt.model.generation)
                    }
                    let _ = rt.db.sources(sources);
                    // Publish cached metadata with the new online flags now;
                    // scanner completion reloads only actual row changes.
                    rt.query = rt.db.catalog().ok();
                    rt.model.apply(Event::LibraryScanStarted);
                    if let Err(error) = rt.scanner.scan(rt.model.sources.clone()) {
                        rt.model.apply(Event::LibraryScanFailed(error.clone()));
                        rt.fail("scanner", error);
                    }
                    rt.dirty.mark_render();
                    rt.dirty.mark_persistence();
                }
                let (generation, copying) = storage::usb_changes();
                let now = Instant::now();
                self.usb_refresh.observe(generation, copying, now);
                if self.usb_refresh.ready(
                    rt.model.library.scanning,
                    !rt.model.sources.is_empty(),
                    rt.sleep_prepared,
                    now,
                ) && rt.scanner.scan(rt.model.sources.clone()).is_ok()
                {
                    self.usb_refresh.requested();
                    rt.model.apply(Event::LibraryScanStarted);
                    rt.dirty.mark_render();
                }
            }
        }
        // The screen just lit: battery and media are observed now, not
        // with the next periodic block (they were read seldom in the dark).
        let woke = self.was_dark && !rt.model.screen_off;
        self.was_dark = rt.model.screen_off;
        if woke && self.observation_pending.is_none() {
            rt.background.refresh_pending(&mut self.observation_pending);
            if request_observation(
                &rt.background,
                &self.observation_requests,
                &mut self.observation_pending,
            ) {
                self.observed = Instant::now();
            }
        }
    }

    /// The periodic block and the session checkpoint.
    fn periodic(&mut self, rt: &mut Runtime) {
        if self.periodic.elapsed() > PERIODIC_EVERY {
            rt.loud_listening_tick(self.periodic.elapsed());
            self.periodic = Instant::now();
            rt.automatic_sleep_tick();
            let quiet = rt.dark_and_quiet();
            if rt.background.update(
                &rt.model.settings.music_directory,
                &rt.model.sources,
                rt.sleep_prepared,
            ) {
                self.observation_pending = None;
            }
            rt.background.refresh_pending(&mut self.observation_pending);
            if self.observation_pending.is_none()
                && (!quiet || self.observed.elapsed() >= dark::OBSERVATION)
                && request_observation(
                    &rt.background,
                    &self.observation_requests,
                    &mut self.observation_pending,
                )
            {
                self.observed = Instant::now();
            }
            if !rt.headless
                && !rt.model.screen_off
                && !matches!(
                    rt.model.platform.busy,
                    Some(
                        reborn_core::PlatformTask::UpdateApply
                            | reborn_core::PlatformTask::UpdateRollback
                    )
                )
                && rt.model.settings.screen_timeout_seconds != 0
                && rt.last_activity.elapsed().as_secs()
                    >= rt.model.settings.screen_timeout_seconds as u64
            {
                let _ = rt.effect(Effect::ScreenSleep);
            }
            // For the watchdog's bundle and the panic record: nothing changes
            // while dark and quiet, so once as the quiet begins, then seldom.
            if !quiet || !self.snapshot_quiet || self.snapshot_taken.elapsed() >= dark::SNAPSHOT {
                if let Ok(mut s) = self.last_snapshot.lock() {
                    let status = rt.status();
                    *s = rt.snapshot_with(status.clone());
                    self.log.context(status);
                }
                (self.snapshot_taken, self.snapshot_quiet) = (Instant::now(), quiet);
            }
            let xruns = self.log.metric("audio_xruns");
            if xruns - self.xrun_bundle >= 3. {
                self.xrun_bundle = xruns;
                let _ = self
                    .log
                    .diagnostic(&rt.root.join("diagnostics"), rt.snapshot(), true);
            }
        }
        if self.checkpoint.elapsed() > CHECKPOINT_EVERY && rt.dirty.checkpoint_due() {
            rt.checkpoint();
            self.checkpoint = Instant::now();
        } else if self.checkpoint.elapsed() > CHECKPOINT_EVERY {
            self.checkpoint = Instant::now();
        }
    }

    /// Drawing and presenting a frame.
    fn render(&mut self, rt: &mut Runtime) {
        if rt.dirty.render
            && !rt.model.screen_off
            && self.render_time.elapsed() >= frame_gap(rt.ui.gliding())
        {
            self.render_time = Instant::now();
            let frame_started = Instant::now();
            let tracks = std::mem::take(&mut rt.model.library.tracks);
            rt.ui.normalize(&mut rt.model, &tracks);
            rt.model.library.tracks = tracks;
            if rt.model.screen == Screen::DiagnosticSection && rt.model.navigation.filter == "audio"
            {
                rt.model.platform.audio_facts = audio_facts(&rt.playback.audio_state(), &rt.model);
            }
            if let Some(g) = &rt.graphics {
                rt.ui.set_resident_thumbs(g.resident_thumbs());
            }
            // The playing wave moves with what is heard (Beta 8.38).
            let heard = (rt.model.playback == PlaybackState::Playing)
                .then(|| rt.playback.heard())
                .flatten();
            rt.ui.set_heard(heard);
            let mut draw = rt.ui.draw(&rt.model, &rt.model.library.tracks, rt.art);
            let layout_ms = frame_started.elapsed().as_secs_f64() * 1000.;
            self.log.gauge("ui_layout_ms", layout_ms);
            if !self.first_frame_presented {
                // The splash's bar completes while this first frame renders.
                reborn_platform::contract::boot_milestone("ready");
            }
            let fading = rt.boot_fade > 0;
            if fading {
                // The first frame is the complete boot screen over the real UI
                // (identical to the splash's last frame); it dissolves at once.
                // A step is only consumed once its frame was presented.
                draw = reborn_ui::boot_transition(
                    draw,
                    f32::from(rt.boot_fade) / f32::from(reborn_ui::BOOT_FADE_FRAMES),
                );
            }
            if rt.graphics.is_none() && fading {
                rt.boot_fade -= 1;
            }
            let mut presentation_failed = false;
            if let Some(g) = &mut rt.graphics {
                if let Err(e) = g.render(&draw) {
                    if !rt.retry_after_wake(&e) {
                        presentation_failed = true;
                        rt.fail("graphics", e);
                        rt.graphics = None;
                        // One bounded recreation attempt. Further attempts wait for an explicit wake.
                        if rt.graphics_recovery.failed() {
                            if let Err(error) = rt.recreate_graphics() {
                                rt.fail("graphics", error);
                            }
                        }
                        let _ =
                            self.log
                                .diagnostic(&rt.root.join("diagnostics"), rt.snapshot(), true);
                    }
                } else {
                    rt.graphics_recovery.presented();
                    if self.performance.enabled() {
                        self.performance.record("frame", json!({"screen":rt.model.screen,"focus":rt.model.navigation.focus,"quads":draw.len(),"layout_ms":layout_ms,"frame_ms":frame_started.elapsed().as_secs_f64()*1000.,"render":g.timings}));
                    }
                    if let Some((_, _, _, events)) = &mut self.monitor {
                        let presented = input::monotonic_us();
                        for event in events
                            .iter_mut()
                            .filter(|event| event["pending_frame"] == true)
                        {
                            event["present_return_mono_us"] = json!(presented);
                            event["pending_frame"] = json!(false);
                        }
                    }
                    if fading {
                        rt.boot_fade -= 1;
                    }
                    if rt.boot_fade == 0 && !self.first_interactive_presented {
                        self.first_interactive_presented = true;
                        self.log.emit(
                            Level::Info,
                            "startup",
                            "interactive",
                            "Boot cover fully revealed; input enabled",
                            None,
                            json!({"elapsed_ms":self.process_started.elapsed().as_millis()}),
                        );
                    }
                    if !self.first_frame_presented {
                        self.first_frame_presented = true;
                        if !rt.headless {
                            // The UI is up with the screen on: the DAC stays
                            // powered until the screen sleeps.
                            if !rt.model.screen_off {
                                hold_dac(&self.log, true);
                            }
                            if let Err(error) = reborn_platform::contract::application_ready() {
                                self.log.emit(
                                    Level::Warn,
                                    "platform",
                                    "readiness",
                                    &error,
                                    None,
                                    json!({}),
                                );
                            }
                        }
                        self.log.emit(
                            Level::Info,
                            "startup",
                            "ready",
                            "First Reborn frame presented",
                            None,
                            json!({"display_handoff":"explicit KMS presentation",
                                "elapsed_ms":self.process_started.elapsed().as_millis()}),
                        );
                        // Early splash evidence lives on the /run mount carried out
                        // of initramfs; retain this bounded record alongside app logs.
                        if let Some(bytes) = reborn_platform::contract::splash_evidence() {
                            let _ = reborn_core::atomic_write(
                                &rt.root.join("logs/splash-boot.jsonl"),
                                &bytes,
                            );
                        }
                    }
                }
            }
            if rt.boot_fade == 0 && !presentation_failed {
                rt.dirty.rendered();
            }
        }
    }

    /// Idle work in the dark, then the wait for the next pass.
    fn wait(&mut self, rt: &mut Runtime) {
        // Screen off and no scan running: a long library's lists are made
        // now, one a pass, not when the screen is woken (Beta 8.39). Input
        // still comes between the pieces.
        let idle = rt.model.screen_off && !rt.model.library.scanning && !rt.sleep_prepared;
        // The playlists are read then too, if nothing read them yet: the
        // prewarm's last piece matches them to the library.
        if idle
            && rt.playlist_worker.is_some()
            && !rt.ui.playlists.loaded
            && !rt.ui.playlists.loading
        {
            if let Err(e) = rt.playlist(reborn_core::PlaylistRequest::Refresh) {
                rt.fail("playlists", e);
            }
        }
        let prewarming = idle && rt.ui.prewarm(&rt.model, &rt.model.library.tracks);
        let interval = if prewarming {
            Duration::from_millis(20)
        } else {
            main_loop_interval(
                rt.model.screen_off,
                rt.dirty.render,
                rt.ui.gliding(),
                matches!(
                    rt.model.playback,
                    PlaybackState::Playing | PlaybackState::Buffering
                ),
                self.render_time.elapsed(),
            )
        };
        let quiet = !prewarming
            && self.monitor.is_none()
            && self.inputs.as_ref().is_some_and(|input| !input.held())
            && rt.dark_and_quiet();
        self.dark_quiet
            .store(quiet, std::sync::atomic::Ordering::Relaxed);
        if let (true, Some(input)) = (quiet, &self.inputs) {
            // Dark and quiet: the next full pass comes with an event or the
            // first timer the loop owes (at most `dark::PASS`); in between,
            // once a second, only what lights the screen or shuts down.
            let now = Instant::now();
            let due = rt
                .dirty
                .checkpoint_due()
                .then(|| self.checkpoint + CHECKPOINT_EVERY);
            let until = now
                + dark::pass_wait(
                    now,
                    rt.dark_timers().chain(due),
                    self.periodic + PERIODIC_EVERY + Duration::from_millis(1),
                );
            let shown = rt.model.platform.snapshot.usb;
            loop {
                let slice = until
                    .saturating_duration_since(Instant::now())
                    .min(dark::TICK);
                // Not the jack: the kernel reads its detector only while the
                // DAC is powered (screen on or playing), never dark and quiet.
                let others = std::iter::once(self.server.wake_fd());
                let ready = input.wait_with(slice, others).unwrap_or_else(|_| {
                    thread::sleep(slice);
                    false
                });
                if ready || Instant::now() >= until || reborn_platform::stop_requested() {
                    break;
                }
                if power::shutdown_intent().is_some() || {
                    let usb = client::usb_now();
                    usb != shown && usb != reborn_core::platform::UsbTransfer::Unknown
                } {
                    break;
                }
            }
        } else if let Some(input) = &self.inputs {
            if input
                .wait_with(interval, std::iter::once(self.server.wake_fd()))
                .is_err()
            {
                thread::sleep(interval);
            }
        } else {
            thread::sleep(interval);
        }
    }
}
