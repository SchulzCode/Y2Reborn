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
    /// True for voltage-derived SOC; it is not a calibrated fuel gauge.
    pub estimated: bool,
    pub charging: ChargingState,
    pub level: LowBattery,
}

/// Deliberate system sleep is independent of display blanking. The platform
/// owns admission and restoration; Reborn never knows PM register details.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SleepPhase {
    #[default]
    Awake,
    SleepRequested,
    SleepPreparing,
    SleepRefused,
    Sleeping,
    Waking,
    Restored,
    RestoreFailed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SleepProblem {
    #[default]
    Other,
    QualificationPending,
    ChargerActive,
    PlaybackActive,
    Busy,
    DifferentBoot,
    RestorationFailed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WakeReason {
    #[default]
    Unknown,
    Power,
    Rtc,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SleepState {
    pub phase: SleepPhase,
    pub problem: Option<SleepProblem>,
    pub wake: WakeReason,
    pub same_boot: Option<bool>,
}

impl SleepState {
    /// Full sleep was refused for a reason the person cannot act on at this
    /// moment: it is not yet qualified, or the charger is active. Power should
    /// then blank the screen instead of leaving it lit, without claiming the
    /// player slept. Playback, busy and other refusals are unaffected.
    pub fn refused_but_screen_can_blank(&self) -> bool {
        self.phase == SleepPhase::SleepRefused
            && matches!(
                self.problem,
                Some(SleepProblem::QualificationPending | SleepProblem::ChargerActive)
            )
    }
}

/// Seconds the screen must stay dark before Wi-Fi is powered down at runtime.
pub const WIFI_IDLE_OFF_SECS: u64 = 300;

/// Recently paused stays in runtime idle; a dark, untouched player eventually
/// asks the platform for full suspend. A refusal retries at most once a minute.
pub fn automatic_sleep_due(
    screen_off: bool,
    dark_seconds: Option<u64>,
    inactive_seconds: u64,
    since_attempt_seconds: Option<u64>,
    playing: bool,
    busy: bool,
) -> bool {
    screen_off
        && !playing
        && !busy
        && dark_seconds.is_some_and(|seconds| seconds >= 60)
        && inactive_seconds >= 60
        && since_attempt_seconds.is_none_or(|seconds| seconds >= 60)
}

/// A running Wi-Fi radio owns the APDMA and BTIF clocks, which keeps CPU0 out of
/// C2/C3 (measured on battery: no deep entry with Wi-Fi associated, 74% C3 with
/// the radio off). Power it down at runtime, never touching the saved
/// preference, only when the player is genuinely idle on battery.
pub fn wifi_idle_power_down(
    dark_secs: u64,
    wifi_on: bool,
    playing: bool,
    on_battery: bool,
    busy: bool,
) -> bool {
    wifi_on && on_battery && !playing && !busy && dark_secs >= WIFI_IDLE_OFF_SECS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LdacQuality {
    Mobile,
    Standard,
    High,
}

/// AAC encoder quality: "High" uses the FDK afterburner and the highest rate
/// the headphones allow where a frame still fits one packet; "Standard" is
/// BlueALSA's tested 220 kbit/s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AacQuality {
    Standard,
    High,
}
impl AacQuality {
    pub const fn argument(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::High => "high",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard · 220 kbps",
            Self::High => "High · up to 256 kbps",
        }
    }
    pub const fn next(self) -> Self {
        match self {
            Self::Standard => Self::High,
            Self::High => Self::Standard,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SbcQuality {
    High,
    Xq,
    XqPlus,
}
impl SbcQuality {
    pub const fn argument(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Xq => "xq",
            Self::XqPlus => "xq+",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::High => "Standard",
            Self::Xq => "XQ",
            Self::XqPlus => "XQ+",
        }
    }
    pub const fn next(self) -> Self {
        match self {
            Self::High => Self::Xq,
            Self::Xq => Self::XqPlus,
            Self::XqPlus => Self::High,
        }
    }
}
impl LdacQuality {
    pub const fn argument(self) -> &'static str {
        match self {
            Self::Mobile => "mobile",
            Self::Standard => "standard",
            Self::High => "high",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::Mobile => "Connection · 303/330 kbps",
            Self::Standard => "Balanced · 606/660 kbps",
            Self::High => "Quality · 909/990 kbps",
        }
    }
    pub const fn next(self) -> Self {
        match self {
            Self::Mobile => Self::Standard,
            Self::Standard => Self::High,
            Self::High => Self::Mobile,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BluetoothQuality {
    pub sbc_supported: bool,
    pub requested_sbc: Option<SbcQuality>,
    pub effective_sbc: Option<SbcQuality>,
    pub ldac_supported: bool,
    pub abr_supported: bool,
    pub requested_quality: Option<LdacQuality>,
    pub effective_quality: Option<LdacQuality>,
    pub requested_abr: Option<bool>,
    pub effective_abr: Option<bool>,
    pub aac_supported: bool,
    pub requested_aac: Option<AacQuality>,
    pub aac_abr_supported: bool,
    pub requested_aac_abr: Option<bool>,
    pub pending_restart: bool,
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
    Copying,
    /// Advanced network transfer on images without the consumer MTP function.
    Developer,
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
    /// Verified and queued; turning the player off and on installs it.
    ReadyToInstall,
    /// The new system is checking itself after the recovery image installed it.
    Installing,
    /// The previous system is restored by the recovery image at the next power-on.
    Restoring,
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
    /// Installing needs the charger or at least half a battery.
    NeedsCharge,
}

/// A running download, read straight from the platform's progress file in
/// RAM while the download task runs (Y2Linux OTA-48): the task itself only
/// answers when it is finished.
/// Wi-Fi and Bluetooth need MediaTek firmware that no image may carry: the
/// owner copies `system.img` from the Innioasis package to the player once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RadioFirmware {
    /// An older platform without the record, or not yet observed.
    #[default]
    Unknown,
    Ready,
    Missing,
    Installing,
    /// The copied file was not the Innioasis Y2 firmware package.
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DownloadLive {
    /// The download is complete and the platform checks the package.
    pub verifying: bool,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Over the last few seconds; None until there is enough to measure.
    pub speed_bytes_per_s: Option<u64>,
    pub seconds_left: Option<u64>,
    /// How far the check is (0..=100) while verifying.
    pub check_percent: Option<u8>,
}

impl DownloadLive {
    /// 0..=1 of the current step (download, then check).
    pub fn fraction(&self) -> f32 {
        if self.verifying {
            f32::from(self.check_percent.unwrap_or(0).min(100)) / 100.
        } else if self.bytes_total > 0 {
            (self.bytes_done.min(self.bytes_total) as f64 / self.bytes_total as f64) as f32
        } else {
            0.
        }
    }
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
    /// Download progress while the platform stages a release.
    pub download_percent: Option<u8>,
    /// Size of the release the last check found, in bytes.
    pub download_bytes: Option<u64>,
    /// System space a refused download needed, after the platform's cleanup.
    pub space_needed_mb: Option<u64>,
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
    /// The MediaTek radio firmware the owner supplies once (Y2Linux 49+).
    pub radio_firmware: RadioFirmware,
    pub info: PlatformInfo,
    pub storage: StorageState,
    pub usb: UsbTransfer,
    /// Durable file changes, coalesced into incremental scans by the app.
    pub usb_refresh_generation: u64,
    pub sleep: SleepState,
    pub bluetooth_quality: BluetoothQuality,
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

/// The wired output level the CS43131 uses for the load in the jack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadphoneLevel {
    Low,
    Mid,
    High,
}

/// The wired headphone load measurement (cs43130 `y2_hp`, Y2Linux #38).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeadphoneLoad {
    /// The kernel does not offer it.
    #[default]
    Unavailable,
    /// Offered, no result yet.
    Unmeasured,
    Measuring,
    /// DC load per channel in ohms (the board's series resistance already
    /// removed by the driver) and the level the codec chose for it.
    Measured {
        ohms: (u16, u16),
        level: Option<HeadphoneLevel>,
    },
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

    #[test]
    fn wifi_powers_down_only_when_dark_idle_on_battery_and_unoccupied() {
        let long = WIFI_IDLE_OFF_SECS;
        assert!(wifi_idle_power_down(long, true, false, true, false));
        assert!(!wifi_idle_power_down(long - 1, true, false, true, false));
        assert!(!wifi_idle_power_down(long, false, false, true, false));
        assert!(!wifi_idle_power_down(long, true, true, true, false));
        assert!(!wifi_idle_power_down(long, true, false, false, false));
        assert!(!wifi_idle_power_down(long, true, false, true, true));
    }

    #[test]
    fn only_non_actionable_refusals_fall_back_to_screen_off() {
        let refused = |problem| SleepState {
            phase: SleepPhase::SleepRefused,
            problem,
            ..SleepState::default()
        };
        assert!(refused(Some(SleepProblem::QualificationPending)).refused_but_screen_can_blank());
        assert!(refused(Some(SleepProblem::ChargerActive)).refused_but_screen_can_blank());
        for problem in [
            None,
            Some(SleepProblem::PlaybackActive),
            Some(SleepProblem::Busy),
            Some(SleepProblem::Other),
        ] {
            assert!(!refused(problem).refused_but_screen_can_blank());
        }
        let sleeping = SleepState {
            phase: SleepPhase::Sleeping,
            problem: Some(SleepProblem::QualificationPending),
            ..SleepState::default()
        };
        assert!(!sleeping.refused_but_screen_can_blank());
    }

    #[test]
    fn automatic_sleep_waits_for_dark_inactivity_and_retries_after_ownership_ends() {
        assert!(!automatic_sleep_due(true, Some(59), 60, None, false, false));
        assert!(!automatic_sleep_due(true, Some(60), 59, None, false, false));
        assert!(!automatic_sleep_due(
            false,
            Some(600),
            600,
            None,
            false,
            false
        ));
        assert!(!automatic_sleep_due(true, None, 600, None, false, false));
        assert!(!automatic_sleep_due(
            true,
            Some(600),
            600,
            None,
            true,
            false
        ));
        assert!(!automatic_sleep_due(
            true,
            Some(600),
            600,
            None,
            false,
            true
        ));
        assert!(!automatic_sleep_due(
            true,
            Some(600),
            600,
            Some(59),
            false,
            false
        ));
        assert!(automatic_sleep_due(
            true,
            Some(600),
            600,
            Some(60),
            false,
            false
        ));
        assert!(automatic_sleep_due(true, Some(60), 60, None, false, false));
    }
}
