use reborn_audio::{AlsaSink, AudioSink};
use reborn_control::{Command, Request, Response, Test};
use reborn_core::{AudioOutput, Source};
use reborn_library::{Database, Filter};
use reborn_media::{Cancel, Decoder, DspConfig, OutputSpec};
use reborn_observability::{Level, Observer};
use reborn_platform::{bluetooth, free_bytes, input, storage, wifi};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{sync_channel, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};
pub struct Job {
    pub request: Request,
    pub reply: SyncSender<Response>,
    pub snapshot: Value,
    pub sources: Vec<Source>,
    pub graphics: Value,
    pub output: AudioOutput,
    pub output_rate: Result<u32, String>,
    pub audio_busy: bool,
}
pub struct Diagnostics {
    pub sender: SyncSender<Job>,
}
impl Diagnostics {
    pub fn spawn(
        log: Observer,
        db: Database,
        fixtures: PathBuf,
        root: PathBuf,
    ) -> std::io::Result<Self> {
        let (tx, rx): (SyncSender<Job>, Receiver<Job>) = sync_channel(2);
        thread::Builder::new()
            .name("diagnostics".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let id = job.request.id;
                    let correlation = log.correlation();
                    log.emit(
                        Level::Info,
                        "diagnostics",
                        "operation_started",
                        "Bounded diagnostic operation",
                        Some(correlation),
                        json!({}),
                    );
                    let result = match job.request.command {
                        Command::Diagnose => log
                            .diagnostic(&root, job.snapshot.clone(), false)
                            .map(|p| json!({"path":p}))
                            .map_err(|e| e.to_string()),
                        Command::Test {
                            name,
                            seconds,
                            saved,
                        } => run(name, seconds, saved, &job, &fixtures, &db, &log),
                        _ => Err("unsupported diagnostic job".into()),
                    };
                    let response = match result {
                        Ok(r) => Response::ok(id, r),
                        Err(e) => {
                            log.emit(
                                Level::Error,
                                "diagnostics",
                                "test_failed",
                                &e,
                                Some(correlation),
                                json!({"recovery_attempted":false}),
                            );
                            let _ = log.diagnostic(&root, job.snapshot, true);
                            Response::error(id, e)
                        }
                    };
                    let _ = job.reply.try_send(response);
                }
            })?;
        Ok(Self { sender: tx })
    }
}
fn decoder(fixtures: &Path, log: &Observer) -> Result<Value, String> {
    let start = Instant::now();
    let mut results = vec![];
    for name in [
        "tone.flac",
        "tone.mp3",
        "tone.m4a",
        "tone.ogg",
        "tone.opus",
        "tone.wav",
        "flac-16-44100.flac",
        "flac-16-48000.flac",
        "flac-24-44100.flac",
        "flac-24-48000.flac",
        "flac-24-88200.flac",
        "flac-24-96000.flac",
        "wav-pcm16.wav",
        "wav-pcm24.wav",
        "wav-pcm32.wav",
        "wav-float32.wav",
        "mp3.mp3",
        "aac.aac",
        "m4a-aac.m4a",
        "m4a-alac.m4a",
        "vorbis.ogg",
        "opus.opus",
        "aiff-pcm24.aiff",
        "ape-silence.ape",
        "wavpack-silence.wv",
    ] {
        let time = Instant::now();
        let mut d = Decoder::open(&fixtures.join(name), 48000, Cancel::new()?)?;
        let mut frames = 0u64;
        let mut packets = 0;
        let mut blocks = 0;
        let mut peak = 0i32;
        while let Some(p) = d.read()? {
            packets = p.packets;
            frames += p.frames;
            peak = peak.max(i32::from(p.data.iter().any(|&sample| sample != 0)));
            blocks += 1;
            if blocks > 1000 || time.elapsed() > Duration::from_secs(10) {
                return Err(format!("fixture bound exceeded: {name}"));
            }
        }
        let expected_duration = name.starts_with("tone.");
        let passed = if expected_duration {
            (45000..55000).contains(&frames) && peak > 0
        } else {
            frames > 0
        };
        results.push(json!({"fixture":name,"passed":passed,"frames":frames,"packets":packets,"blocks":blocks,"peak_nonzero":peak != 0,"errors":0,"elapsed_ms":time.elapsed().as_millis()}));
        if !passed {
            return Err(format!("fixture decoded unexpected PCM: {name}"));
        }
    }
    for name in ["artwork-png.flac", "artwork-jpeg.flac", "artwork-webp.flac"] {
        let mut art = Decoder::open(&fixtures.join(name), 48000, Cancel::new()?)?;
        let rgba = art.artwork()?;
        if rgba.len() != 160 * 160 * 4 || !rgba.chunks_exact(4).any(|p| p[3] != 0) {
            return Err(format!("artwork fixture pixel mismatch: {name}"));
        }
    }
    for name in [
        "artwork-external.jpg",
        "artwork-external.png",
        "artwork-external.webp",
    ] {
        let rgba = reborn_media::external_artwork(&fixtures.join(name))?;
        if rgba.len() != 160 * 160 * 4 || !rgba.chunks_exact(4).any(|p| p[3] != 0) {
            return Err(format!("external artwork fixture pixel mismatch: {name}"));
        }
    }
    for name in [
        "corrupt-metadata.mp3",
        "truncated-flac.flac",
        "truncated-wav.wav",
    ] {
        if let Ok(mut decoder) = Decoder::open(&fixtures.join(name), 48000, Cancel::new()?) {
            for _ in 0..128 {
                match decoder.read() {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        }
    }
    log.emit(
        Level::Info,
        "media",
        "fixture_suite",
        "Decoder fixtures completed",
        None,
        json!({"formats":results.len(),"corruption_cases":3,"artwork_formats":6}),
    );
    Ok(
        json!({"passed":true,"tests":results,"artwork_passed":true,"external_artwork_passed":true,"corruption_cases_passed":true,"elapsed_ms":start.elapsed().as_millis()}),
    )
}
fn audio(output: AudioOutput, rate: u32, fixtures: &Path, log: &Observer) -> Result<Value, String> {
    let before = log.metrics()["audio_xruns"].as_f64().unwrap_or(0.);
    let now = Instant::now();
    let (spec, planned) = AlsaSink::plan(&output, rate, log.clone(), log.correlation())?;
    let mut d = Decoder::open_with(
        &fixtures.join("tone.wav"),
        OutputSpec {
            rate: spec.rate,
            format: spec.format,
        },
        DspConfig {
            volume: 25,
            ..Default::default()
        },
        Cancel::new()?,
    )?;
    let mut sink = AlsaSink::open_spec(&spec, log.clone(), log.correlation())?;
    let params = sink.parameters();
    let mut frames = 0;
    while let Some(p) = d.read()? {
        let mut offset = 0;
        while offset < p.data.len() {
            let n = sink.write(&p.data[offset..])?;
            offset += n * p.format.bytes_per_frame();
            frames += n;
            if now.elapsed() > Duration::from_secs(5) {
                return Err("audio diagnostic timeout".into());
            }
        }
    }
    while sink.delay() > 0 && now.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(10));
    }
    let xruns = log.metrics()["audio_xruns"].as_f64().unwrap_or(0.) - before;
    Ok(
        json!({"passed":xruns==0.,"planned":planned,"params":params,"frames_written":frames,"bytes_written":frames as usize*params.format.bytes_per_frame(),"xruns":xruns,"elapsed_ms":now.elapsed().as_millis()}),
    )
}
fn storage_test(sources: &[Source]) -> Value {
    json!({"passed":sources.iter().any(|s|matches!(s.kind,reborn_core::MediaSource::Internal)&&s.online&&s.root.is_dir()),"sources":sources.iter().map(|s|json!({"id":s.id,"online":s.online,"mount":s.mount,"readable":std::fs::read_dir(&s.root).is_ok(),"free_bytes":free_bytes(&s.root).ok()})).collect::<Vec<_>>(),"sd_present":storage::sd_present()})
}
fn library(db: &Database, sources: &[Source]) -> Result<Value, String> {
    let rows = db
        .list(Filter {
            limit: 64,
            ..Default::default()
        })?
        .recv_timeout(Duration::from_secs(10))
        .map_err(|e| e.to_string())??;
    let missing = rows
        .iter()
        .filter(|t| {
            t.online && sources.iter().any(|s| s.id == t.source_id && s.online) && !t.path.is_file()
        })
        .count();
    Ok(
        json!({"passed":missing==0,"sample":rows.len(),"missing_online":missing,"offline":rows.iter().filter(|t|!t.online).count()}),
    )
}
fn database(db: &Database) -> Result<Value, String> {
    db.test()?
        .recv_timeout(Duration::from_secs(10))
        .map_err(|e| e.to_string())?
}
fn run(
    name: Test,
    seconds: Option<u64>,
    saved: Option<u32>,
    job: &Job,
    fixtures: &Path,
    db: &Database,
    log: &Observer,
) -> Result<Value, String> {
    match name {
        Test::Decoder | Test::Playback => decoder(fixtures, log),
        Test::Database => database(db),
        Test::Library => library(db, &job.sources),
        Test::Storage => Ok(storage_test(&job.sources)),
        Test::Graphics => Ok(job.graphics.clone()),
        Test::Input => Ok(
            json!({"passed":!input::devices().is_empty(),"devices":input::devices(),"physical_activation":"requires owner"}),
        ),
        Test::WifiScan => wifi::scan_test(),
        Test::WifiConnect => wifi::saved_test(saved.ok_or("--saved ID is required")?),
        Test::Bluetooth => bluetooth::service_test(),
        Test::BluetoothScan => bluetooth::scan_test(seconds.unwrap_or(10)),
        Test::AudioWired | Test::AudioBluetooth => {
            if job.audio_busy {
                return Err("pause playback before an explicit audio test".into());
            }
            let output = if matches!(name, Test::AudioWired) {
                AudioOutput::Wired
            } else {
                if matches!(job.output, AudioOutput::Wired) {
                    return Err("select a connected Bluetooth audio output first".into());
                }
                job.output.clone()
            };
            let rate = if matches!(output, AudioOutput::Wired) {
                44100
            } else {
                job.output_rate.clone()?
            };
            audio(output, rate, fixtures, log)
        }
        Test::Baseline => {
            let mut tests = vec![];
            tests.push(json!({"name":"health","passed":log.health()["overall"]!="failed","result":log.health()}));
            tests.push(json!({"name":"graphics","passed":job.graphics["passed"]==true,"result":job.graphics}));
            for (n, r) in [
                ("decoder", decoder(fixtures, log)),
                ("database", database(db)),
                ("library", library(db, &job.sources)),
            ] {
                tests.push(match r {
                    Ok(v) => json!({"name":n,"passed":v["passed"]==true,"result":v}),
                    Err(e) => json!({"name":n,"passed":false,"error":e}),
                });
            }
            let s = storage_test(&job.sources);
            tests.push(json!({"name":"storage","passed":s["passed"]==true,"result":s}));
            let wired = reborn_audio::wired_device();
            tests.push(
                json!({"name":"audio-availability","passed":wired.is_ok(),"device":wired.ok()}),
            );
            let bt = bluetooth::service_test();
            tests.push(json!({"name":"bluetooth-service","optional":true,"available":bt.is_ok(),"result":bt.ok()}));
            tests.push(json!({"name":"wifi-service","optional":true,"available":Path::new("/run/wpa_supplicant/global").exists()}));
            tests.push(json!({"name":"input","passed":!input::devices().is_empty()}));
            let passed = tests
                .iter()
                .all(|v| v["optional"] == true || v["passed"] == true);
            Ok(
                json!({"passed":passed,"tests":tests,"test_count":tests.len(),"warnings":if storage::sd_present(){vec![]}else{vec!["SD absent"]}}),
            )
        }
    }
}
