#![forbid(unsafe_code)]
use reborn_core::platform::{BatteryState, ChargingState, LowBattery, ShutdownIntent};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use y2p_contract::power::{
    BatterySource, BatteryState as Level, Intent, IntentState, PowerRecord, ShutdownAction,
};
pub fn status() -> Value {
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    let platform = fs::read("/run/y2/power.json")
        .ok()
        .filter(|b| b.len() <= 8192)
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| Some((PowerRecord::deserialize(&v).ok()?, v)))
        .filter(|(record, _)| {
            crate::native::monotonic_seconds()
                .is_some_and(|now| fresh_record(record, boot.trim(), now))
        });
    let mut supplies = vec![];
    for entry in fs::read_dir("/sys/class/power_supply")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
    {
        let mut item = json!({"name":entry.file_name().to_string_lossy()});
        for name in [
            "type",
            "status",
            "online",
            "present",
            "capacity",
            "voltage_now",
            "current_now",
            "temp",
        ] {
            // The safety owner already samples BAT0. A fresh same-boot value
            // avoids a redundant hardware ADC conversion for each UI poll.
            if name == "voltage_now" && item["type"].as_str() == Some("Battery") {
                if let Some(uv) = platform
                    .as_ref()
                    .and_then(|(record, _)| recorded_voltage(record))
                {
                    item[name] = json!(uv.to_string());
                    continue;
                }
            }
            if let Ok(v) = fs::read_to_string(entry.path().join(name)) {
                item[name] = json!(v.trim());
            }
        }
        supplies.push(item);
    }
    let platform = platform.map(|(_, value)| value);
    json!({"supplies":supplies,"backlight":backlight().map(|p|p.to_string_lossy().into_owned()),"platform":platform})
}
fn recorded_voltage(record: &PowerRecord) -> Option<u64> {
    record
        .battery
        .voltage_uv
        .and_then(|v| u64::try_from(v).ok())
        .filter(|v| *v > 0)
}
fn fresh_record(record: &PowerRecord, boot: &str, now: f64) -> bool {
    record.boot_id.as_deref() == Some(boot) && !boot.is_empty() && {
        let age = now - record.monotonic_ns as f64 / 1_000_000_000.;
        (0.0..5.0).contains(&age)
    }
}
/// The normal-user battery state: valid SOC, charging and low-battery level.
pub fn battery(status: &Value) -> BatteryState {
    let supply = status["supplies"].as_array().and_then(|supplies| {
        supplies
            .iter()
            .find(|s| s["type"].as_str() == Some("Battery"))
            .or_else(|| supplies.iter().find(|s| s["name"].as_str() == Some("BAT0")))
    });
    let status_text = supply.and_then(|s| s["status"].as_str());
    let cable = matches!(status_text, Some("Charging" | "Full" | "Not charging"));
    let charging = match status_text {
        Some("Charging") => ChargingState::Charging,
        Some("Full") => ChargingState::Full,
        Some("Discharging" | "Not charging") => ChargingState::OnBattery,
        _ => ChargingState::Unknown,
    };
    let record = PowerRecord::deserialize(&status["platform"]).ok();
    let level = match record.as_ref().map(|r| r.state) {
        Some(Level::Low) => LowBattery::Low,
        Some(Level::Critical) => LowBattery::Critical,
        Some(Level::ShutdownPending) => LowBattery::ShuttingDown,
        _ => LowBattery::Normal,
    };
    BatteryState {
        percent: record.as_ref().and_then(battery_percent),
        estimated: record.as_ref().is_some_and(|r| {
            matches!(
                r.battery.source,
                BatterySource::VoltageEstimate | BatterySource::Hybrid
            )
        }),
        charging,
        cable,
        level,
    }
}

fn battery_percent(record: &PowerRecord) -> Option<u8> {
    // `stock_oam`: the stock open-circuit estimate the platform reports
    // since POWER-FINAL-29 (charging and resting); Reborn showed no level then.
    if record.battery.source == BatterySource::Unavailable {
        return None;
    }
    u8::try_from(record.battery.percent?)
        .ok()
        .filter(|p| *p <= 100)
}
fn backlight() -> Option<PathBuf> {
    fs::read_dir("/sys/class/backlight")
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.join("bl_power").exists())
}
pub fn blank(off: bool) -> Result<(), String> {
    let path = backlight().ok_or("backlight unavailable")?;
    fs::write(path.join("bl_power"), if off { "4\n" } else { "0\n" }).map_err(|e| e.to_string())
}

/// Whether the panel backlight accepts brightness levels.
pub fn brightness_available() -> bool {
    backlight().is_some_and(|p| {
        fs::read_to_string(p.join("max_brightness"))
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .is_some_and(|max| max >= 4)
    })
}

/// Set the backlight to `percent` of its maximum (bounded to 10–100 %).
pub fn set_brightness(percent: u8) -> Result<(), String> {
    let path = backlight().ok_or("backlight unavailable")?;
    let max = fs::read_to_string(path.join("max_brightness"))
        .map_err(|e| e.to_string())?
        .trim()
        .parse::<u32>()
        .map_err(|_| "invalid backlight range")?;
    fs::write(
        path.join("brightness"),
        format!("{}\n", brightness_value(max, percent)),
    )
    .map_err(|e| e.to_string())
}

/// Keep the headphone DAC powered (true) or let it power down two idle
/// minutes later (false), through the Y2Linux kernel's hold (Beta 8.23). The
/// stock firmware keeps the DAC and its crystal running while the screen is
/// on; a marginal crystal stops after enough power cycles (#52). Best effort:
/// Ok(false) where the kernel has no hold (older Y2Linux), nothing written.
pub fn dac_hold(on: bool) -> Result<bool, String> {
    dac_hold_at(Path::new("/sys/bus/i2c/devices"), on)
}
fn dac_hold_at(devices: &Path, on: bool) -> Result<bool, String> {
    let Some(path) = dac_hold_path(devices) else {
        return Ok(false);
    };
    fs::write(&path, if on { "1\n" } else { "0\n" })
        .map(|()| true)
        .map_err(|e| format!("{}: {e}", path.display()))
}
/// The DAC (I2C address 0x30) sits on bus 0 or 1, by probe order.
fn dac_hold_path(devices: &Path) -> Option<PathBuf> {
    let mut holds = fs::read_dir(devices)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.ends_with("-0030")))
        .map(|e| e.path().join("y2_hp/hold"))
        .filter(|p| p.is_file())
        .collect::<Vec<_>>();
    holds.sort();
    holds.into_iter().next()
}

fn brightness_value(max: u32, percent: u8) -> u32 {
    let percent = u32::from(percent.clamp(reborn_core::BRIGHTNESS_MIN, 100));
    (max * percent).div_ceil(100).clamp(1, max.max(1))
}

/// Request a platform power transition through the initramfs-provided command.
/// UI code never shells out directly; this is the platform service boundary.
pub fn request_shutdown(reboot: bool) -> Result<(), String> {
    if std::path::Path::new("/etc/y2linux/platform-contract").exists() {
        platform_request(json!({"action":if reboot {"reboot"} else {"poweroff"},"reason":"user"}))?;
        return Ok(());
    }
    let program = if reboot {
        "/sbin/reboot"
    } else {
        "/sbin/poweroff"
    };
    let status = Command::new(program)
        .status()
        .map_err(|e| format!("{} unavailable: {}", program, e))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} returned {}", program, status))
    }
}

fn platform_request(request: Value) -> Result<Value, String> {
    let mut client = UnixStream::connect("/run/y2/power.sock").map_err(|e| e.to_string())?;
    client
        .set_read_timeout(Some(Duration::from_millis(500)))
        .map_err(|e| e.to_string())?;
    client
        .set_write_timeout(Some(Duration::from_millis(500)))
        .map_err(|e| e.to_string())?;
    writeln!(client, "{request}").map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    let mut byte = [0u8];
    while data.len() < 8192 {
        if client.read(&mut byte).map_err(|e| e.to_string())? == 0 {
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        data.push(byte[0]);
    }
    let response: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    if response["ok"] != true {
        return Err(response["error"].to_string());
    }
    Ok(response["result"].clone())
}

pub fn shutdown_intent() -> Option<ShutdownIntent> {
    let data = fs::read("/run/y2/shutdown.json").ok()?;
    if data.len() > 8192 {
        return None;
    }
    let value: Intent = serde_json::from_slice(&data).ok()?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    intent(&value, boot.trim())
}

fn intent(value: &Intent, boot: &str) -> Option<ShutdownIntent> {
    if value.schema != Some(1)
        || value.state != IntentState::ShutdownPending
        || !value.boot_id.is(Some(boot))
    {
        return None;
    }
    let id = &value.id;
    (id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')).then(|| {
        ShutdownIntent {
            id: id.clone(),
            restart: value.action == ShutdownAction::Reboot,
            low_battery: value.reason.as_deref() == Some("low_battery"),
        }
    })
}

pub fn acknowledge_shutdown(id: &str, ready: bool) -> Result<(), String> {
    platform_request(
        json!({"operation":"ack","id":id,"outcome":if ready {"Ready"} else {"Failed"}}),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn voltage_reuse_requires_fresh_same_boot_valid_observation() {
        let record = |value: Value| PowerRecord::deserialize(&value).unwrap();
        let value = record(json!({"boot_id":"one","monotonic_ns":1_000_000_000u64,
                           "battery":{"voltage_uv":3_900_000}}));
        assert!(fresh_record(&value, "one", 2.0));
        assert!(!fresh_record(&value, "two", 2.0));
        assert!(!fresh_record(&value, "one", 6.0));
        assert!(!fresh_record(&value, "one", 0.0));
        assert_eq!(recorded_voltage(&value), Some(3_900_000));
        let voltage = |uv: i64| recorded_voltage(&record(json!({"battery":{"voltage_uv":uv}})));
        assert_eq!(voltage(0), None);
        assert_eq!(voltage(-1), None);
    }
    #[test]
    fn shutdown_intent_requires_current_boot_and_pending_state() {
        let mut value = json!({"schema":1,"state":"ShutdownPending","boot_id":"one",
            "id":"12345678-1234-1234-1234-123456789abc","action":"poweroff",
            "deadline_monotonic_ns":0,"client":null,"acknowledgement":null});
        let read = |value: &Value, boot: &str| intent(&Intent::deserialize(value).ok()?, boot);
        assert!(read(&value, "one").is_some_and(|i| !i.restart && !i.low_battery));
        assert!(read(&value, "two").is_none());
        value["action"] = json!("reboot");
        assert!(read(&value, "one").is_some_and(|i| i.restart));
        value["reason"] = json!("low_battery");
        assert!(read(&value, "one").is_some_and(|i| i.low_battery));
        value["state"] = json!("Failed");
        assert!(read(&value, "one").is_none());
    }

    #[test]
    fn battery_presents_valid_soc_charging_and_low_level_only() {
        let status = json!({"supplies":[{"name":"BAT0","type":"Battery","status":"Charging"}],
            "platform":{"state":"Low","battery":{"source":"voltage_estimate","percent":72}}});
        let b = battery(&status);
        assert_eq!(b.percent, Some(72));
        assert_eq!(b.charging, ChargingState::Charging);
        assert_eq!(b.level, LowBattery::Low);
        let oam = battery(&json!({"platform":{"battery":{"source":"stock_oam","percent":96}}}));
        assert_eq!(oam.percent, Some(96));
        let unknown = battery(&json!({"platform":{"battery":{"source":"guess","percent":72}}}));
        assert_eq!(unknown.percent, None);
        assert_eq!(unknown.charging, ChargingState::Unknown);
        assert!(b.cable && !unknown.cable);
        // A full battery held on its cable reads "Not charging": shown as on battery, still a cable.
        let held = |status: &str| {
            battery(&json!({"supplies":[{"name":"BAT0","type":"Battery","status":status}]}))
        };
        assert_eq!(held("Not charging").charging, ChargingState::OnBattery);
        assert!(held("Not charging").cable && held("Full").cable);
        assert!(!held("Discharging").cable);
    }

    #[test]
    fn dac_hold_finds_the_dac_on_either_bus_and_is_quiet_without_one() {
        let root = std::env::temp_dir().join(format!("reborn-dac-hold-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // An older kernel: the DAC without a hold, other devices, no hold.
        fs::create_dir_all(root.join("1-0030")).unwrap();
        fs::create_dir_all(root.join("0-0050/y2_hp")).unwrap();
        fs::write(root.join("0-0050/y2_hp/hold"), "0\n").unwrap();
        fs::create_dir_all(root.join("i2c-1")).unwrap();
        assert_eq!(dac_hold_path(&root), None);
        assert_eq!(dac_hold_at(&root, true), Ok(false));
        assert_eq!(dac_hold_at(&root.join("missing"), true), Ok(false));
        // This kernel: bus 1 here (probe order), written as asked.
        fs::create_dir_all(root.join("1-0030/y2_hp")).unwrap();
        let hold = root.join("1-0030/y2_hp/hold");
        fs::write(&hold, "0\n").unwrap();
        assert_eq!(dac_hold_path(&root), Some(hold.clone()));
        assert_eq!(dac_hold_at(&root, true), Ok(true));
        assert_eq!(fs::read_to_string(&hold).unwrap(), "1\n");
        assert_eq!(dac_hold_at(&root, false), Ok(true));
        assert_eq!(fs::read_to_string(&hold).unwrap(), "0\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn brightness_is_bounded_and_never_zero() {
        assert_eq!(brightness_value(255, 100), 255);
        assert_eq!(brightness_value(255, 0), 13);
        // The Y2's backlight: 5 % is its second-lowest of 32 levels.
        assert_eq!(brightness_value(32, 5), 2);
        assert_eq!(brightness_value(4, 20), 1);
        assert_eq!(brightness_value(1023, 60), 614);
    }
}

/// A real process-owned PM lock, not a stale PID or marker file.
pub fn sleep_owner_present() -> std::io::Result<bool> {
    sleep_owner_at(Path::new("/run/y2/suspend.lock"))
}
fn sleep_owner_at(path: &Path) -> std::io::Result<bool> {
    let lock = match fs::File::open(path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match lock.try_lock_shared() {
        Ok(()) => Ok(false),
        Err(fs::TryLockError::WouldBlock) => Ok(true),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Who may hold an audio output that refused to open as busy, if the kernel
/// says: the headphone PCM's owner, and the holders of the activity lease the
/// sink takes before it opens (the sleep helper keeps it exclusive until the
/// radios are back after a wake).
pub fn output_busy_holders() -> Value {
    let pcm = fs::read_to_string("/proc/asound/card0/pcm0p/sub0/status").ok();
    let lease = activity_lease_holders();
    json!({
        "pcm_owner_pid":pcm.as_deref().and_then(pcm_owner_pid),
        "activity_lease":lease
    })
}
/// `owner_pid` of an open substream's status; a closed one has none.
fn pcm_owner_pid(status: &str) -> Option<u32> {
    status.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "owner_pid").then(|| value.trim().parse().ok())?
    })
}
fn activity_lease_holders() -> Vec<String> {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::metadata("/run/y2/activity.lock") else {
        return Vec::new();
    };
    let locks = fs::read_to_string("/proc/locks").unwrap_or_default();
    flock_holders(&locks, meta.dev(), meta.ino())
        .into_iter()
        .map(|pid| {
            let name = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            format!("{pid}/{}", name.trim())
        })
        .collect()
}
/// The PIDs holding a flock on one file in `/proc/locks` (device numbers in
/// hex, as the kernel prints them).
fn flock_holders(locks: &str, dev: u64, inode: u64) -> Vec<u32> {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    locks
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.get(1) != Some(&"FLOCK") {
                return None;
            }
            let pid = fields.get(4)?.parse().ok()?;
            let mut id = fields.get(5)?.split(':');
            let same = u64::from_str_radix(id.next()?, 16).ok()? == major
                && u64::from_str_radix(id.next()?, 16).ok()? == minor
                && id.next()?.parse::<u64>().ok()? == inode;
            same.then_some(pid)
        })
        .take(8)
        .collect()
}

#[cfg(test)]
mod sleep_owner_tests {
    use super::*;
    #[test]
    fn sleep_owner_is_the_live_lock_not_a_leftover_file() {
        let path = std::env::temp_dir().join(format!("reborn-sleep-owner-{}", std::process::id()));
        let owner = fs::File::create(&path).unwrap();
        assert!(!sleep_owner_at(&path).unwrap());
        owner.lock().unwrap();
        assert!(sleep_owner_at(&path).unwrap());
        drop(owner);
        assert!(!sleep_owner_at(&path).unwrap());
        fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod busy_output_tests {
    use super::*;
    #[test]
    fn a_busy_output_names_its_holders_from_the_kernel() {
        let open = "state: RUNNING\nowner_pid   : 812\ntrigger_time: 1.0\n";
        assert_eq!(pcm_owner_pid(open), Some(812));
        assert_eq!(pcm_owner_pid("closed\n"), None);
        assert_eq!(pcm_owner_pid(""), None);
        // st_dev 0:0x17 (tmpfs /run), inode 4242.
        let locks = "1: FLOCK  ADVISORY  WRITE 977 00:17:4242 0 EOF\n\
                     2: FLOCK  ADVISORY  READ  501 00:17:9999 0 EOF\n\
                     3: POSIX  ADVISORY  WRITE 12 00:17:4242 0 EOF\n\
                     4: FLOCK  ADVISORY  READ  640 00:18:4242 0 EOF\n\
                     malformed\n";
        assert_eq!(flock_holders(locks, 0x17, 4242), [977]);
        assert!(flock_holders(locks, 0x17, 1).is_empty());
        assert!(flock_holders("", 0x17, 4242).is_empty());
        // Cheap and safe off the device (no /run/y2, no sound card).
        let holders = output_busy_holders();
        assert!(holders.get("activity_lease").is_some());
    }
}
