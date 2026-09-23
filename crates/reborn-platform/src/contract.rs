//! Versioned platform policy and application readiness. Hardware still belongs
//! to standard Linux interfaces; absent capabilities never become implicit true.
#![forbid(unsafe_code)]
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, io::Read, path::Path};

#[derive(Debug, Deserialize)]
pub struct Capability {
    #[serde(default)]
    pub implemented: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub qualified: bool,
    #[serde(flatten)]
    pub details: BTreeMap<String, Value>,
}
#[derive(Debug, Deserialize)]
pub struct Capabilities {
    pub schema: String,
    pub capabilities: BTreeMap<String, Capability>,
}
fn parse(raw: &[u8]) -> Result<Capabilities, String> {
    if raw.len() > 65536 {
        return Err("platform capability size limit".into());
    }
    let value: Capabilities = serde_json::from_slice(raw).map_err(|e| e.to_string())?;
    if value.schema != "org.y2linux.capabilities/v1" {
        return Err("unsupported platform capability schema".into());
    }
    Ok(value)
}
pub fn capabilities() -> Result<Capabilities, String> {
    let mut raw = Vec::new();
    fs::File::open("/etc/y2linux/capabilities.json")
        .map_err(|e| e.to_string())?
        .take(65537)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    parse(&raw)
}

/// Called only after the actual first KMS frame and creation of runtime workers.
/// The platform independently checks this PID generation and responsive health.
pub fn application_ready() -> Result<(), String> {
    if !Path::new("/etc/y2linux/platform-contract").exists() {
        return Ok(());
    }
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(|e| e.to_string())?;
    let stat = fs::read_to_string("/proc/self/stat").map_err(|e| e.to_string())?;
    let ticks: u64 = stat
        .rsplit_once(") ")
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .ok_or("process start unavailable")?
        .parse()
        .map_err(|_| "invalid process start")?;
    let ready = json!({"schema":1,"boot_id":boot.trim(),"pid":std::process::id(),
        "start_ticks":ticks,"first_frame":true,"monotonic_s":crate::native::monotonic_seconds()});
    reborn_core::atomic_write(
        Path::new("/run/y2/application-ready.json"),
        &serde_json::to_vec(&ready).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_qualification_stays_false_and_future_schema_is_rejected() {
        let caps = parse(br#"{"schema":"org.y2linux.capabilities/v1","capabilities":{"audio":{"implemented":true,"enabled":true}}}"#).unwrap();
        assert!(caps.capabilities["audio"].implemented);
        assert!(!caps.capabilities["audio"].qualified);
        assert!(!caps.capabilities.contains_key("usb_host"));
        assert!(parse(br#"{"schema":"v2","capabilities":{}}"#).is_err());
        assert!(parse(&vec![b' '; 65537]).is_err());
    }
}
