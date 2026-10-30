//! Names and shapes of the records the Y2 platform prints and writes (platform_api_version 1), shared by
//! the platform (Y2Linux `platform/`) and Reborn so the compiler checks both sides
//! (docs/planning/PLATFORM-RUST-PORT-PLAN.md §4; the inventory is platform/CONTRACT.md).
//!
//! The canonical copy lives in Y2Linux `platform/crates/y2p-contract`; Y2Reborn carries a copy made by
//! `release.py sync-contract`, and `release.py build` refuses to build when they differ. Edit it in
//! Y2Linux only.
//!
//! Types grow with the port. A record not typed field by field yet keeps its JSON (`Value`, or `extra`
//! maps flattened into the struct), so reading and writing it back changes nothing; the golden files in
//! `tests/golden` are the Python platform's own output.
// SPDX-License-Identifier: MIT
pub mod lenient;
pub mod network;
pub mod power;
pub mod reports;
pub mod sleep;
pub mod storage;
pub mod update;
pub mod usb;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The `schema` strings of the versioned records.
pub mod schema {
    pub const STATUS: &str = "org.y2linux.status/v1";
    pub const CAPABILITIES: &str = "org.y2linux.capabilities/v1";
    pub const HEALTH: &str = "org.y2linux.health/v1";
    pub const SLEEP: &str = "org.y2linux.sleep/v1";
    pub const ERROR: &str = "org.y2linux.error/v1";
    pub const CODEC_SETTINGS: &str = "org.y2linux.codec-settings/v1";
    /// The USB file-transfer receipt (`usb.py`), read by Reborn after an MTP session.
    pub const USB_TRANSFER: &str = "org.y2linux.usb-transfer/v1";
}

/// The root filesystem's platform contract marker (`/etc/y2linux/platform-contract`), checked by the
/// initramfs before it switches root.
pub const PLATFORM_CONTRACT: &str = "y2-platform-v1";
/// `versions.json` `platform_api_version` and `feature_contract_version`.
pub const PLATFORM_API_VERSION: u32 = 1;
pub const FEATURE_CONTRACT_VERSION: u32 = 2;

/// `y2-platform boot-stage <stage>`: the stages the platform records in `boot.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootStage {
    PlatformStart,
    ServicesStarted,
    ApplicationReady,
    ShutdownPending,
    ShutdownComplete,
}

impl BootStage {
    pub const ALL: [BootStage; 5] = [
        BootStage::PlatformStart,
        BootStage::ServicesStarted,
        BootStage::ApplicationReady,
        BootStage::ShutdownPending,
        BootStage::ShutdownComplete,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            BootStage::PlatformStart => "platform_start",
            BootStage::ServicesStarted => "services_started",
            BootStage::ApplicationReady => "application_ready",
            BootStage::ShutdownPending => "shutdown_pending",
            BootStage::ShutdownComplete => "shutdown_complete",
        }
    }

    pub fn parse(text: &str) -> Option<BootStage> {
        BootStage::ALL
            .into_iter()
            .find(|stage| stage.as_str() == text)
    }
}

/// One line of the runtime stage trail (`/run/y2/boot-stages.jsonl`). The initramfs writes its own
/// stages (`initramfs`, `switch_root`, …) into the same file, so `stage` is any name here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BootStageLine {
    pub stage: String,
    pub monotonic_s: f64,
}

/// `/data/system/platform/boot.json` (schema 1), also printed by `boot-stage`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BootRecord {
    pub schema: u32,
    pub boot_id: String,
    pub boot_count: u64,
    pub previous_boot_id: Option<String>,
    pub last_stage: BootStage,
    pub stage_monotonic_ns: u64,
    pub orderly_shutdown: bool,
    pub consecutive_unclean: u64,
    pub recovery_recommended: bool,
    pub reset_cause: Option<String>,
    pub reset_cause_reason: String,
    pub reset_status_raw: Option<String>,
    /// Earlier boots (at most 8), each `{boot_id, last_stage, orderly_shutdown, identity}` as the boot
    /// that wrote it left it: kept verbatim, also older shapes.
    pub history: Vec<Value>,
    /// `/etc/y2linux/versions.json` as read (null when unreadable).
    pub identity: Value,
}

/// /etc/y2linux/versions.json: what the installed build is (written by the build, `status system`'s
/// `versions`). A field of another build's set is kept as it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Versions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rootfs_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_api_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature_contract_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battery_profile_schema: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low_battery_policy_schema: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_schema_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reborn_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_git_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reborn_source_commit: Option<String>,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

/// The `record` block of observations (`Context.record` in common.py). A field a record lacks reads
/// as its default.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ObservationRecord {
    pub wall_timestamp: String,
    pub monotonic_ns: u64,
    pub boot_id: Option<String>,
    pub y2linux_commit: Option<String>,
    pub reborn_commit: Option<String>,
    pub kernel: Option<String>,
    pub rootfs_release: Option<String>,
    pub build_id: Option<String>,
    pub workload: String,
    pub parameters: Map<String, Value>,
    pub units: Map<String, Value>,
    pub result: Value,
    pub failure: Value,
    pub evidence_level: String,
    /// Added by some commands (`collection_duration_ns` for status).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A health check's or the overall health's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum CheckState {
    Ok,
    Degraded,
    Failed,
    Unavailable,
}

/// `y2-platform health` (exit 1 when `state` is FAILED; Reborn reads it as evidence, not an error).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub schema: String,
    pub record: ObservationRecord,
    pub state: CheckState,
    pub checks: Vec<HealthCheck>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HealthCheck {
    pub name: String,
    pub state: CheckState,
    pub reason: Option<String>,
    /// Per-check details (`errors`, `unreadable`, …).
    #[serde(flatten)]
    pub details: Map<String, Value>,
}

/// `y2-platform capabilities`: `/etc/y2linux/capabilities.json` with the running release's identity and
/// the owner's Wi-Fi/Bluetooth switches. Without the file: only `schema`, `state` and `platform_version`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub schema: String,
    pub platform_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_contract_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualification_scope: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<BTreeMap<String, Capability>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capability {
    pub enabled: bool,
    pub experimental: bool,
    pub implemented: bool,
    pub qualified: bool,
    pub reason: Option<String>,
    #[serde(flatten)]
    pub details: Map<String, Value>,
}

/// `y2-platform status [section]`: every section, or `schema`, `record` and the one asked for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub schema: String,
    pub record: ObservationRecord,
    /// The sections (`audio`, `bluetooth`, `cpu`, `memory`, `power`, `readiness`, `storage`, `system`,
    /// `thermal`, `wifi`, `usb` for `status usb`, and `reborn_metrics` with `--reborn`); typed in
    /// phase 2 of the port.
    #[serde(flatten)]
    pub sections: BTreeMap<String, Value>,
}

/// What `main.py` prints for an `OSError`, `ValueError` or `RuntimeError` no command caught (exit 1).
/// `reason` is Python's `str(error)`; Reborn shows `failure`, which this record does not have.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorRecord {
    pub schema: String,
    pub state: String,
    pub reason: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_stage_names_round_trip() {
        for stage in BootStage::ALL {
            let text = serde_json::to_string(&stage).unwrap();
            assert_eq!(text, format!("\"{}\"", stage.as_str()));
            assert_eq!(BootStage::parse(stage.as_str()), Some(stage));
        }
        assert_eq!(BootStage::parse("initramfs"), None);
        assert!(serde_json::from_str::<CheckState>("\"Ok\"").is_err());
        assert_eq!(
            serde_json::from_str::<CheckState>("\"UNAVAILABLE\"").unwrap(),
            CheckState::Unavailable
        );
    }
}
