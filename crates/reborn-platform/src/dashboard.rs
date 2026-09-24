//! Bounded application client for Platform v1. All commands use a fixed argv;
//! no shell, raw hardware access, private credentials, or platform policy here.
use reborn_core::PlatformTask;
use serde_json::{json, Value};
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::mpsc::{sync_channel, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};
const LIMIT: u64 = 1024 * 1024;
pub struct Reply {
    pub task: PlatformTask,
    pub result: Result<Value, String>,
}
pub struct Dashboard {
    pub commands: SyncSender<PlatformTask>,
    pub events: Receiver<Reply>,
}
impl Dashboard {
    pub fn spawn() -> std::io::Result<Self> {
        let (tx, rx) = sync_channel(1);
        let (events_tx, events) = sync_channel(1);
        thread::Builder::new()
            .name("platform-ui".into())
            .spawn(move || {
                while let Ok(task) = rx.recv() {
                    let result = execute(task);
                    if events_tx.send(Reply { task, result }).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            commands: tx,
            events,
        })
    }
}
fn call(args: &[&str], seconds: u64) -> Result<Value, String> {
    run("/usr/bin/y2-platform", args, Duration::from_secs(seconds))
}
fn run(program: &str, args: &[&str], deadline: Duration) -> Result<Value, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Platform service is unavailable. Try again after startup.".to_owned())?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Platform response is unavailable")?;
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
                bytes = Some(output.map_err(|_| "Could not read platform response")?);
            }
        }
        if bytes.as_ref().is_some_and(|b| b.len() > LIMIT as usize) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Platform response exceeded the safe size limit".into());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|_| "Platform process status unavailable")?
        {
            let raw = match bytes {
                Some(v) => v,
                None => rx
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| "Platform response timed out")?
                    .map_err(|_| "Could not read platform response")?,
            };
            if raw.len() > LIMIT as usize {
                return Err("Platform response exceeded the safe size limit".into());
            }
            let value: Value = serde_json::from_slice(&raw)
                .map_err(|_| "Platform returned an unreadable response")?;
            if !value.is_object() {
                return Err("Platform response has an unsupported shape".into());
            }
            // Health exit 1 is valid evidence, not a transport failure.
            if !status.success() && value["schema"] != "org.y2linux.health/v1" {
                return Err(friendly_error(
                    value["failure"].as_str().unwrap_or("operation_failed"),
                ));
            }
            return Ok(value);
        }
        if started.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Operation timed out. Refresh its state before trying again.".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn schema(value: Value, expected: &str) -> Result<Value, String> {
    if value["schema"] == expected {
        Ok(value)
    } else {
        Err("Unsupported Platform v1 response. Check the installed software pair.".into())
    }
}
fn execute(task: PlatformTask) -> Result<Value, String> {
    use PlatformTask::*;
    match task {
        Refresh => {
            let status = schema(
                call(&["status", "--reborn", "--pss"], 25)?,
                "org.y2linux.status/v1",
            )?;
            let capabilities = schema(call(&["capabilities"], 10)?, "org.y2linux.capabilities/v1")?;
            Ok(json!({"status":status,"capabilities":capabilities}))
        }
        Health => schema(call(&["health"], 30)?, "org.y2linux.health/v1"),
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
        // Passive IP/route/DNS readiness only. Active ping/throughput requires an
        // owner-chosen peer; this UI does not invent one or ping an external service.
        NetworkCheck => schema(call(&["status", "wifi"], 25)?, "org.y2linux.status/v1"),
    }
}
pub fn friendly_error(error: &str) -> String {
    let e = error.to_ascii_lowercase();
    if e.contains("password") || e.contains("wrong_key") || e.contains("wrong_credentials") {
        "Wrong password. Forget the saved network and connect again."
    } else if e.contains("dns") {
        "DNS is unavailable. Check the network's internet connection."
    } else if e.contains("dhcp") || e.contains("no_address") {
        "Couldn't get an IP address. Check the router and retry."
    } else if e.contains("auth") && e.contains("time") {
        "Authentication timed out. Check the password and retry."
    } else if e.contains("space") || e.contains("reserve") {
        "Not enough free storage. Remove unneeded files using PC Transfer."
    } else if e.contains("signature") || e.contains("key") {
        "Update signature could not be verified. Use a trusted signed package."
    } else if e.contains("kernel") || e.contains("compatib") {
        "This update needs a manual platform update. The installed system is unchanged."
    } else if e.contains("url") || e.contains("source") || e.contains("channel") {
        "No usable update channel. Configure the owner update channel over USB."
    } else if e.contains("clock") || e.contains("time_not") {
        "Clock not established. Connect Wi-Fi and wait for synchronization."
    } else if e.contains("unavailable") || e.contains("not_found") {
        "Service unavailable. Check connectivity and try again."
    } else {
        "Operation could not complete. Refresh status, then retry. See Diagnostics for details."
    }
    .into()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_future_and_oversize_records_fail_closed() {
        assert!(schema(json!({"schema":"v2"}), "org.y2linux.status/v1").is_err());
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
        let health = run(
            "/bin/sh",
            &[
                "-c",
                "printf '{\"schema\":\"org.y2linux.health/v1\",\"state\":\"FAILED\"}'; exit 1",
            ],
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(health["state"], "FAILED");
        assert!(run("/bin/sleep", &["2"], Duration::from_millis(40)).is_err());
    }
    #[test]
    fn ordinary_errors_are_actionable_and_do_not_echo_service_text() {
        assert!(friendly_error("signature_invalid raw private details")
            .contains("trusted signed package"));
        assert!(!friendly_error("org.bluez.Error.Failed private").contains("org.bluez"));
    }
}
