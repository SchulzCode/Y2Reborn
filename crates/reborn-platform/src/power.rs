#![forbid(unsafe_code)]
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
    json!({"supplies":supplies,"backlight":backlight().map(|p|p.to_string_lossy().into_owned()),
        "platform":fs::read("/run/y2/power.json").ok().and_then(|b|serde_json::from_slice::<Value>(&b).ok())})
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

pub fn shutdown_intent() -> Option<String> {
    let data = fs::read("/run/y2/shutdown.json").ok()?;
    if data.len() > 8192 {
        return None;
    }
    let value: Value = serde_json::from_slice(&data).ok()?;
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    intent_id(&value, boot.trim())
}

fn intent_id(value: &Value, boot: &str) -> Option<String> {
    if value["schema"] != 1 || value["state"] != "ShutdownPending" || value["boot_id"] != boot {
        return None;
    }
    let id = value["id"].as_str()?;
    (id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')).then(|| id.to_owned())
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
        assert!(intent_id(&value, "one").is_some());
        assert!(intent_id(&value, "two").is_none());
        value["state"] = json!("Failed");
        assert!(intent_id(&value, "one").is_none());
    }
}
