#![forbid(unsafe_code)]
use reborn_core::platform::{BatteryState, ChargingState, LowBattery, ShutdownIntent};
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::Command,
    time::Duration,
};
pub fn status() -> Value {
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
            if let Ok(v) = fs::read_to_string(entry.path().join(name)) {
                item[name] = json!(v.trim());
            }
        }
        supplies.push(item);
    }
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    let platform = fs::read("/run/y2/power.json")
        .ok()
        .filter(|b| b.len() <= 8192)
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(|v| {
            crate::native::monotonic_seconds().is_some_and(|now| fresh_record(v, boot.trim(), now))
        });
    json!({"supplies":supplies,"backlight":backlight().map(|p|p.to_string_lossy().into_owned()),"platform":platform})
}
fn fresh_record(value: &Value, boot: &str, now: f64) -> bool {
    value["boot_id"].as_str() == Some(boot)
        && !boot.is_empty()
        && value["monotonic_ns"].as_u64().is_some_and(|ns| {
            let age = now - ns as f64 / 1_000_000_000.;
            (0.0..5.0).contains(&age)
        })
}
/// The normal-user battery state: valid SOC, charging and low-battery level.
pub fn battery(status: &Value) -> BatteryState {
    let supply = status["supplies"].as_array().and_then(|supplies| {
        supplies
            .iter()
            .find(|s| s["type"].as_str() == Some("Battery"))
            .or_else(|| supplies.iter().find(|s| s["name"].as_str() == Some("BAT0")))
    });
    let charging = match supply.and_then(|s| s["status"].as_str()) {
        Some("Charging") => ChargingState::Charging,
        Some("Full") => ChargingState::Full,
        Some("Discharging" | "Not charging") => ChargingState::OnBattery,
        _ => ChargingState::Unknown,
    };
    let level = match status["platform"]["state"].as_str() {
        Some("Low") => LowBattery::Low,
        Some("Critical") => LowBattery::Critical,
        Some("ShutdownPending") => LowBattery::ShuttingDown,
        _ => LowBattery::Normal,
    };
    BatteryState {
        percent: battery_percent(status),
        estimated: matches!(
            status["platform"]["battery"]["source"].as_str(),
            Some("voltage_estimate" | "hybrid")
        ),
        charging,
        level,
    }
}

pub fn battery_percent(status: &Value) -> Option<u8> {
    let battery = &status["platform"]["battery"];
    let source = battery["source"].as_str()?;
    if !matches!(source, "fuel_gauge" | "hybrid" | "voltage_estimate") {
        return None;
    }
    let percent = battery["percent"].as_u64()?;
    (percent <= 100).then_some(percent as u8)
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

fn brightness_value(max: u32, percent: u8) -> u32 {
    let percent = u32::from(percent.clamp(10, 100));
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
    let value: Value = serde_json::from_slice(&data).ok()?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    intent(&value, boot.trim())
}

fn intent(value: &Value, boot: &str) -> Option<ShutdownIntent> {
    if value["schema"] != 1 || value["state"] != "ShutdownPending" || value["boot_id"] != boot {
        return None;
    }
    let id = value["id"].as_str()?;
    (id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')).then(|| {
        ShutdownIntent {
            id: id.to_owned(),
            restart: value["action"] == "reboot",
            low_battery: value["reason"] == "low_battery",
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
    fn shutdown_intent_requires_current_boot_and_pending_state() {
        let mut value = json!({"schema":1,"state":"ShutdownPending","boot_id":"one",
                              "id":"12345678-1234-1234-1234-123456789abc"});
        assert!(intent(&value, "one").is_some_and(|i| !i.restart && !i.low_battery));
        assert!(intent(&value, "two").is_none());
        value["action"] = json!("reboot");
        assert!(intent(&value, "one").is_some_and(|i| i.restart));
        value["reason"] = json!("low_battery");
        assert!(intent(&value, "one").is_some_and(|i| i.low_battery));
        value["state"] = json!("Failed");
        assert!(intent(&value, "one").is_none());
    }

    #[test]
    fn battery_presents_valid_soc_charging_and_low_level_only() {
        let status = json!({"supplies":[{"name":"BAT0","type":"Battery","status":"Charging"}],
            "platform":{"state":"Low","battery":{"source":"voltage_estimate","percent":72}}});
        let b = battery(&status);
        assert_eq!(b.percent, Some(72));
        assert_eq!(b.charging, ChargingState::Charging);
        assert_eq!(b.level, LowBattery::Low);
        let unknown = battery(&json!({"platform":{"battery":{"source":"guess","percent":72}}}));
        assert_eq!(unknown.percent, None);
        assert_eq!(unknown.charging, ChargingState::Unknown);
    }

    #[test]
    fn brightness_is_bounded_and_never_zero() {
        assert_eq!(brightness_value(255, 100), 255);
        assert_eq!(brightness_value(255, 0), 26);
        assert_eq!(brightness_value(4, 20), 1);
        assert_eq!(brightness_value(1023, 60), 614);
    }
}
