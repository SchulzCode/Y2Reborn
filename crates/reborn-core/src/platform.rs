//! Typed product state for the Y2 platform.
//!
//! `reborn-platform` is the only crate that reads Y2Linux files, sockets or
//! command output. It projects them into these models; the UI presents them.
//! Nothing here performs I/O, and no raw platform JSON crosses this boundary.

/// What a normal user is told about the battery.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatteryState {
    /// Only set when the platform publishes a valid state of charge.
    pub percent: Option<u8>,
    pub charging: ChargingState,
    pub level: LowBattery,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChargingState {
    #[default]
    Unknown,
    OnBattery,
    Charging,
    Full,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LowBattery {
    #[default]
    Normal,
    Low,
    Critical,
    ShuttingDown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VolumeState {
    #[default]
    Unknown,
    Ready,
    LowSpace,
    AlmostFull,
    ReadOnly,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VolumeSpace {
    pub state: VolumeState,
    pub free_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SdCard {
    #[default]
    Unknown,
    Absent,
    /// Inserted but not usable: unsupported filesystem, damaged, or not mounted.
    Error,
    Ready(VolumeSpace),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StorageState {
    pub internal: VolumeSpace,
    pub sd: SdCard,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsbTransfer {
    #[default]
    Unknown,
    /// No computer attached.
    Disconnected,
    /// Cable attached, transfer service still starting.
    Starting,
    Ready,
    /// A computer is attached but the transfer service failed.
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdatePhase {
    #[default]
    Unknown,
    /// Installed software is current as far as the last check could tell.
    UpToDate,
    Checking,
    Available {
        version: String,
    },
    Downloading,
    /// Verified and queued; a restart installs it.
    ReadyToInstall,
    Installing,
    /// The last operation failed; the installed system is unchanged.
    Failed(UpdateProblem),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateProblem {
    #[default]
    Other,
    NeedsNetwork,
    NoUpdateSource,
    NotEnoughSpace,
    VerificationFailed,
    NeedsComputer,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateState {
    pub phase: UpdatePhase,
    pub can_check: bool,
    pub can_download: bool,
    pub can_install: bool,
    pub can_cancel: bool,
    /// A verified previous system root can be restored (Diagnostics only).
    pub can_rollback: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HealthLevel {
    #[default]
    Unknown,
    Ok,
    Degraded,
    Failed,
}

/// Normal-user Wi-Fi problems; supplicant states never leave the platform crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiProblem {
    WrongPassword,
    NoAddress,
    NoInternetNames,
    NetworkNotFound,
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlatformInfo {
    pub release_version: Option<String>,
}

/// One labelled diagnostic observation. Values are already human-readable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub label: String,
    pub value: String,
}
impl Fact {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// A Diagnostics page. Sections are produced by the platform client from the
/// versioned status contract; the UI lists them without interpreting them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticSection {
    pub id: &'static str,
    pub title: &'static str,
    pub facts: Vec<Fact>,
}

/// Everything Reborn knows about the platform from its last observation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlatformSnapshot {
    /// False until a successful observation of the current boot.
    pub observed: bool,
    pub info: PlatformInfo,
    pub storage: StorageState,
    pub usb: UsbTransfer,
    pub update: UpdateState,
    pub health: HealthLevel,
    /// Capability keys whose platform flag is `enabled`.
    pub enabled: Vec<String>,
    pub diagnostics: Vec<DiagnosticSection>,
}
impl PlatformSnapshot {
    pub fn enabled(&self, capability: &str) -> bool {
        self.enabled.iter().any(|c| c == capability)
    }
    pub fn section(&self, id: &str) -> Option<&DiagnosticSection> {
        self.diagnostics.iter().find(|s| s.id == id)
    }
}

/// A platform shutdown or restart the Y2Linux power coordinator announced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShutdownIntent {
    pub id: String,
    pub restart: bool,
    pub low_battery: bool,
}

/// Result of an explicit platform operation, already reduced to facts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperationResult {
    pub succeeded: bool,
    pub facts: Vec<Fact>,
}

pub fn human_bytes(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.;
    const MB: f64 = 1_000_000.;
    let b = bytes as f64;
    if b >= 100. * GB {
        format!("{:.0} GB", b / GB)
    } else if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.0} MB", b / MB)
    } else {
        format!("{} KB", bytes / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bytes_use_decimal_units_like_computers_show_for_cards() {
        assert_eq!(human_bytes(3_221_225_472), "3.2 GB");
        assert_eq!(human_bytes(128_000_000_000), "128 GB");
        assert_eq!(human_bytes(42_000_000), "42 MB");
        assert_eq!(human_bytes(900), "0 KB");
    }
}
