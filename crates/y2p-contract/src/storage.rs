//! The storage records: the SD card records (`media <action>`'s answer, media.json, our claim on the
//! SD mount media-mount.json, media-lifecycle.json) with the card identity, the read-only reason and
//! the filesystem check, and Y2Media's claim (internal-media.json): written by the platform, read by
//! Reborn's library sources.
// SPDX-License-Identifier: MIT
use crate::lenient::{self, BootId, Shaped};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};

/// Why our SD mount is read-only (`read_only` of the claim and the operation records).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOnly {
    /// `null`: mounted read-write (the owner switch, `sd-always-writable`).
    ReadWrite,
    /// A healthy card between writes: writable only while a write window is open.
    Protected,
    /// Its admission check did not finish in time; `media verify` finishes it (F49).
    Checking,
    /// Its check found errors: Repair (or a computer) makes it writable.
    FilesystemErrors,
    /// Its background check did not finish either: Repair is offered too.
    CheckTimedOut,
}

impl ReadOnly {
    /// The reason a record names; `None` for a value no version of the platform writes.
    pub fn of(value: &Value) -> Option<ReadOnly> {
        Some(match value {
            Value::Null => ReadOnly::ReadWrite,
            Value::String(name) => match name.as_str() {
                "protected" => ReadOnly::Protected,
                "checking" => ReadOnly::Checking,
                "filesystem_errors" => ReadOnly::FilesystemErrors,
                "check_timed_out" => ReadOnly::CheckTimedOut,
                _ => return None,
            },
            _ => return None,
        })
    }

    /// The value a record keeps.
    pub fn value(self) -> Value {
        match self {
            ReadOnly::ReadWrite => Value::Null,
            ReadOnly::Protected => json!("protected"),
            ReadOnly::Checking => json!("checking"),
            ReadOnly::FilesystemErrors => json!("filesystem_errors"),
            ReadOnly::CheckTimedOut => json!("check_timed_out"),
        }
    }

    /// Read-only because the card may be damaged, so Repair is offered.
    pub fn damaged(reason: Option<ReadOnly>) -> bool {
        matches!(
            reason,
            Some(ReadOnly::FilesystemErrors | ReadOnly::CheckTimedOut)
        )
    }
}

/// One SD operation's record: what `media <action>` prints and media.json keeps. The mount's is our
/// claim on the card (media-mount.json): a removed card has no sysfs path any more, and the claim
/// names the exact mount we may still unmount. Reconcile's media-lifecycle.json starts with the
/// inventory it acted on. Read back leniently: a field of another shape reads as missing.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct MediaRecord {
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::or_none"
    )]
    pub schema: Option<u8>,
    /// The SD devices reconcile saw (media-lifecycle.json).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::or_none"
    )]
    pub inventory: Option<Vec<Inserted>>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "attempts")]
    pub attempts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "flag")]
    pub retry: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub action: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub state: Option<String>,
    #[serde(deserialize_with = "lenient::text")]
    pub reason: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::there"
    )]
    pub boot_id: Option<BootId>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub device: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub filesystem: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub uuid: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub cid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<Shaped<Identity>>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub source_instance: Option<String>,
    /// The card's size in sectors (`format`, `grow`).
    #[serde(skip_serializing_if = "Option::is_none", skip_deserializing)]
    pub sectors: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", skip_deserializing)]
    pub errno: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesystem_check: Option<Shaped<Check>>,
    #[serde(skip_serializing_if = "Option::is_none", skip_deserializing)]
    pub repair: Option<Value>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::integer"
    )]
    pub mount_id: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub device_id: Option<String>,
    /// Why our mount is read-only, or a value no version writes (kept as it is).
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "write_read_only",
        deserialize_with = "read_only"
    )]
    pub read_only: Option<Result<ReadOnly, Value>>,
    /// What Reborn reads from a refused command (`media repair`).
    #[serde(skip_serializing_if = "Option::is_none", skip_deserializing)]
    pub failure: Option<String>,
}

/// One SD device reconcile saw: its name, `dev`, disk sequence and CID.
pub type Inserted = (String, Option<String>, Option<String>, Option<String>);

#[allow(clippy::type_complexity)]
fn read_only<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Result<ReadOnly, Value>>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(Some(ReadOnly::of(&value).ok_or(value)))
}

fn write_read_only<S: serde::Serializer>(
    read_only: &Option<Result<ReadOnly, Value>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match read_only {
        Some(Ok(reason)) => reason.serialize(serializer),
        Some(Err(value)) => value.serialize(serializer),
        None => serializer.serialize_none(),
    }
}

/// The attempts so far: a flag counts as 0 or 1, anything else that is no integer as 0.
fn attempts<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    Ok(Some(match Value::deserialize(deserializer)? {
        Value::Bool(flag) => i64::from(flag),
        other => other.as_i64().unwrap_or(0),
    }))
}

fn flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
    lenient::truthy(deserializer).map(Some)
}

impl Serialize for ReadOnly {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value().serialize(serializer)
    }
}

impl MediaRecord {
    pub fn set_state(&mut self, state: &str, reason: Option<&str>) {
        self.state = Some(state.into());
        self.reason = reason.map(str::to_owned);
    }

    pub fn state_is(&self, state: &str) -> bool {
        self.state.as_deref() == Some(state)
    }

    /// The later record over this one: its fields where it has them (`reason` always), this one's
    /// elsewhere.
    pub fn absorb(&mut self, later: MediaRecord) {
        let earlier = std::mem::take(self);
        *self = MediaRecord {
            schema: later.schema.or(earlier.schema),
            inventory: later.inventory.or(earlier.inventory),
            attempts: later.attempts.or(earlier.attempts),
            retry: later.retry.or(earlier.retry),
            action: later.action.or(earlier.action),
            state: later.state.or(earlier.state),
            reason: later.reason,
            boot_id: later.boot_id.or(earlier.boot_id),
            device: later.device.or(earlier.device),
            filesystem: later.filesystem.or(earlier.filesystem),
            uuid: later.uuid.or(earlier.uuid),
            cid: later.cid.or(earlier.cid),
            identity: later.identity.or(earlier.identity),
            source_instance: later.source_instance.or(earlier.source_instance),
            sectors: later.sectors.or(earlier.sectors),
            errno: later.errno.or(earlier.errno),
            filesystem_check: later.filesystem_check.or(earlier.filesystem_check),
            repair: later.repair.or(earlier.repair),
            mount_id: later.mount_id.or(earlier.mount_id),
            device_id: later.device_id.or(earlier.device_id),
            read_only: later.read_only.or(earlier.read_only),
            failure: later.failure.or(earlier.failure),
        };
    }

    /// The claim's read-only reason (a claim without one is read-write); `None` for a value no
    /// version writes.
    pub fn read_only(&self) -> Option<ReadOnly> {
        match &self.read_only {
            None => Some(ReadOnly::ReadWrite),
            Some(recorded) => recorded.as_ref().ok().copied(),
        }
    }

    pub fn set_read_only(&mut self, reason: ReadOnly) {
        self.read_only = Some(Ok(reason));
    }

    /// Made by the boot named `boot` (none: an unreadable id).
    pub fn of_boot(&self, boot: Option<&str>) -> bool {
        self.boot_id.as_ref().unwrap_or(&BootId::Null).is(boot)
    }
}

/// What a filesystem check found (`filesystem_check` of the records). The checker's output is kept
/// only from a run that passed: a failed one can name files.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Check {
    #[serde(deserialize_with = "lenient::is_true")]
    pub ok: bool,
    #[serde(deserialize_with = "lenient::text")]
    pub reason: Option<String>,
    #[serde(deserialize_with = "lenient::text")]
    pub output: Option<String>,
    #[serde(deserialize_with = "lenient::or_default")]
    pub elapsed_ms: u64,
    /// The classes of what it reported: `timeout`, the FAT line classes, or `other`.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::texts"
    )]
    pub findings: Option<Vec<String>>,
    /// What FAT's unclassified lines said, without names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other: Option<Value>,
    /// Why a FAT check that failed passed anyway: `unclean_unmount` (#44) or `benign`.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub finding: Option<String>,
    /// Run by `verify` after the card was admitted read-only.
    #[serde(
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "lenient::is_true"
    )]
    pub background: bool,
}

impl Check {
    /// The checker ran out of time; that is not damage (F49).
    pub fn timed_out(&self) -> bool {
        self.findings.as_ref().is_some_and(|f| f == &["timeout"])
    }
}

/// A card's identity (`identity` of the SD records): what names it across insertions, and `id`, the
/// hash of the rest. Only exactly this shape reads as one.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub cid: Option<String>,
    pub geometry: Geometry,
    pub filesystem: String,
    pub uuid: Option<String>,
    pub id: String,
}

/// Where the filesystem sits on the card (sysfs texts).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Geometry {
    pub partition: Option<String>,
    pub start: Option<String>,
    pub size: Option<String>,
}

/// internal-media.json as a reader checks it: the claim's state and the mount it names, with the
/// types the claim is written in: texts for state, device and boot id, an integer for the mount id. A
/// field of another type matches nothing (and is not shown).
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Y2MediaClaim {
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
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub reason: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub path: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::there"
    )]
    pub boot_id: Option<BootId>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub filesystem: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub label: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub serial: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::integer"
    )]
    pub mount_id: Option<i64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub device_id: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub uuid: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub source_instance: Option<String>,
}

impl Y2MediaClaim {
    /// Whether the claim says Y2Media is ready on this boot's mount `mount_id` of `device_id` (`boot`:
    /// this boot's id, none when it is unreadable).
    pub fn ready_on(&self, boot: Option<&str>, mount_id: i64, device_id: &str) -> bool {
        self.state.as_deref() == Some("Ready")
            && self.boot_id.as_ref().unwrap_or(&BootId::Null).is(boot)
            && self.mount_id == Some(mount_id)
            && self.device_id.as_deref() == Some(device_id)
    }
}

/// This boot's Y2Media setup state as its note says it (counts and the old storage's serial, no file
/// names); `Unknown` without a note of this boot.
#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct SetupStatus {
    #[serde(skip_serializing)]
    pub boot_id: BootId,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub state: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub reason: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::count"
    )]
    pub files: Option<u64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::count"
    )]
    pub bytes: Option<u64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub serial: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::count"
    )]
    pub kept: Option<u64>,
}

impl SetupStatus {
    /// The old storage holds more than can be kept: the listener must confirm.
    pub fn waiting(&self) -> bool {
        self.state.as_deref() == Some("Waiting")
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod storage_tests;
