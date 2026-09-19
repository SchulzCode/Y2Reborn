#![forbid(unsafe_code)]
use serde_json::{json, Value};
use std::{fs, path::PathBuf, process::Command};
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
    json!({"supplies":supplies,"backlight":backlight().map(|p|p.to_string_lossy().into_owned())})
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
