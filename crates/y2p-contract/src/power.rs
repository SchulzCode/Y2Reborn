//! The power daemon's records: the battery reading and low-battery state (`/run/y2/power.json`, the
//! power journal) and the shutdown intent (`/run/y2/shutdown.json`, the power socket's answers):
//! written by the platform, read by Reborn.
// SPDX-License-Identifier: MIT
use crate::lenient::{self, BootId, Shaped};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where a sample's percentage comes from (`battery.source`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatterySource {
    Unavailable,
    FuelGauge,
    Hybrid,
    StockOam,
    VoltageEstimate,
}

/// How far a sample's percentage can be trusted (`battery.confidence`): the low-battery policy acts
/// on a percentage only from `hardware` or `calibrated_estimate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Unavailable,
    Hardware,
    CalibratedEstimate,
    Provisional,
}

/// What one sample publishes (`battery` in power.json and in the journal rows), in this key order:
/// the reading, then for a voltage estimate the estimate's own fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BatteryStatus {
    pub percent: Option<i64>,
    pub source: BatterySource,
    pub confidence: Confidence,
    pub status: Option<String>,
    pub charging: bool,
    pub voltage_uv: Option<i64>,
    pub current_ua: Option<i64>,
    pub temperature_millicelsius: Option<i64>,
    pub charge_counter_uah: Option<i64>,
    pub provisional: bool,
    pub resting: bool,
    #[serde(flatten)]
    pub estimate: Option<Estimate>,
}

impl Default for BatteryStatus {
    /// No usable reading: no percentage, nothing measured.
    fn default() -> Self {
        Self {
            percent: None,
            source: BatterySource::Unavailable,
            confidence: Confidence::Unavailable,
            status: None,
            charging: false,
            voltage_uv: None,
            current_ua: None,
            temperature_millicelsius: None,
            charge_counter_uah: None,
            provisional: true,
            resting: false,
            estimate: None,
        }
    }
}

/// The voltage estimate's fields of a [`BatteryStatus`]; the capacity pair only with a design capacity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub design_capacity_uah: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_uah_estimate: Option<i64>,
    pub estimate_percent: i64,
    pub anchor_percent: Option<f64>,
    pub full_latched: bool,
    pub filtered_voltage_uv: i64,
    pub start: Option<String>,
    pub calibration_source: String,
}

/// The low-battery state (power.json `state`, the journal rows): ShutdownPending after
/// `critical_samples` consecutive critical samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatteryState {
    /// No policy, or no battery reading.
    #[default]
    Unavailable,
    Normal,
    Low,
    Critical,
    ShutdownPending,
}

impl BatteryState {
    /// The text records carry.
    pub fn as_str(self) -> &'static str {
        match self {
            BatteryState::Unavailable => "Unavailable",
            BatteryState::Normal => "Normal",
            BatteryState::Low => "Low",
            BatteryState::Critical => "Critical",
            BatteryState::ShutdownPending => "ShutdownPending",
        }
    }
}

impl PartialEq<&str> for BatteryState {
    fn eq(&self, text: &&str) -> bool {
        self.as_str() == *text
    }
}

/// `/run/y2/power.json` as the power daemon writes it (written sorted, renewed at least every 2 s;
/// Reborn refuses one 5 s old): read back for the status snapshot and by Reborn.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct PowerRecord {
    pub schema: i64,
    pub boot_id: Option<String>,
    pub monotonic_ns: u64,
    pub state: BatteryState,
    pub voltage_uv: Option<i64>,
    pub reason: Option<String>,
    pub thresholds_enabled: bool,
    pub battery: BatteryStatus,
    /// The policy document's `qualification_reference` (a text).
    pub qualification_reference: Option<String>,
    /// The shutdown intent the daemon coordinates (its own record).
    pub shutdown: Option<Intent>,
}

/// A shutdown intent's state (`state` of shutdown.json and the socket's answers).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntentState {
    /// Requested; its grace or Reborn's acknowledgement ends the wait.
    ShutdownPending,
    /// Its steps run (a daemon restart resumes it as pending, with its deadline).
    Executing,
    /// Init refused the transition.
    Failed,
}

/// What a shutdown does (`action`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShutdownAction {
    Poweroff,
    Reboot,
}

impl ShutdownAction {
    pub fn as_str(self) -> &'static str {
        match self {
            ShutdownAction::Poweroff => "poweroff",
            ShutdownAction::Reboot => "reboot",
        }
    }

    /// The action a request names (only these exact texts).
    pub fn named(text: &str) -> Option<ShutdownAction> {
        Some(match text {
            "poweroff" => ShutdownAction::Poweroff,
            "reboot" => ShutdownAction::Reboot,
            _ => return None,
        })
    }
}

/// A shutdown intent (`/run/y2/shutdown.json`, the socket's answers, power.json's `shutdown`): one the
/// coordinator built, or one resumed from shutdown.json after a daemon restart. Resuming needs the
/// fields the coordinator decides on in their types, `client` and `acknowledgement` present; the
/// others read leniently.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Intent {
    #[serde(default, deserialize_with = "lenient::integer")]
    pub schema: Option<i64>,
    pub id: String,
    #[serde(default)]
    pub boot_id: BootId,
    pub state: IntentState,
    pub action: ShutdownAction,
    #[serde(default, deserialize_with = "lenient::text")]
    pub reason: Option<String>,
    #[serde(default, deserialize_with = "lenient::integer")]
    pub requested_monotonic_ns: Option<i64>,
    pub deadline_monotonic_ns: i64,
    /// Reborn's process when the intent was made (`null` without one).
    #[serde(deserialize_with = "lenient::or_none")]
    pub client: Option<ProcessIdentity>,
    /// Reborn's answer (`null` until it answers; what a damaged file holds is kept as it is).
    #[serde(deserialize_with = "acknowledgement")]
    pub acknowledgement: Option<Shaped<Acknowledgement>>,
    #[serde(default, deserialize_with = "lenient::text")]
    pub failure: Option<String>,
    /// What each shutdown step did, once they ran.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::or_none"
    )]
    pub stop_results: Option<std::collections::BTreeMap<String, Step>>,
}

/// Reborn's acknowledgement of an intent.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Acknowledgement {
    /// `Ready` or `Failed` (an acknowledgement without one is a damaged file's).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::there"
    )]
    pub outcome: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::integer"
    )]
    pub monotonic_ns: Option<i64>,
}

fn acknowledgement<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Shaped<Acknowledgement>>, D::Error> {
    Option::deserialize(deserializer)
}

impl Intent {
    pub fn pending(&self) -> bool {
        self.state == IntentState::ShutdownPending
    }

    /// The deadline in CLOCK_MONOTONIC ns.
    pub fn deadline_ns(&self) -> i128 {
        i128::from(self.deadline_monotonic_ns)
    }

    /// Whether Reborn acknowledged with `Ready`. Anything but an acknowledgement with that outcome
    /// (none, or what a damaged resumed intent holds) is no acknowledgement: the deadline still ends
    /// the grace.
    pub fn ready(&self) -> bool {
        matches!(&self.acknowledgement, Some(Shaped::Is(ack))
            if ack.outcome.as_ref().and_then(Value::as_str) == Some("Ready"))
    }

    /// An acknowledgement that is set but is no object with an outcome (only a damaged file has
    /// one).
    pub fn acknowledgement_odd(&self) -> bool {
        match &self.acknowledgement {
            Some(Shaped::Is(ack)) => ack.outcome.is_none(),
            Some(Shaped::Odd(ack)) => lenient::is_truthy(ack),
            None => false,
        }
    }
}

/// A step's `{ok, reason}` (its output is never kept).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub ok: bool,
    pub reason: Option<String>,
}

/// A live process told apart from a later one with its pid: its start time in clock ticks.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: i64,
    pub start_ticks: i64,
}
