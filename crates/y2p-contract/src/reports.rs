//! The live record of a report the owner started (`/run/y2/report-progress.json`, in RAM): written by
//! the platform at most five times a second, read by Reborn's report screen.
// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};

/// A report run's step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportPhase {
    Collect,
    Scrub,
    Pack,
    Upload,
    Save,
}

impl ReportPhase {
    /// The share of the whole run the step covers, in percent.
    pub fn span(self) -> (f64, f64) {
        match self {
            ReportPhase::Collect => (0.0, 12.0),
            ReportPhase::Scrub => (12.0, 76.0),
            ReportPhase::Pack => (76.0, 82.0),
            ReportPhase::Upload | ReportPhase::Save => (82.0, 100.0),
        }
    }
}

/// The record (written with sorted keys): the step, the whole run's percent and an estimate of the
/// seconds left; the bytes an upload or a save has done of its whole.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ReportProgress {
    pub schema: u8,
    pub phase: ReportPhase,
    pub percent: f64,
    pub seconds_left: Option<i64>,
    pub destination: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_done: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_total: Option<u64>,
}
