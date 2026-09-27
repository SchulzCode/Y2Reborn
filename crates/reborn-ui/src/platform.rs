//! Human-readable projection of the versioned Platform v1 observations.
//! No I/O, inferred measurements, or hardware policy belongs in this module.
use crate::Item;
use reborn_core::AppModel;
use serde_json::Value;

pub fn value(v: &Value) -> String {
    match v {
        Value::Null => "Unavailable".into(),
        Value::Bool(b) => if *b { "Yes" } else { "No" }.into(),
        Value::String(s) => {
            if s.is_empty() {
                "Unavailable".into()
            } else {
                s.clone()
            }
        }
        Value::Number(n) => n.to_string(),
        Value::Array(a) => a.iter().take(8).map(value).collect::<Vec<_>>().join(", "),
        _ => "Unavailable in this view".into(),
    }
}
fn field(label: &str, v: &Value) -> Item {
    fact(label, value(v))
}
fn fact(label: &str, text: impl Into<String>) -> Item {
    let text = text.into();
    Item::new(label, format!("value:{label}\u{1f}{text}")).with_secondary(text)
}
fn page(label: &str, id: &str, description: &str) -> Item {
    Item::new(label, format!("page:{id}")).with_secondary(description)
}
fn action(label: &str, id: &str, description: &str, enabled: bool) -> Item {
    let mut row = Item::new(label, id).with_secondary(description);
    row.enabled = enabled;
    row
}
fn bytes(v: &Value) -> String {
    v.as_u64()
        .map(|n| {
            if n >= 1_073_741_824 {
                format!("{:.1} GiB", n as f64 / 1_073_741_824.)
            } else {
                format!("{:.1} MiB", n as f64 / 1_048_576.)
            }
        })
        .unwrap_or_else(|| "Unavailable".into())
}
fn kib(v: &Value) -> String {
    v.as_u64()
        .map(|n| format!("{:.1} MiB", n as f64 / 1024.))
        .unwrap_or_else(|| "Unavailable".into())
}
fn entries(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten()
}
fn capability_label(key: &str) -> String {
    match key {
        "telemetry" => "Platform Observations",
        "health" => "Platform Health",
        "storage" => "Storage",
        "wifi" => "Wi-Fi",
        "bluetooth" => "Bluetooth",
        "usb_device" => "USB Device / PC Transfer",
        "usb_host" => "USB Host",
        "audio" => "Wired Audio",
        "power_observation" => "Power Observations",
        "low_battery_shutdown" => "Low-Battery Shutdown",
        "deep_suspend" => "Deep Suspend",
        "cpuidle" => "CPU Idle",
        "system_watchdog" => "System Watchdog",
        "ota" => "Signed Root Update",
        "automatic_bootimg_update" => "Automatic Boot Image Update",
        "shutdown" => "Shutdown / Restart",
        _ => return key.replace('_', " "),
    }
    .into()
}
pub fn enabled(m: &AppModel, key: &str) -> bool {
    m.platform.capabilities["capabilities"][key]["enabled"] == true
}
pub fn title(id: &str) -> &str {
    match id {
        "storage" => "Storage",
        "power" => "Power & Battery",
        "clock" => "Date & Time",
        "about" => "About Reborn",
        "usb" => "PC Transfer",
        "update" => "Software Update",
        "update_diag" => "Update Details",
        "versions" => "Build Information",
        "backup" => "Backup & Export",
        "maintenance" => "Reset & Maintenance",
        "health" => "Platform Health",
        "capabilities" => "Capabilities",
        "cpu" => "CPU & Memory",
        "thermal" => "Thermal & Power",
        "network" => "Network Details",
        "bluetooth" => "Bluetooth Audio",
        "audio" => "Audio Information",
        "boot" => "Boot & Services",
        "codec" => "Codec Preference",
        "benchmarks" => "Advanced Checks",
        "result" => "Operation Result",
        _ => "Diagnostics",
    }
}
pub fn rows(m: &AppModel, id: &str) -> Vec<Item> {
    let p = &m.platform;
    let s = &p.status;
    let busy = p.busy.is_some();
    let mut out = match id {
        "" => vec![
            page("Platform Health", "health", "Subsystem status"),
            page(
                "Latest Operation",
                "result",
                "Results, exports and failures",
            ),
            page(
                "Capabilities",
                "capabilities",
                "Implemented / enabled / qualified",
            ),
            page("CPU & Memory", "cpu", "Frequency, load and Reborn memory"),
            page("Thermal & Power", "thermal", "Die temperatures and cooling"),
            page("Storage", "storage", "Internal storage and SD card"),
            page("Network", "network", "Wi-Fi, IP, route and DNS"),
            page("Bluetooth", "bluetooth", "Peer, codec and negotiated PCM"),
            page("Audio", "audio", "Source, processing and actual sink"),
            page("USB Transfer", "usb", "USB-only SFTP readiness"),
            page("Update", "update_diag", "Signed root update and rollback"),
            page(
                "Boot & Services",
                "boot",
                "Previous boot and service readiness",
            ),
            page(
                "Advanced Checks",
                "benchmarks",
                "Explicit, bounded safe tests",
            ),
        ],
        "health" => {
            let mut r = vec![
                fact("Overall", value(&p.health["state"])),
                action(
                    "Refresh Health",
                    "task:health",
                    "Read-only platform checks",
                    !busy,
                ),
            ];
            for c in entries(&p.health["checks"]) {
                r.push(fact(
                    &value(&c["name"]),
                    format!("{} · {}", value(&c["state"]), value(&c["reason"])),
                ));
            }
            r
        }
        "capabilities" => {
            let mut r = vec![];
            if let Some(caps) = p.capabilities["capabilities"].as_object() {
                for (key, c) in caps {
                    let state = if c["implemented"] != true {
                        "Unavailable"
                    } else if c["enabled"] != true {
                        "Disabled"
                    } else {
                        "Enabled"
                    };
                    let qualification = if c["qualified"] == true {
                        "Qualified"
                    } else {
                        "Qualification pending"
                    };
                    r.push(fact(&capability_label(key), format!("Implemented: {}. Enabled: {}. Physically qualified: {}. Gate: {}. Reason: {}",value(&c["implemented"]),value(&c["enabled"]),value(&c["qualified"]),value(&c["gate"]),value(&c["reason"]))).with_secondary(format!("{state} · {qualification}")));
                }
            }
            for (name, key) in [
                ("Wired S32", "s32"),
                ("Preserved 24-bit", "preserved_24bit"),
            ] {
                r.push(fact(
                    name,
                    if p.capabilities["capabilities"]["audio"][key] == true {
                        "Enabled"
                    } else {
                        "Unavailable"
                    },
                ));
            }
            if let Some(list) =
                p.capabilities["capabilities"]["bluetooth"]["optional_codecs"].as_array()
            {
                for name in ["AAC", "aptX", "aptX HD", "LDAC"] {
                    r.push(fact(
                        name,
                        if list.iter().any(|v| v == name) {
                            "Built; see codec inventory"
                        } else {
                            "Not built"
                        },
                    ));
                }
            }
            r.push(field(
                "CPU idle states",
                &p.capabilities["capabilities"]["cpuidle"]["states"],
            ));
            r.push(field(
                "Wired enabled formats",
                &p.capabilities["capabilities"]["audio"]["enabled_formats"],
            ));
            r.push(field(
                "Wired enabled rates",
                &p.capabilities["capabilities"]["audio"]["enabled_rates_hz"],
            ));
            r
        }
        "cpu" => {
            let mut r = vec![
                field("Cores online", &s["cpu"]["online"]),
                field("Load average", &s["cpu"]["load_average"]),
                fact("Memory total", kib(&s["memory"]["meminfo"]["MemTotal"])),
                fact(
                    "Memory available",
                    kib(&s["memory"]["meminfo"]["MemAvailable"]),
                ),
            ];
            for c in entries(&s["cpu"]["policies"]) {
                let hz = c["scaling_cur_freq"]
                    .as_str()
                    .and_then(|v| v.parse::<f64>().ok());
                r.push(fact(
                    "CPU frequency",
                    hz.map(|n| {
                        format!(
                            "{:.1} MHz · cores {}",
                            n / 1000.,
                            value(&c["affected_cpus"])
                        )
                    })
                    .unwrap_or_else(|| "Unavailable".into()),
                ));
                r.push(field("Governor", &c["scaling_governor"]));
            }
            for process in entries(&s["memory"]["processes"]) {
                r.push(fact("Reborn RSS", kib(&process["rss_kib"])));
                r.push(fact("Reborn PSS", kib(&process["pss_kib"])));
            }
            r.push(fact("Page cache", kib(&s["memory"]["meminfo"]["Cached"])));
            r.push(fact("Slab", kib(&s["memory"]["meminfo"]["Slab"])));
            r
        }
        "thermal" => {
            let mut r = vec![fact(
                "Temperature sensors",
                "CPU / PMIC die; not battery temperature",
            )];
            for zone in entries(&s["thermal"]["zones"]) {
                r.push(fact(
                    &value(&zone["type"]),
                    zone["temperature_millicelsius"]
                        .as_f64()
                        .map(|n| format!("{:.1} °C die", n / 1000.))
                        .unwrap_or_else(|| "Unavailable".into()),
                ));
            }
            for c in entries(&s["thermal"]["cooling"]) {
                r.push(fact(
                    &format!("Cooling {}", value(&c["type"])),
                    format!("{} / {}", value(&c["state"]), value(&c["max_state"])),
                ));
            }
            r.push(page(
                "Battery & Source",
                "power",
                "Voltage and charging observations",
            ));
            r
        }
        "power" => {
            let mut r = vec![fact(
                "Battery level",
                s["power"]["soc_percent"]
                    .as_u64()
                    .filter(|p| *p <= 100)
                    .map(|p| format!("{p}%"))
                    .unwrap_or_else(|| "Percentage unavailable".into()),
            )];
            r.push(field("SOC source", &s["power"]["soc_source"]));
            r.push(field("SOC confidence", &s["power"]["soc_confidence"]));
            r.push(field(
                "Calibration",
                &s["power"]["battery"]["calibration_source"],
            ));
            for supply in entries(&s["power"]["supplies"]) {
                r.push(fact(
                    &value(&supply["name"]),
                    format!(
                        "{} · {}",
                        value(&supply["type"]),
                        if !supply["status"].is_null() {
                            value(&supply["status"])
                        } else {
                            match supply["online"].as_str() {
                                Some("1") => "Source present".into(),
                                Some("0") => "Disconnected".into(),
                                _ => "State unavailable".into(),
                            }
                        }
                    ),
                ));
                let n = supply["voltage_now"]
                    .as_str()
                    .and_then(|n| n.parse::<f64>().ok());
                if let Some(n) = n {
                    r.push(fact(
                        "Battery / supply voltage",
                        format!("{:.3} V", n / 1_000_000.),
                    ));
                }
                if !supply["usb_type"].is_null() {
                    r.push(field("Source", &supply["usb_type"]));
                }
                if !supply["online"].is_null() {
                    r.push(field("Source online", &supply["online"]));
                }
            }
            r.push(field(
                "Low-battery policy",
                &s["power"]["low_battery"]["state"],
            ));
            r.push(field(
                "Measured current (uA)",
                &s["power"]["measured_current_ua"],
            ));
            r.push(field(
                "Pack temperature (mC)",
                &s["power"]["pack_temperature"],
            ));
            r
        }
        "storage" => {
            let mut r = vec![];
            for target in ["/data", "/media/sd", "/"] {
                let label = match target {
                    "/data" => "Internal storage",
                    "/media/sd" => "SD card",
                    _ => "System root",
                };
                if let Some(v) = entries(&s["storage"]["volumes"]).find(|v| v["path"] == target) {
                    let ready = v["state"] == "Ready";
                    r.push(fact(
                        label,
                        if ready {
                            match v["space_state"].as_str() {
                                Some("Normal") => "Available".into(),
                                Some("LowSpace") => {
                                    "Low space · remove files using PC Transfer".into()
                                }
                                Some("CriticalSpace") => {
                                    "Storage nearly full · free space before scanning".into()
                                }
                                _ => value(&v["space_state"]),
                            }
                        } else if target == "/media/sd" {
                            "Not mounted · insert or check the card".into()
                        } else {
                            value(&v["state"])
                        },
                    ));
                    if !ready {
                        continue;
                    }
                    r.push(fact(
                        "Free / total",
                        format!(
                            "{} / {}",
                            bytes(&v["available_bytes"]),
                            bytes(&v["total_bytes"])
                        ),
                    ));
                    r.push(field("Filesystem", &v["filesystem"]));
                    if !v["reason"].is_null() {
                        r.push(field("Storage detail", &v["reason"]));
                    }
                } else {
                    r.push(fact(label, "Not available"));
                }
            }
            r
        }
        "network" => vec![
            field("Wi-Fi readiness", &s["wifi"]["state"]),
            fact(
                "Reason",
                if s["wifi"]["reason"].is_null() && !s["wifi"]["state"].is_null() {
                    "None reported".into()
                } else {
                    value(&s["wifi"]["reason"])
                },
            ),
            field("IP address", &s["wifi"]["ip_addresses"]),
            fact(
                "Default route",
                if let Some(routes) = s["wifi"]["default_route"].as_array() {
                    if routes.is_empty() {
                        "Unavailable".into()
                    } else {
                        routes
                            .iter()
                            .take(4)
                            .map(|r| format!("via {} · {}", value(&r["gateway"]), value(&r["dev"])))
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                } else {
                    value(&s["wifi"]["default_route"])
                },
            ),
            field("DNS ready", &s["wifi"]["dns_ready"]),
            field("Signal (dBm)", &s["wifi"]["rssi_dbm"]),
            field("RX bytes", &s["wifi"]["traffic_counters"]["rx_bytes"]),
            field("TX bytes", &s["wifi"]["traffic_counters"]["tx_bytes"]),
            action(
                "Network Check",
                "task:network",
                "Read-only readiness check",
                !busy,
            ),
        ],
        "bluetooth" => {
            let bt = &p.bluetooth;
            let mut r = vec![
                field("Adapter available", &bt["available"]),
                field("Adapter powered", &bt["powered"]),
                fact("Preference", m.settings.codec_preference.label()),
                page(
                    "Codec Preference",
                    "codec",
                    "Active codec is observed separately",
                ),
                fact(
                    "Auto policy",
                    if p.capabilities["capabilities"]["bluetooth"]["codec_auto"]["eligible_codecs"]
                        .as_array()
                        .is_some_and(|a| !a.is_empty())
                    {
                        "Eligible codecs available"
                    } else {
                        "Unavailable · no qualified eligible codec"
                    },
                ),
            ];
            for dev in
                entries(&bt["devices"]).filter(|d| d["connected"] == true || d["paired"] == true)
            {
                r.push(field("Device", &dev["name"]));
                r.push(field("Paired", &dev["paired"]));
                r.push(field("Connected", &dev["connected"]));
            }
            for pcm in entries(&bt["pcms"]) {
                r.push(field("Active codec", &pcm["codec"]));
                r.push(fact(
                    "PCM format",
                    match pcm["format"].as_u64() {
                        Some(0x8210) => "S16_LE",
                        Some(0x8418) => "S24_LE",
                        Some(0x8420) => "S32_LE",
                        _ => "Unavailable",
                    },
                ));
                r.push(field("PCM rate (Hz)", &pcm["rate"]));
                r.push(field("PCM channels", &pcm["channels"]));
                r.push(field("PCM running", &pcm["running"]));
            }
            r.push(field(
                "Trusted peer",
                &s["bluetooth"]["selected_peer"]["trusted"],
            ));
            r.push(field(
                "Reconnect state",
                &s["bluetooth"]["reconnect"]["state"],
            ));
            if entries(&bt["pcms"]).next().is_none() {
                for label in [
                    "Active codec",
                    "PCM format",
                    "PCM rate (Hz)",
                    "PCM channels",
                ] {
                    r.push(fact(label, "Unavailable · no observed playback transport"));
                }
            }
            let first = [
                "Device",
                "Active codec",
                "PCM format",
                "PCM rate (Hz)",
                "PCM channels",
                "Preference",
                "Codec Preference",
            ];
            r.sort_by_key(|row| {
                first
                    .iter()
                    .position(|label| *label == row.label)
                    .unwrap_or(first.len())
            });
            r
        }
        "codec" => {
            let stopped = m.playback == reborn_core::PlaybackState::Stopped;
            let connected = entries(&p.bluetooth["devices"])
                .any(|d| d["connected"] == true && d["audio"] == true);
            vec![
                fact("Preference", m.settings.codec_preference.label()),
                action(
                    "SBC",
                    "codec_sbc",
                    "Built baseline; stop playback before applying",
                    connected && stopped && !busy,
                ),
                action(
                    "Auto",
                    "codec_auto",
                    "No production-qualified codec until owner qualification",
                    connected
                        && stopped
                        && !busy
                        && p.capabilities["capabilities"]["bluetooth"]["codec_auto"]
                            ["eligible_codecs"]
                            .as_array()
                            .is_some_and(|a| !a.is_empty()),
                ),
                action(
                    "Stop Playback",
                    "stop_playback",
                    "Release the PCM for a codec request",
                    !stopped,
                ),
                page(
                    "Active Codec & PCM",
                    "bluetooth",
                    "Actual transport observation",
                ),
            ]
        }
        "audio" => {
            let a = &p.audio;
            let mut r = vec![
                field("Source codec", &a["source"]["codec"]),
                field("Source rate (Hz)", &a["source"]["sample_rate"]),
                field("Source precision (bits)", &a["source"]["source_bits"]),
                field("Decoded precision", &a["decoder_format"]),
                field("Processing", &a["internal_processing_format"]),
                fact(
                    "ReplayGain",
                    crate::replay_gain_label(m.settings.replay_gain),
                ),
                field("Applied gain (dB)", &a["replay_gain"]["applied_gain_db"]),
                field("Equalizer active", &a["eq"]["active"]),
                fact("Crossfade", crate::crossfade_label(m.settings.crossfade_ms)),
                fact(
                    "Selected output",
                    crate::components::output_label(&m.output),
                ),
            ];
            r.push(fact(
                "Sink observation",
                if m.playback == reborn_core::PlaybackState::Playing {
                    "Current playback"
                } else {
                    "Last opened sink; playback is inactive"
                },
            ));
            r.push(field("Observed ALSA format", &a["alsa"]["format"]));
            r.push(field("Observed ALSA rate", &a["alsa"]["rate"]));
            r.push(field("Observed ALSA channels", &a["alsa"]["channels"]));
            for (path, v) in s["audio"]["hw_params"].as_object().into_iter().flatten() {
                r.push(fact(path, value(v)));
            }
            r
        }
        "usb" => {
            let mut r = vec![
                fact("PC Transfer", "Authenticated SFTP over USB only"),
                field("SFTP readiness", &s["system"]["ssh"]["state"]),
                field("USB address", &s["system"]["ssh"]["bind"]),
                fact("Authentication", "Use the owner's configured SSH key"),
                fact("Transfer activity", "Activity reporting unavailable"),
            ];
            for udc in entries(&s["system"]["usb"]["udcs"]) {
                r.insert(1, field("USB cable", &udc["state"]));
            }
            r.push(fact("Available over Wi-Fi", "No"));
            r.push(page("Free Space", "storage", "Internal storage and SD"));
            r
        }
        "clock" => {
            let t = &s["system"]["time"];
            let mut r = vec![
                fact(
                    "Clock",
                    if t["tls_ready"] == true {
                        "Established"
                    } else {
                        "Untrusted · connect Wi-Fi to synchronize"
                    },
                ),
                field("Synchronization source", &t["source"]),
            ];
            if t["tls_ready"] == true {
                r.push(field("UTC time", &s["record"]["wall_timestamp"]));
            }
            r
        }
        "about" => vec![
            fact("Reborn", reborn_core::VERSION),
            field("Y2Linux", &s["system"]["versions"]["release_version"]),
            field("Kernel", &s["record"]["kernel"]),
            fact("Current output", crate::components::output_label(&m.output)),
            page(
                "Build Information",
                "versions",
                "Compiled source and rootfs identity",
            ),
        ],
        "versions" => vec![
            fact("Reborn", reborn_core::VERSION),
            field("Y2Linux", &s["system"]["versions"]["release_version"]),
            field("Kernel", &s["record"]["kernel"]),
            field("Platform build", &s["system"]["versions"]["build_id"]),
            fact("Current output", crate::components::output_label(&m.output)),
            field(
                "Compiled Reborn commit",
                &s["system"]["versions"]["reborn_source_commit"],
            ),
            field(
                "Platform source commit",
                &s["system"]["versions"]["build_git_commit"],
            ),
            field("Rootfs release", &s["system"]["versions"]["rootfs_version"]),
        ],
        "update" | "update_diag" => {
            let u = &s["system"]["update"];
            let state = u["state"].as_str().unwrap_or("Unavailable");
            let ready = enabled(m, "ota");
            let mut rows = vec![
                fact(
                    "Current release",
                    value(&s["system"]["versions"]["release_version"]),
                ),
                fact("Update state", state),
                fact(
                    "Update scope",
                    "Signed system root only; kernel / BOOTIMG stays manual",
                ),
                field("Network readiness", &s["wifi"]["state"]),
                field("Download / staging", &u["download"]["state"]),
                fact(
                    "Signature / payload",
                    if u["download"]["state"] == "Verified" {
                        "Verified by the platform updater"
                    } else {
                        "Not yet verified for installation"
                    },
                ),
                field("Available release", &u["download"]["release_version"]),
                field("Previous result", &u["failure"]),
                action(
                    "Check for Update",
                    "task:update_check",
                    "Configured signed update channel",
                    ready && !busy,
                ),
                action(
                    "Download & Stage",
                    "task:update_stage",
                    "Verify signature and reserve space",
                    ready
                        && !busy
                        && matches!(state, "Idle" | "Acknowledged" | "RolledBack" | "Failed"),
                ),
                action(
                    "Install & Restart",
                    "confirm:update_apply",
                    "Only a verified queued root update",
                    ready && !busy && state == "Queued",
                ),
                action(
                    "Cancel Queued Update",
                    "confirm:update_cancel",
                    "Preserve the installed system",
                    ready && !busy && state == "Queued",
                ),
                action(
                    "Restore Previous Root",
                    "confirm:update_rollback",
                    "Requires a verified platform backup",
                    ready
                        && !busy
                        && u["rollback_allowed"] == true
                        && matches!(state, "Acknowledged" | "PendingHealth"),
                ),
                field("Signing key ID", &u["key_id"]),
                field("Update sequence", &u["sequence"]),
                fact(
                    "Rollback",
                    if state == "RollbackPending" {
                        "Pending · owner restart required"
                    } else {
                        "See update state"
                    },
                ),
            ];
            if id == "update" {
                let order = [
                    "Current release",
                    "Update state",
                    "Check for Update",
                    "Download & Stage",
                    "Install & Restart",
                    "Available release",
                    "Network readiness",
                    "Cancel Queued Update",
                    "Restore Previous Root",
                ];
                rows.retain(|row| order.contains(&row.label.as_str()));
                rows.sort_by_key(|row| {
                    order
                        .iter()
                        .position(|label| *label == row.label)
                        .unwrap_or(99)
                });
                rows.push(page(
                    "Latest Result",
                    "result",
                    "Operation outcome or error",
                ));
                rows.push(page(
                    "Update Details",
                    "update_diag",
                    "Signature, sequence and root-only scope",
                ));
            } else {
                rows.push(page(
                    "Latest Result",
                    "result",
                    "Operation outcome or error",
                ));
            }
            rows
        }
        "backup" => vec![
            fact(
                "Settings & Player Data",
                "Private export preserves session and queue",
            ),
            fact(
                "Included",
                "Settings, queue and consistent library snapshot",
            ),
            fact(
                "Excluded",
                "SSH private keys, Bluetooth bonds, network passwords, calibration",
            ),
            action(
                "Create Export",
                "task:export",
                "Retrieve /data/exports using USB SFTP",
                !busy && enabled(m, "storage"),
            ),
            page("Export Result", "result", "Archive path and checksum"),
            page("PC Transfer", "usb", "Download the completed archive"),
        ],
        "maintenance" => vec![
            page(
                "Reset Reborn Settings",
                "reset_settings",
                "Preserves music, queue and library",
            ),
            page(
                "Reset Network",
                "reset_network",
                "Removes saved network credentials",
            ),
            page(
                "Remove Bluetooth Bonds",
                "reset_bluetooth-bonds",
                "Devices must be paired again",
            ),
            page(
                "Rebuild Library",
                "reset_library",
                "Recoverable database replacement",
            ),
            page(
                "Clear Cache",
                "reset_caches",
                "Removes disposable cached files",
            ),
            page(
                "Full User Data Reset",
                "reset_full-user",
                "Removes music and known user state",
            ),
        ],
        "benchmarks" => vec![
            page(
                "Latest Result",
                "result",
                "Measurements and operation status",
            ),
            action(
                "Refresh Platform Health",
                "task:health",
                "Read-only subsystem checks",
                !busy,
            ),
            action(
                "Network Check",
                "task:network",
                "Read-only readiness; no peer required",
                !busy,
            ),
            action(
                "Storage Benchmark",
                "confirm:storage_benchmark",
                "16 MiB private scratch; never music",
                !busy && enabled(m, "storage"),
            ),
            action(
                "Library Benchmark",
                "confirm:library_benchmark",
                "1,000 synthetic tracks in private scratch",
                !busy && enabled(m, "storage"),
            ),
        ],
        "boot" => {
            let mut r = vec![
                field("Boot ID", &s["record"]["boot_id"]),
                field(
                    "Previous boot",
                    &s["system"]["boot_history"]["previous_boot_id"],
                ),
                field(
                    "Clean shutdown",
                    &s["system"]["previous_boot_evidence"]["previous_orderly_shutdown"],
                ),
                field("Last stage", &s["system"]["boot_history"]["last_stage"]),
                field("Reset reason", &s["system"]["reset_cause"]),
                field("Kernel taint", &s["system"]["kernel_taint"]),
            ];
            for (name, v) in s["readiness"].as_object().into_iter().flatten() {
                r.push(fact(
                    name,
                    format!("{} · {}", value(&v["state"]), value(&v["reason"])),
                ));
            }
            r
        }
        "result" => {
            let r = p.result.as_ref().unwrap_or(&Value::Null);
            let mut rows = vec![fact(
                "Operation",
                p.failure.clone().unwrap_or_else(|| {
                    if busy {
                        "In progress".into()
                    } else if !r["state"].is_null() {
                        value(&r["state"])
                    } else if !r["record"]["result"].is_null() {
                        value(&r["record"]["result"])
                    } else {
                        "No completed operation".into()
                    }
                }),
            )];
            for (label, key) in [
                ("Export path", "path"),
                ("SHA-256", "sha256"),
                ("Release", "release_version"),
            ] {
                if !r[key].is_null() {
                    rows.push(field(label, &r[key]));
                }
            }
            if r["record"]["result"] == "FAILED" {
                rows.push(fact("Check did not finish", "Scratch results are incomplete. User music was not used. Refresh platform health before retrying."));
            }
            for measurement in entries(&r["measurements"]) {
                let label = value(&measurement["operation"]);
                rows.push(fact(
                    &label,
                    format!(
                        "{} · {} MB/s · p95 {} ms",
                        value(&measurement["result"]),
                        value(&measurement["MB_per_second"]),
                        value(&measurement["p95_ms"])
                    ),
                ));
            }
            // Reborn's benchmark emits a bounded structured object. Only public
            // metric names/values are shown; no raw platform response dump.
            if let Some(measurements) = r["measurements"].as_object() {
                for measurement in entries(&r["measurements"]["ui"]) {
                    rows.push(fact(
                        &value(&measurement["operation"]),
                        format!(
                            "p95 {} ms · max {} ms",
                            value(&measurement["p95_ms"]),
                            value(&measurement["max_ms"])
                        ),
                    ));
                }
                for (key, v) in measurements {
                    if key != "memory" && !v.is_object() && !v.is_array() {
                        rows.push(field(key, v));
                    }
                }
            }
            if !r["wifi"].is_null() {
                rows.push(field("Wi-Fi readiness", &r["wifi"]["state"]));
                rows.push(field("IP address", &r["wifi"]["ip_addresses"]));
                rows.push(field("DNS ready", &r["wifi"]["dns_ready"]));
            }
            rows
        }
        id if id.starts_with("reset_") => {
            let scope = id.trim_start_matches("reset_");
            vec![fact("Owner maintenance required","This platform operation requires stopped services. Use authenticated USB maintenance."),fact("Scope",scope),fact("Prepare",format!("y2-platform reset plan --scope {scope}")),fact("Confirm","Stop the consumers listed by the plan, re-plan, then execute its exact digest."),fact("Full user reset",if scope=="full-user"{"Deletes music and known user state; requires --erase-user-music. Not secure erasure."}else{"Not selected. Other reset scopes remain separate."}),page("Backup First","backup","Export settings and player data"),page("PC Transfer","usb","Authenticated USB owner connection")]
        }
        id if id.starts_with("value:") => vec![Item::new("Back", "back")],
        _ => vec![fact(
            "Unavailable",
            "This platform section is not exposed by this build",
        )],
    };
    if out.is_empty() {
        out.push(fact(
            "No observation",
            "Refresh to read the Platform v1 service",
        ));
    }
    if !id.starts_with("value:") && !id.is_empty() {
        out.push(action(
            "Refresh",
            "task:refresh",
            "Read current platform observations",
            !busy,
        ));
    }
    out
}
