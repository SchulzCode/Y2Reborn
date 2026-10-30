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
    /// A charger or computer is connected: the battery charges, is full or is held ("Not
    /// charging", which a full battery passes through on its cable and the UI shows as on battery).
    pub cable: bool,
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

/// The wait before the next automatic sleep after `refusals` sleeps in a row
/// were refused once Reborn had prepared (Y2Linux F47: a UART that never
/// let the kernel sleep was retried every minute for hours). Each try quiets
/// the radios for seconds, so the wait doubles up to half an hour.
pub fn sleep_retry_seconds(refusals: u32) -> u64 {
    (60u64 << refusals.min(5)).min(30 * 60)
}

/// After the platform refused a sleep because a charger or computer is
/// connected, automatic sleep waits while the battery still shows a cable
/// (charging, full or held): the platform would refuse each try the same way,
/// and every try starts a platform process (once a minute for as long as the
/// player lies on its cable). Unplugged or unknown, the normal tries resume.
/// Power still asks at once.
pub fn charger_holds_sleep(refused_for_charger: bool, cable: bool) -> bool {
    refused_for_charger && cable
}

/// With Bluetooth headphones connected, a pause keeps the player awake this
/// long so the headphones' own play button can continue (Beta 8.18): asleep,
/// the radio is off and nothing hears the button.
pub const HEADSET_PAUSE_AWAKE_SECS: u64 = 15 * 60;

/// The pause with headphones connected (seconds since it began, if one is
/// going on) still holds off automatic sleep.
pub fn headset_pause_awake(paused_seconds: Option<u64>) -> bool {
    paused_seconds.is_some_and(|seconds| seconds < HEADSET_PAUSE_AWAKE_SECS)
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
    /// Inserted, but its filesystem check found damage, so it was not mounted
    /// (a repair on a computer fixes it). A card only pulled out while in use
    /// is not this: it is mounted.
    NeedsRepair,
    /// Inserted, but its filesystem check did not finish in time (a slow or
    /// very full card): not damage, and inserting it again checks it again.
    CheckTimedOut,
    /// Mounted read-only because its check found errors (Y2Linux 8.27): the
    /// music plays, nothing is written to it; Repair makes it writable again.
    ReadOnly(VolumeSpace),
    /// Mounted read-only because its check did not finish in time.
    Unchecked(VolumeSpace),
    Ready(VolumeSpace),
}

impl SdCard {
    /// The player can repair it (Settings → Storage → Repair SD Card).
    pub fn repairable(&self) -> bool {
        matches!(
            self,
            Self::ReadOnly(_) | Self::Unchecked(_) | Self::NeedsRepair
        )
    }
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

/// The step of a report run the owner started: collecting the logs, taking
/// the personal details out (and packing), then sending or saving it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportStep {
    Collect,
    Clean,
    Finish,
}

/// Live progress of *Send Report Now* or *Save Report* (Y2Linux
/// `/run/y2/report-progress.json`): the whole run's percent and its step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReportLive {
    pub step: ReportStep,
    /// 0..=100 of the whole run.
    pub percent: f32,
    pub seconds_left: Option<u64>,
    /// While sending: how much of the report went out.
    pub bytes_done: Option<u64>,
    pub bytes_total: Option<u64>,
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
    /// The system's build date (unix seconds): the clock is never set
    /// earlier than a day before it, nor more than five years after.
    pub build_epoch: Option<i64>,
    /// Where the clock came from: "ntp", "manual", "retained_rtc", "build_floor".
    pub clock_source: Option<String>,
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

/// Turning the stock player's storage into Internal Storage (Y2Linux STORAGE-SETUP): the
/// platform does it by itself at start unless the old storage holds a real library.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InternalSetup {
    /// An older platform, or not observed this boot.
    #[default]
    Unknown,
    NotNeeded,
    /// Waits for a later start (an update still being checked, the battery).
    Pending(SetupWait),
    /// The old storage holds more than can be kept: the listener decides (it is erased).
    Waiting {
        files: u32,
        bytes: u64,
    },
    Running,
    /// Set up during this start; `kept` old files are in "Old Player Files".
    Done {
        kept: u32,
    },
    Failed,
}

/// Why the setup waits: a restart alone does not help a low battery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupWait {
    /// Under 25 % on battery at start: it runs at a start on a charger.
    Battery,
    /// An update's health check, or anything else a later start clears.
    Restart,
}

/// Everything Reborn knows about the platform from its last observation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlatformSnapshot {
    /// False until a successful observation of the current boot.
    pub observed: bool,
    /// The MediaTek radio firmware the owner supplies once (Y2Linux 49+).
    pub radio_firmware: RadioFirmware,
    /// The copy goes into the Firmware folder on Internal Storage (else the Firmware drive).
    pub firmware_in_internal: bool,
    /// The last copy was refused: not enough space where it was copied to.
    pub firmware_no_space: bool,
    pub internal_setup: InternalSetup,
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
    /// Opt-in error reports (`y2-platform report status`); None on a platform
    /// without them.
    pub reports: Option<ReportState>,
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
    fn a_charger_refusal_holds_automatic_sleep_only_while_a_cable_shows() {
        assert!(charger_holds_sleep(true, true));
        assert!(!charger_holds_sleep(true, false));
        assert!(!charger_holds_sleep(false, true));
        assert!(!charger_holds_sleep(false, false));
    }
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
        // Refused sleeps in a row: 1, 2, 4, 8, 16, then 30 minutes apart.
        let waits: Vec<u64> = (0..8).map(sleep_retry_seconds).collect();
        assert_eq!(waits, [60, 120, 240, 480, 960, 1800, 1800, 1800]);
        assert_eq!(sleep_retry_seconds(u32::MAX), 1800);
        // Headphones connected: a pause stays awake for 15 minutes, then sleeps.
        assert!(headset_pause_awake(Some(0)));
        assert!(headset_pause_awake(Some(HEADSET_PAUSE_AWAKE_SECS - 1)));
        assert!(!headset_pause_awake(Some(HEADSET_PAUSE_AWAKE_SECS)));
        assert!(!headset_pause_awake(None));
    }
}

/// Opt-in error reports (Y2Linux docs/planning/ERROR-REPORTS-PLAN.md).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportState {
    /// None: the owner has not been asked yet.
    pub enabled: Option<bool>,
    /// Random, made on the player; links reports from one player.
    pub install_id: Option<String>,
    /// Reports waiting to be sent.
    pub queued: usize,
    pub last: Option<ReportLast>,
    /// The platform can upload reports; until it can, the owner saves them to
    /// storage and sends the file, and the switch and question stay hidden.
    pub upload: bool,
    /// Storage a report can be saved to now (mounted writable).
    pub destinations: Vec<ReportDestination>,
}
impl ReportState {
    /// The install ID as shown: its first eight characters, upper case.
    pub fn short_id(&self) -> Option<String> {
        self.install_id.as_deref().map(|id| {
            id.chars()
                .filter(|c| *c != '-')
                .take(8)
                .collect::<String>()
                .to_uppercase()
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportLast {
    pub report_id: String,
    /// "manual" or the automatic trigger (kernel_crash, reborn_crash, …).
    pub trigger: String,
    /// queued, sent, failed (retried), dropped (refused or unsent after three days) or saved.
    pub state: String,
    pub created_utc: String,
    pub bytes: u64,
    /// Where a saved report went.
    pub destination: Option<ReportDestination>,
}

/// Where *Save Report* writes the file (its Error Reports folder, seen over MTP).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportDestination {
    Internal,
    Sd,
}
impl ReportDestination {
    pub const fn argument(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Sd => "sd",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::Internal => "Internal Storage",
            Self::Sd => "SD Card",
        }
    }
    pub fn from_argument(value: &str) -> Option<Self> {
        [Self::Internal, Self::Sd]
            .into_iter()
            .find(|d| d.argument() == value)
    }
}
