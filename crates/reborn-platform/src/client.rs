//! The one Reborn client of the Y2Linux platform service contract.
//!
//! A single worker thread runs bounded `y2-platform` operations with a fixed
//! argv (no shell). Records are validated against their `/v1` schema and
//! projected into `reborn_core::platform` types here; the UI never receives
//! platform JSON. Observation is on demand: the application asks for a
//! refresh only while a platform-backed screen is visible and never while the
//! display is off.
use reborn_core::{
    platform::{
        DiagnosticSection, Fact, HealthLevel, OperationResult, PlatformInfo, PlatformSnapshot,
        SdCard, StorageState, UpdatePhase, UpdateProblem, UpdateState, UsbTransfer, VolumeSpace,
        VolumeState,
    },
    PlatformTask,
};
use serde_json::{json, Value};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc::{sync_channel, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

const LIMIT: u64 = 1024 * 1024;
const PROGRAM: &str = "/usr/bin/y2-platform";
pub const STATUS_SCHEMA: &str = "org.y2linux.status/v1";
pub const CAPABILITIES_SCHEMA: &str = "org.y2linux.capabilities/v1";
pub const HEALTH_SCHEMA: &str = "org.y2linux.health/v1";

/// A typed reply for one task.
pub enum Reply {
    Snapshot(Box<PlatformSnapshot>),
    Health(HealthLevel, DiagnosticSection),
    Operation(PlatformTask, Result<OperationResult, String>),
    Failed(PlatformTask, String),
}

pub struct Client {
    pub commands: SyncSender<PlatformTask>,
    pub replies: Receiver<Reply>,
}
impl Client {
    pub fn spawn() -> std::io::Result<Self> {
        let (tx, rx) = sync_channel::<PlatformTask>(1);
        let (reply_tx, replies) = sync_channel(1);
        thread::Builder::new()
            .name("platform-client".into())
            .spawn(move || {
                while let Ok(task) = rx.recv() {
                    if reply_tx.send(execute(task)).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            commands: tx,
            replies,
        })
    }
}

fn call(args: &[&str], seconds: u64) -> Result<Value, String> {
    run(PROGRAM, args, Duration::from_secs(seconds))
}

fn run(program: &str, args: &[&str], deadline: Duration) -> Result<Value, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "System service is starting. Try again in a moment.".to_owned())?;
    let stdout = child.stdout.take().ok_or("No response from the system")?;
    let (tx, rx) = sync_channel(1);
    thread::spawn(move || {
        let mut data = Vec::new();
        let result = stdout.take(LIMIT + 1).read_to_end(&mut data).map(|_| data);
        let _ = tx.send(result);
    });
    let started = Instant::now();
    let mut bytes = None;
    loop {
        if bytes.is_none() {
            if let Ok(output) = rx.try_recv() {
                bytes = Some(output.map_err(|_| "Could not read the system response")?);
            }
        }
        if bytes.as_ref().is_some_and(|b| b.len() > LIMIT as usize) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("The system response was too large".into());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|_| "System service status unavailable")?
        {
            let raw = match bytes {
                Some(v) => v,
                None => rx
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| "The system did not answer in time")?
                    .map_err(|_| "Could not read the system response")?,
            };
            if raw.len() > LIMIT as usize {
                return Err("The system response was too large".into());
            }
            let value: Value =
                serde_json::from_slice(&raw).map_err(|_| "The system response was unreadable")?;
            if !value.is_object() {
                return Err("The system response has an unsupported shape".into());
            }
            // Health exit 1 is valid evidence, not a transport failure.
            if !status.success() && value["schema"] != HEALTH_SCHEMA {
                return Err(value["failure"]
                    .as_str()
                    .unwrap_or("operation_failed")
                    .to_owned());
            }
            return Ok(value);
        }
        if started.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("timeout".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn schema(value: Value, expected: &str) -> Result<Value, String> {
    if value["schema"] == expected {
        Ok(value)
    } else {
        Err("incompatible_platform_version".into())
    }
}

fn execute(task: PlatformTask) -> Reply {
    use PlatformTask::*;
    let result = match task {
        Refresh => {
            let status =
                call(&["status", "--reborn", "--pss"], 25).and_then(|v| schema(v, STATUS_SCHEMA));
            let capabilities =
                call(&["capabilities"], 10).and_then(|v| schema(v, CAPABILITIES_SCHEMA));
            return match (status, capabilities) {
                (Ok(status), Ok(capabilities)) => {
                    Reply::Snapshot(Box::new(snapshot(&status, &capabilities)))
                }
                (Err(error), _) | (_, Err(error)) => Reply::Failed(task, friendly_error(&error)),
            };
        }
        Health => {
            return match call(&["health"], 30).and_then(|v| schema(v, HEALTH_SCHEMA)) {
                Ok(value) => {
                    let (level, section) = health(&value);
                    Reply::Health(level, section)
                }
                Err(error) => Reply::Failed(task, friendly_error(&error)),
            }
        }
        UpdateCheck => call(&["update", "check"], 100),
        UpdateStage => call(&["update", "stage"], 950),
        UpdateApply => call(&["update", "apply"], 30),
        UpdateCancel => call(&["update", "cancel"], 30),
        UpdateRollback => call(&["update", "rollback"], 30),
        Export => call(&["export-state", "--include-database"], 120),
        StorageBenchmark => call(
            &[
                "bench-storage",
                "--volume",
                "/data",
                "--size-mib",
                "16",
                "--operations",
                "64",
                "--seconds",
                "60",
            ],
            90,
        ),
        LibraryBenchmark => call(
            &["bench-library", "--volume", "/data", "--tracks", "1000"],
            120,
        ),
        // Passive IP/route/DNS readiness only; never pings an external host.
        NetworkCheck => call(&["status", "wifi"], 25).and_then(|v| schema(v, STATUS_SCHEMA)),
    };
    Reply::Operation(
        task,
        result
            .map(|v| operation(&v))
            .map_err(|e| friendly_error(&e)),
    )
}

/// User-language text for a platform failure code. Raw service text is never
/// echoed; Diagnostics keeps the observation itself.
pub fn friendly_error(error: &str) -> String {
    let e = error.to_ascii_lowercase();
    if e.contains("password") || e.contains("wrong_key") || e.contains("wrong_credentials") {
        "Check the password and try again."
    } else if e.contains("dns") {
        "Network connected, but internet name lookup isn't working."
    } else if e.contains("dhcp") || e.contains("no_address") {
        "Connected to Wi-Fi, but couldn't get a network address."
    } else if e.contains("space") || e.contains("reserve") {
        "Not enough free storage. Remove some files using a computer."
    } else if e.contains("signature") || e.contains("hash") || e.contains("manifest") {
        "The update couldn't be verified. Nothing was changed."
    } else if e.contains("kernel") || e.contains("compatib") || e.contains("incompatible") {
        "This needs a system update from a computer."
    } else if e.contains("exactly_one") || e.contains("channel") || e.contains("url") {
        "Software updates are installed using a computer."
    } else if e.contains("clock") || e.contains("tls") || e.contains("network") {
        "Connect to Wi-Fi to check for updates."
    } else if e.contains("timeout") {
        "That took too long. Please try again."
    } else if e.contains("starting") || e.contains("unavailable") || e.contains("not_found") {
        "Not ready yet. Try again in a moment."
    } else {
        "Couldn't complete that. Please try again."
    }
    .into()
}

pub fn update_problem(error: &str) -> UpdateProblem {
    let e = error.to_ascii_lowercase();
    if e.contains("exactly_one") || e.contains("channel") || e.contains("local_package") {
        UpdateProblem::NoUpdateSource
    } else if e.contains("clock") || e.contains("tls") || e.contains("http") || e.contains("url") {
        UpdateProblem::NeedsNetwork
    } else if e.contains("space") || e.contains("reserve") {
        UpdateProblem::NotEnoughSpace
    } else if e.contains("signature") || e.contains("hash") || e.contains("manifest") {
        UpdateProblem::VerificationFailed
    } else if e.contains("kernel") || e.contains("compatib") || e.contains("maintenance") {
        UpdateProblem::NeedsComputer
    } else {
        UpdateProblem::Other
    }
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "Yes" } else { "No" }.into()),
        Value::Array(a) if !a.is_empty() => Some(
            a.iter()
                .take(8)
                .filter_map(text)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        _ => None,
    }
}
/// A diagnostic value. Absence is stated plainly; this is Diagnostics, where
/// an engineer wants to know a field was not reported.
fn shown(v: &Value) -> String {
    text(v).unwrap_or_else(|| "Not reported".into())
}
fn fact(label: &str, v: &Value) -> Fact {
    Fact::new(label, shown(v))
}
fn entries(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten()
}
fn mib_from_kib(v: &Value) -> String {
    v.as_u64()
        .map(|n| format!("{:.1} MiB", n as f64 / 1024.))
        .unwrap_or_else(|| "Not reported".into())
}
fn bytes(v: &Value) -> String {
    v.as_u64()
        .map(reborn_core::platform::human_bytes)
        .unwrap_or_else(|| "Not reported".into())
}

fn volume(status: &Value, path: &str) -> Option<Value> {
    entries(&status["storage"]["volumes"])
        .find(|v| v["path"] == path)
        .cloned()
}

fn space(v: &Value) -> VolumeSpace {
    let state = match (v["state"].as_str(), v["space_state"].as_str()) {
        (Some("Ready"), Some("Normal")) => VolumeState::Ready,
        (Some("Ready"), Some("LowSpace")) => VolumeState::LowSpace,
        (Some("Ready"), Some("CriticalSpace")) => VolumeState::AlmostFull,
        (_, Some("ReadOnlyRisk")) => VolumeState::ReadOnly,
        (Some("Ready"), _) => VolumeState::Ready,
        (Some("Failed"), _) | (_, Some("Failed")) => VolumeState::Error,
        _ => VolumeState::Unknown,
    };
    VolumeSpace {
        state,
        free_bytes: v["available_bytes"].as_u64(),
        total_bytes: v["total_bytes"].as_u64(),
    }
}

pub fn storage(status: &Value) -> StorageState {
    let internal = volume(status, "/data")
        .map(|v| space(&v))
        .unwrap_or_default();
    let sd = match volume(status, "/media/sd") {
        None => SdCard::Unknown,
        Some(v) if v["state"] == "Ready" => SdCard::Ready(space(&v)),
        Some(v) => match v["reason"].as_str() {
            // The platform's own vocabulary for "no card inserted".
            Some(r)
                if r.contains("absent") || r.contains("no_media") || r.contains("not_present") =>
            {
                SdCard::Absent
            }
            Some(r) if r.contains("not_uniquely_mounted") || r.contains("not_mounted") => {
                SdCard::Absent
            }
            _ if v["state"] == "Unavailable" => SdCard::Absent,
            _ => SdCard::Error,
        },
    };
    StorageState { internal, sd }
}

pub fn usb(status: &Value) -> UsbTransfer {
    let cable = entries(&status["system"]["usb"]["udcs"])
        .filter_map(|u| u["state"].as_str())
        .find(|s| !s.is_empty());
    let attached = match cable {
        None => return UsbTransfer::Unknown,
        Some("not attached") => false,
        Some(_) => true,
    };
    if !attached {
        return UsbTransfer::Disconnected;
    }
    match status["system"]["ssh"]["state"].as_str() {
        Some("Ready") if cable == Some("configured") => UsbTransfer::Ready,
        Some("Failed") => UsbTransfer::Error,
        _ => UsbTransfer::Starting,
    }
}

pub fn update(status: &Value, ota_enabled: bool) -> UpdateState {
    let u = &status["system"]["update"];
    let state = u["state"].as_str().unwrap_or("");
    let download = &u["download"];
    let current = status["system"]["versions"]["release_version"].as_str();
    let terminal = matches!(state, "Idle" | "Acknowledged" | "RolledBack" | "Failed");
    let phase = match state {
        "Queued" => UpdatePhase::ReadyToInstall,
        "PendingHealth" | "RollbackPending" => UpdatePhase::Installing,
        "Failed" => UpdatePhase::Failed(update_problem(u["failure"].as_str().unwrap_or(""))),
        _ => match download["state"].as_str() {
            Some("Staging") => UpdatePhase::Downloading,
            Some("Checked") => match download["release_version"].as_str() {
                Some(v) if Some(v) != current => UpdatePhase::Available {
                    version: v.to_owned(),
                },
                Some(_) => UpdatePhase::UpToDate,
                None => UpdatePhase::Unknown,
            },
            Some("Failed") => {
                UpdatePhase::Failed(update_problem(download["failure"].as_str().unwrap_or("")))
            }
            _ => UpdatePhase::Unknown,
        },
    };
    UpdateState {
        can_check: ota_enabled && terminal,
        can_download: ota_enabled && terminal && matches!(phase, UpdatePhase::Available { .. }),
        can_install: ota_enabled && state == "Queued",
        can_cancel: ota_enabled && state == "Queued",
        can_rollback: ota_enabled
            && u["rollback_allowed"] == true
            && matches!(state, "Acknowledged" | "PendingHealth"),
        phase,
    }
}

pub fn health(value: &Value) -> (HealthLevel, DiagnosticSection) {
    let level = match value["state"].as_str() {
        Some("OK") => HealthLevel::Ok,
        Some("DEGRADED") => HealthLevel::Degraded,
        Some("FAILED") => HealthLevel::Failed,
        _ => HealthLevel::Unknown,
    };
    let mut facts = vec![fact("Overall", &value["state"])];
    for c in entries(&value["checks"]) {
        facts.push(Fact::new(
            shown(&c["name"]),
            format!("{} · {}", shown(&c["state"]), shown(&c["reason"])),
        ));
    }
    (
        level,
        DiagnosticSection {
            id: "health",
            title: "Health",
            facts,
        },
    )
}

fn enabled_capabilities(caps: &Value) -> Vec<String> {
    caps["capabilities"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, c)| c["enabled"] == true)
        .map(|(k, _)| k.clone())
        .collect()
}

/// Project one status/capabilities pair. Pure; covered by host tests.
pub fn snapshot(status: &Value, caps: &Value) -> PlatformSnapshot {
    let enabled = enabled_capabilities(caps);
    let ota = enabled.iter().any(|c| c == "ota");
    PlatformSnapshot {
        observed: true,
        info: PlatformInfo {
            release_version: text(&status["system"]["versions"]["release_version"]),
        },
        storage: storage(status),
        usb: usb(status),
        update: update(status, ota),
        health: HealthLevel::Unknown,
        diagnostics: diagnostics(status, caps),
        enabled,
    }
}

fn diagnostics(s: &Value, caps: &Value) -> Vec<DiagnosticSection> {
    let mut out = vec![];
    // Battery and power source
    let p = &s["power"];
    let mut battery = vec![
        Fact::new(
            "State of charge",
            p["soc_percent"]
                .as_u64()
                .filter(|v| *v <= 100)
                .map(|v| format!("{v}%"))
                .unwrap_or_else(|| "Not reported".into()),
        ),
        fact("SOC source", &p["soc_source"]),
        fact("SOC confidence", &p["soc_confidence"]),
        fact("Calibration", &p["battery"]["calibration_source"]),
        fact("Low-battery policy", &p["low_battery"]["state"]),
        Fact::new(
            "Battery voltage",
            p["low_battery"]["voltage_uv"]
                .as_f64()
                .map(|v| format!("{:.3} V", v / 1e6))
                .unwrap_or_else(|| "Not reported".into()),
        ),
        fact("Measured current (µA)", &p["measured_current_ua"]),
        fact("Pack temperature (m°C)", &p["pack_temperature"]),
    ];
    for supply in entries(&p["supplies"]) {
        battery.push(Fact::new(
            format!("Supply {}", shown(&supply["name"])),
            format!(
                "{} · {}",
                shown(&supply["type"]),
                text(&supply["status"])
                    .or_else(|| text(&supply["online"]).map(|o| format!("online {o}")))
                    .unwrap_or_else(|| "Not reported".into())
            ),
        ));
    }
    out.push(DiagnosticSection {
        id: "battery",
        title: "Battery",
        facts: battery,
    });
    // Storage
    let mut storage = vec![];
    for (path, label) in [
        ("/data", "Internal"),
        ("/media/sd", "SD card"),
        ("/", "System"),
    ] {
        if let Some(v) = volume(s, path) {
            storage.push(Fact::new(
                label,
                format!("{} · {}", shown(&v["state"]), shown(&v["space_state"])),
            ));
            storage.push(Fact::new(
                format!("{label} free / total"),
                format!(
                    "{} / {}",
                    bytes(&v["available_bytes"]),
                    bytes(&v["total_bytes"])
                ),
            ));
            storage.push(fact(&format!("{label} filesystem"), &v["filesystem"]));
            for key in ["uuid", "device", "mount_generation", "reason"] {
                if !v[key].is_null() {
                    storage.push(fact(&format!("{label} {}", key.replace('_', " ")), &v[key]));
                }
            }
        }
    }
    for c in entries(&s["storage"]["controllers"]) {
        storage.push(Fact::new(
            shown(&c["name"]),
            format!(
                "cap {} Hz · actual {} Hz · errors {} · fallbacks {}",
                shown(&c["cap_hz"]),
                shown(&c["actual_hz"]),
                shown(&c["transport_errors"]),
                shown(&c["fallbacks"])
            ),
        ));
    }
    out.push(DiagnosticSection {
        id: "storage",
        title: "Storage",
        facts: storage,
    });
    // Network
    let w = &s["wifi"];
    out.push(DiagnosticSection {
        id: "network",
        title: "Network",
        facts: vec![
            fact("Readiness", &w["state"]),
            fact("Reason", &w["reason"]),
            fact("IP address", &w["ip_addresses"]),
            Fact::new(
                "Default route",
                entries(&w["default_route"])
                    .take(4)
                    .map(|r| format!("via {} · {}", shown(&r["gateway"]), shown(&r["dev"])))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            fact("DNS ready", &w["dns_ready"]),
            fact("Signal (dBm)", &w["rssi_dbm"]),
            fact("RX bytes", &w["traffic_counters"]["rx_bytes"]),
            fact("TX bytes", &w["traffic_counters"]["tx_bytes"]),
            fact("Power save", &w["power_save"]["enabled"]),
        ],
    });
    // Bluetooth service state (transport facts are added by the application)
    out.push(DiagnosticSection {
        id: "bluetooth",
        title: "Bluetooth",
        facts: vec![
            fact("Trusted peer", &s["bluetooth"]["selected_peer"]["trusted"]),
            fact("Reconnect state", &s["bluetooth"]["reconnect"]["state"]),
            fact(
                "Optional codecs built",
                &caps["capabilities"]["bluetooth"]["optional_codecs"],
            ),
            fact(
                "Auto-eligible codecs",
                &caps["capabilities"]["bluetooth"]["codec_auto"]["eligible_codecs"],
            ),
        ],
    });
    // Audio hardware parameters (pipeline facts are added by the application)
    let mut audio = vec![
        fact(
            "Enabled formats",
            &caps["capabilities"]["audio"]["enabled_formats"],
        ),
        fact(
            "Enabled rates (Hz)",
            &caps["capabilities"]["audio"]["enabled_rates_hz"],
        ),
    ];
    for (path, v) in s["audio"]["hw_params"].as_object().into_iter().flatten() {
        audio.push(Fact::new(path.clone(), shown(v)));
    }
    out.push(DiagnosticSection {
        id: "audio",
        title: "Audio",
        facts: audio,
    });
    // CPU, memory and thermal
    let mut cpu = vec![
        fact("Cores online", &s["cpu"]["online"]),
        fact("Load average", &s["cpu"]["load_average"]),
        Fact::new(
            "Memory available",
            mib_from_kib(&s["memory"]["meminfo"]["MemAvailable"]),
        ),
        Fact::new(
            "Memory total",
            mib_from_kib(&s["memory"]["meminfo"]["MemTotal"]),
        ),
        fact("Clocksource", &s["cpu"]["timer"]["clocksource"]),
        fact("High resolution", &s["cpu"]["timer"]["highres_active"]),
        fact("Tickless idle", &s["cpu"]["timer"]["no_hz_active"]),
    ];
    for c in entries(&s["cpu"]["policies"]) {
        cpu.push(Fact::new(
            format!("CPU {}", shown(&c["affected_cpus"])),
            format!(
                "{} MHz · {}",
                c["scaling_cur_freq"]
                    .as_str()
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(|k| (k / 1000).to_string())
                    .unwrap_or_else(|| "?".into()),
                shown(&c["scaling_governor"])
            ),
        ));
    }
    for idle in entries(&s["cpu"]["idle"]) {
        cpu.push(Fact::new(
            format!("{} {}", shown(&idle["cpu"]), shown(&idle["name"])),
            format!(
                "{} entries · {} µs",
                shown(&idle["usage"]),
                shown(&idle["time_us"])
            ),
        ));
    }
    for zone in entries(&s["thermal"]["zones"]) {
        cpu.push(Fact::new(
            format!("{} (die)", shown(&zone["type"])),
            zone["temperature_millicelsius"]
                .as_f64()
                .map(|n| format!("{:.1} °C", n / 1000.))
                .unwrap_or_else(|| "Not reported".into()),
        ));
    }
    for process in entries(&s["memory"]["processes"]) {
        cpu.push(Fact::new("Reborn PSS", mib_from_kib(&process["pss_kib"])));
    }
    out.push(DiagnosticSection {
        id: "cpu",
        title: "CPU & Power",
        facts: cpu,
    });
    // USB
    let dma = &s["system"]["usb"]["dma"];
    let mut usb = vec![
        fact("Transfer service", &s["system"]["ssh"]["state"]),
        fact("Address", &s["system"]["ssh"]["bind"]),
        fact("SFTP", &s["system"]["ssh"]["sftp"]),
        fact("Transfer path", &dma["transfer"]),
        fact("DMA errors", &dma["dma_errors"]),
        fact("DMA RX bytes", &dma["dma_rx_programmed_bytes"]),
        fact("DMA TX bytes", &dma["dma_tx_programmed_bytes"]),
    ];
    for udc in entries(&s["system"]["usb"]["udcs"]) {
        usb.insert(0, fact("Cable", &udc["state"]));
    }
    out.push(DiagnosticSection {
        id: "usb",
        title: "USB",
        facts: usb,
    });
    // Update
    let u = &s["system"]["update"];
    out.push(DiagnosticSection {
        id: "update",
        title: "Update",
        facts: vec![
            fact("Journal state", &u["state"]),
            fact("Download", &u["download"]["state"]),
            fact("Offered release", &u["download"]["release_version"]),
            fact("Failure", &u["failure"]),
            fact("Download failure", &u["download"]["failure"]),
            fact("Signing key ID", &u["key_id"]),
            fact("Sequence", &u["sequence"]),
            fact("Rollback allowed", &u["rollback_allowed"]),
            Fact::new("Scope", "System root only; boot image stays manual"),
        ],
    });
    // Boot and services
    let mut boot = vec![
        fact("Boot ID", &s["record"]["boot_id"]),
        fact(
            "Previous boot",
            &s["system"]["boot_history"]["previous_boot_id"],
        ),
        fact(
            "Clean shutdown",
            &s["system"]["previous_boot_evidence"]["previous_orderly_shutdown"],
        ),
        fact("Last stage", &s["system"]["boot_history"]["last_stage"]),
        fact("Reset cause", &s["system"]["reset_cause"]),
        fact("Kernel taint", &s["system"]["kernel_taint"]),
        fact("Clock trusted", &s["system"]["time"]["tls_ready"]),
        fact("Clock source", &s["system"]["time"]["source"]),
    ];
    for (name, v) in s["readiness"].as_object().into_iter().flatten() {
        boot.push(Fact::new(
            name.clone(),
            format!("{} · {}", shown(&v["state"]), shown(&v["reason"])),
        ));
    }
    out.push(DiagnosticSection {
        id: "boot",
        title: "Boot & Services",
        facts: boot,
    });
    // Build information
    let v = &s["system"]["versions"];
    out.push(DiagnosticSection {
        id: "build",
        title: "Build Information",
        facts: vec![
            Fact::new("Reborn", reborn_core::BUILD_LABEL),
            fact("Y2Linux release", &v["release_version"]),
            fact("Build ID", &v["build_id"]),
            fact("Kernel", &s["record"]["kernel"]),
            fact("Root filesystem", &v["rootfs_version"]),
            fact("Reborn source", &v["reborn_source_commit"]),
            fact("Y2Linux source", &v["build_git_commit"]),
        ],
    });
    // Capabilities
    let mut capabilities = vec![];
    for (key, c) in caps["capabilities"].as_object().into_iter().flatten() {
        capabilities.push(Fact::new(
            key.replace('_', " "),
            format!(
                "{} · {}{}",
                if c["implemented"] != true {
                    "not implemented"
                } else if c["enabled"] == true {
                    "enabled"
                } else {
                    "disabled"
                },
                if c["qualified"] == true {
                    "qualified"
                } else {
                    "qualification pending"
                },
                if c["experimental"] == true {
                    " · experimental"
                } else {
                    ""
                }
            ),
        ));
    }
    out.push(DiagnosticSection {
        id: "capabilities",
        title: "Capabilities",
        facts: capabilities,
    });
    out
}

/// Reduce an explicit operation's record to public facts.
fn operation(r: &Value) -> OperationResult {
    let mut facts = vec![];
    for (label, key) in [
        ("State", "state"),
        ("Release", "release_version"),
        ("Archive", "path"),
        ("SHA-256", "sha256"),
    ] {
        if let Some(v) = text(&r[key]) {
            facts.push(Fact::new(label, v));
        }
    }
    if let Some(v) = text(&r["record"]["result"]) {
        facts.push(Fact::new("Result", v));
    }
    for m in entries(&r["measurements"]) {
        facts.push(Fact::new(
            shown(&m["operation"]),
            format!(
                "{} · {} MB/s · p95 {} ms",
                shown(&m["result"]),
                shown(&m["MB_per_second"]),
                shown(&m["p95_ms"])
            ),
        ));
    }
    if !r["wifi"].is_null() {
        facts.push(fact("Wi-Fi readiness", &r["wifi"]["state"]));
        facts.push(fact("IP address", &r["wifi"]["ip_addresses"]));
        facts.push(fact("DNS ready", &r["wifi"]["dns_ready"]));
    }
    OperationResult {
        succeeded: r["record"]["result"] != "FAILED",
        facts,
    }
}

/// A projection with no observation yet; used before the first refresh.
pub fn unobserved() -> PlatformSnapshot {
    let mut s = snapshot(&json!({}), &json!({}));
    s.observed = false;
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Value {
        json!({"schema":STATUS_SCHEMA,
            "storage":{"volumes":[
                {"path":"/data","state":"Ready","space_state":"LowSpace","available_bytes":1_000_000_000_u64,"total_bytes":6_000_000_000_u64,"uuid":"abcd"},
                {"path":"/media/sd","state":"Unavailable","reason":"not_uniquely_mounted"}]},
            "system":{"versions":{"release_version":"1.0"},
                "usb":{"udcs":[{"state":"configured"}]},"ssh":{"state":"Ready","bind":"10.42.0.1:22"},
                "update":{"state":"Idle","download":{"state":"Checked","release_version":"1.1"}}}})
    }
    fn caps() -> Value {
        json!({"schema":CAPABILITIES_SCHEMA,"capabilities":{"ota":{"implemented":true,"enabled":true},"usb_host":{"implemented":false}}})
    }

    #[test]
    fn typed_projection_presents_states_not_platform_fields() {
        let s = snapshot(&status(), &caps());
        assert!(s.observed);
        assert_eq!(s.storage.internal.state, VolumeState::LowSpace);
        assert_eq!(s.storage.internal.free_bytes, Some(1_000_000_000));
        assert_eq!(s.storage.sd, SdCard::Absent);
        assert_eq!(s.usb, UsbTransfer::Ready);
        assert_eq!(
            s.update.phase,
            UpdatePhase::Available {
                version: "1.1".into()
            }
        );
        assert!(s.update.can_download && s.update.can_check && !s.update.can_install);
        assert!(s.enabled("ota") && !s.enabled("usb_host"));
        // Engineering details live only in Diagnostics.
        let usb = s.section("usb").unwrap();
        assert!(usb.facts.iter().any(|f| f.value.contains("10.42.0.1")));
    }

    #[test]
    fn optional_absent_and_unknown_fields_stay_unknown_not_invented() {
        let s = snapshot(&json!({"schema":STATUS_SCHEMA}), &json!({}));
        assert_eq!(s.storage.internal.state, VolumeState::Unknown);
        assert_eq!(s.storage.sd, SdCard::Unknown);
        assert_eq!(s.usb, UsbTransfer::Unknown);
        assert_eq!(s.update.phase, UpdatePhase::Unknown);
        assert!(!s.update.can_check, "no capability, no action");
        assert!(!unobserved().observed);
    }

    #[test]
    fn update_states_and_failures_become_user_states() {
        let mut v = status();
        v["system"]["update"] = json!({"state":"Queued"});
        let u = update(&v, true);
        assert_eq!(u.phase, UpdatePhase::ReadyToInstall);
        assert!(u.can_install && u.can_cancel && !u.can_check);
        v["system"]["update"] =
            json!({"state":"Idle","download":{"state":"Checked","release_version":"1.0"}});
        assert_eq!(update(&v, true).phase, UpdatePhase::UpToDate);
        v["system"]["update"] = json!({"state":"Idle","download":{"state":"Failed","failure":"clock_or_entropy_not_ready_for_TLS"}});
        assert_eq!(
            update(&v, true).phase,
            UpdatePhase::Failed(UpdateProblem::NeedsNetwork)
        );
        assert_eq!(
            update_problem("exactly_one_local_package_or_HTTPS_manifest_required"),
            UpdateProblem::NoUpdateSource
        );
        assert!(!update(&v, false).can_check);
    }

    #[test]
    fn usb_cable_and_service_states() {
        let mut v = status();
        v["system"]["usb"]["udcs"] = json!([{"state":"not attached"}]);
        assert_eq!(usb(&v), UsbTransfer::Disconnected);
        v["system"]["usb"]["udcs"] = json!([{"state":"configured"}]);
        v["system"]["ssh"]["state"] = json!("Starting");
        assert_eq!(usb(&v), UsbTransfer::Starting);
        v["system"]["ssh"]["state"] = json!("Failed");
        assert_eq!(usb(&v), UsbTransfer::Error);
    }

    #[test]
    fn malformed_future_and_oversize_records_fail_closed() {
        assert!(schema(json!({"schema":"v2"}), STATUS_SCHEMA).is_err());
        assert!(run("/bin/sh", &["-c", "printf '[]'"], Duration::from_secs(1)).is_err());
        assert!(run(
            "/bin/sh",
            &["-c", "head -c 1048577 /dev/zero"],
            Duration::from_secs(1)
        )
        .is_err());
    }

    #[test]
    fn failed_health_is_observation_and_commands_have_deadlines() {
        let value = run(
            "/bin/sh",
            &[
                "-c",
                "printf '{\"schema\":\"org.y2linux.health/v1\",\"state\":\"FAILED\"}'; exit 1",
            ],
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(health(&value).0, HealthLevel::Failed);
        assert!(run("/bin/sleep", &["2"], Duration::from_millis(40)).is_err());
    }

    #[test]
    fn errors_are_user_language_and_never_echo_service_text() {
        assert!(friendly_error("wrong_key").contains("password"));
        assert!(friendly_error("dhcp_timeout").contains("network address"));
        assert!(friendly_error("dns_unavailable").contains("name lookup"));
        let raw = friendly_error("org.bluez.Error.Failed private details");
        assert!(!raw.contains("org.bluez") && !raw.contains("private"));
    }
}
