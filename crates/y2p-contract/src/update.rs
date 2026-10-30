//! The update records (`update status`, `status`'s `system.update`, download.json, last-check.json and
//! the live download record): written by the platform, read by Reborn's Software Update page.
// SPDX-License-Identifier: MIT
use crate::lenient;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `update status`: the core's journal (Idle on a ready Y2DATA without one, Unavailable without a
/// ready Y2DATA), with the download and check records and the channel's name.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Status {
    /// The core's journal (the platform's stand-in without one).
    #[serde(flatten)]
    pub journal: CoreJournal,
    /// What a readable journal is shown with.
    #[serde(flatten)]
    pub records: Option<Records>,
}

impl Status {
    /// The journal's failure text (none without one).
    pub fn failure(&self) -> Option<&str> {
        self.journal.failure()
    }

    /// The download record shown with the journal.
    pub fn download(&self) -> Option<&Download> {
        self.records.as_ref()?.download.as_ref()
    }
}

/// The records `update status` shows next to a readable journal (each may be missing to a reader).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Records {
    pub download: Option<Download>,
    pub last_check: Option<LastCheck>,
    pub channel: Option<String>,
    pub full_system: bool,
    pub physical_qualification: bool,
}

/// The native update core's journal (`/data/updates/state.json`, written by y2-update-core): the
/// fields the platform and Reborn read in their types (a field of another type is not shown); its
/// install records (backup, timings, outcome, …) as the core wrote them.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct CoreJournal {
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::integer"
    )]
    pub schema: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub state: Option<String>,
    /// `null` without a failure.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "failure")]
    failure: Option<Option<String>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::integer"
    )]
    pub sequence: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::integer"
    )]
    pub previous_sequence: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub release_version: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::or_none"
    )]
    pub rollback_allowed: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub y2linux_commit: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub manifest_sha256: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub kept_manifest_sha256: Option<String>,
    /// The core's install records, as it wrote them.
    #[serde(flatten)]
    pub records: Map<String, Value>,
}

fn failure<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    lenient::text(deserializer).map(Some)
}

impl CoreJournal {
    /// The platform's stand-in for a journal (no update yet, an unreadable one, no Y2DATA).
    pub fn stand_in(state: &str, failure: Option<&str>) -> CoreJournal {
        CoreJournal {
            schema: Some(1),
            state: Some(state.into()),
            failure: Some(failure.map(str::to_owned)),
            ..CoreJournal::default()
        }
    }

    /// The failure text (none without one).
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_ref().and_then(Option::as_deref)
    }

    /// Whether the core is at rest: a new check, download or restore may start.
    pub fn terminal(&self) -> bool {
        matches!(
            self.state.as_deref(),
            Some("Idle" | "Acknowledged" | "RolledBack" | "Failed")
        )
    }
}

/// download.json, what the Software Update page shows (written sorted; a field is left out where
/// its state has none). Read back by `update status`, a later check and a failed check (which keeps
/// what the page offered). The state is text: a record of another version keeps its state.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct Download {
    pub schema: u8,
    /// Checking, Checked, Staging, Verified or Failed.
    pub state: String,
    /// The token of the check or download that runs (Checking, Staging).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub up_to_date: Option<bool>,
    /// Up to date: whether the channel has a release at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<bool>,
    /// As the signed manifest names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_version: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reborn_version: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_budget: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleaned_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needed_bytes: Option<i64>,
    /// A check that failed after one had offered a release: why (the offer stays).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_check_failure: Option<String>,
}

impl Download {
    pub fn state(state: &str) -> Download {
        Download {
            schema: 1,
            state: state.to_owned(),
            ..Download::default()
        }
    }

    /// A download or check that ended without its own record.
    pub fn failed(failure: &str) -> Download {
        Download {
            failure: Some(failure.into()),
            ..Download::state("Failed")
        }
    }

    pub fn is(&self, state: &str) -> bool {
        self.state == state
    }

    /// The release on offer, when the manifest names it as text.
    pub fn release(&self) -> Option<&str> {
        self.release_version.as_ref()?.as_str()
    }
}

/// last-check.json: when a check last reached the server (`time`, only a successful check) and when
/// one was last tried (`attempt`), with the last check's result. Each field reads leniently.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct LastCheck {
    #[serde(deserialize_with = "lenient::or_none")]
    pub schema: Option<i64>,
    #[serde(
        deserialize_with = "lenient::or_none",
        skip_serializing_if = "Option::is_none"
    )]
    pub attempt: Option<f64>,
    #[serde(
        deserialize_with = "lenient::or_none",
        skip_serializing_if = "Option::is_none"
    )]
    pub time: Option<f64>,
    #[serde(
        deserialize_with = "lenient::text",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<String>,
    /// The release the check found, as the signed manifest names it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_version: Option<Value>,
    #[serde(
        deserialize_with = "lenient::text",
        skip_serializing_if = "Option::is_none"
    )]
    pub failure: Option<String>,
}

impl LastCheck {
    /// When a check last reached the server (seconds since the epoch).
    pub fn checked(&self) -> Option<f64> {
        self.time
    }

    /// When a check was last tried.
    pub fn attempted(&self) -> Option<f64> {
        self.attempt
    }
}

/// The step a download is in (`phase` of the live record); the last record of one says how it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadPhase {
    Download,
    Verify,
    Done,
    Failed,
}

/// The live download record (`/run/y2/update-progress.json`, in RAM): rewritten at most four times a
/// second while a download runs, read by Reborn without asking the busy platform worker.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DownloadProgress {
    pub schema: u8,
    pub phase: DownloadPhase,
    #[serde(default)]
    pub release_version: Value,
    pub bytes_done: i64,
    pub bytes_total: i64,
    /// Over the last seconds; none until half a second is measured.
    pub speed_bytes_per_s: Option<i64>,
    /// Downloading at a known speed only.
    pub seconds_left: Option<i64>,
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod update_tests;
