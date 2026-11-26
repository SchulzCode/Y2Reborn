//! The USB responder's receipt (`/run/y2/usb-transfer.json`, `status`'s `system.usb.file_transfer`) as
//! its readers see it: the storage group's media hand-off and Reborn's USB screen and library refresh.
// SPDX-License-Identifier: MIT
use crate::lenient::{self, BootId};
use crate::schema;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// The receipt's fields, each read leniently (an odd field reads as absent).
#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct Receipt {
    #[serde(deserialize_with = "lenient::text")]
    pub schema: Option<String>,
    pub boot_id: BootId,
    /// Starting, Ready, Copying, Failed or Unavailable.
    #[serde(deserialize_with = "lenient::text")]
    pub state: Option<String>,
    #[serde(deserialize_with = "lenient::text")]
    pub reason: Option<String>,
    /// `mtp` or `developer`.
    #[serde(deserialize_with = "lenient::text")]
    pub mode: Option<String>,
    /// The controller's state (`not attached`, `configured`, …).
    #[serde(deserialize_with = "lenient::text")]
    pub cable: Option<String>,
    /// Counts the copies that changed the library this boot.
    #[serde(deserialize_with = "lenient::count")]
    pub refresh_generation: Option<u64>,
    /// A copy is in progress.
    #[serde(deserialize_with = "lenient::is_true")]
    pub active: bool,
    /// The responder's last copy error (an errno: 28 for a copy that did not fit).
    #[serde(deserialize_with = "lenient::integer")]
    pub error: Option<i64>,
    /// What the computer sees; none for views that cannot be read.
    #[serde(deserialize_with = "views")]
    pub views: Option<Vec<View>>,
    /// The SD card as a USB drive (none: a platform without it).
    #[serde(deserialize_with = "sd_disk")]
    pub sd_disk: Option<SdDisk>,
}

/// `sd_disk`: the owner's switch and what became of the card in this connection.
#[derive(Deserialize, Debug, Default, PartialEq)]
#[serde(default)]
pub struct SdDisk {
    /// The switch (Settings → Storage → SD Card as a USB Stick).
    #[serde(deserialize_with = "lenient::is_true")]
    pub enabled: bool,
    /// Off, Idle, Lending, Lent or Kept.
    #[serde(deserialize_with = "lenient::text")]
    pub state: Option<String>,
    /// Why the card is not lent (`sd_in_use`, `computer_ejected`, `not_fat`, …).
    #[serde(deserialize_with = "lenient::text")]
    pub reason: Option<String>,
}

impl SdDisk {
    /// The computer has the card, or is about to: the player leaves it alone.
    pub fn on_computer(&self) -> bool {
        matches!(self.state.as_deref(), Some("Lending" | "Lent"))
    }
}

impl Default for Receipt {
    /// No receipt: nothing shown to the computer.
    fn default() -> Self {
        Receipt {
            schema: None,
            boot_id: BootId::default(),
            state: None,
            reason: None,
            mode: None,
            cable: None,
            refresh_generation: None,
            active: false,
            error: None,
            views: Some(Vec::new()),
            sd_disk: None,
        }
    }
}

/// An object reads as `SdDisk` (its odd fields as their defaults); anything else is none.
fn sd_disk<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<SdDisk>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        object @ Value::Object(_) => Some(SdDisk::deserialize(object).unwrap_or_default()),
        _ => None,
    })
}

/// One item of `views`: a view's path, or something that is no view.
#[derive(Debug, PartialEq)]
pub enum View {
    Path(Option<String>),
    Invalid,
}

/// A list of `{"path": …}` objects; an empty text or object holds no views.
fn views<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Vec<View>>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::Array(items) => Some(
            items
                .into_iter()
                .map(|item| match item {
                    Value::Object(view) => match view.get("path") {
                        Some(Value::String(path)) => View::Path(Some(path.clone())),
                        _ => View::Path(None),
                    },
                    _ => View::Invalid,
                })
                .collect(),
        ),
        Value::String(text) if text.is_empty() => Some(Vec::new()),
        Value::Object(map) if map.is_empty() => Some(Vec::new()),
        _ => None,
    })
}

impl Receipt {
    /// A receipt of this schema written by the boot `boot` (none: an unreadable id).
    pub fn of_boot(&self, boot: Option<&str>) -> bool {
        self.schema.as_deref() == Some(schema::USB_TRANSFER) && self.boot_id.is(boot)
    }

    /// Whether the computer is shown `path` (read up to the first match); none when the views cannot
    /// be read (the computer might still hold that medium).
    pub fn shows(&self, path: &str) -> Option<bool> {
        for view in self.views.as_ref()? {
            match view {
                View::Path(Some(shown)) if shown == path => return Some(true),
                View::Path(_) => {}
                View::Invalid => return None,
            }
        }
        Some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(value: Value) -> Receipt {
        Receipt::deserialize(&value).unwrap()
    }

    #[test]
    fn views_read_as_paths_and_odd_views_refuse() {
        assert_eq!(Receipt::default().shows("/media/sd"), Some(false));
        assert_eq!(read(json!({})).shows("/media/sd"), Some(false));
        assert_eq!(read(json!({"views": ""})).shows("/media/sd"), Some(false));
        let shown = read(json!({"views": [{"path": "/media/internal"}, {"path": "/media/sd"}]}));
        assert_eq!(shown.shows("/media/sd"), Some(true));
        assert_eq!(read(json!({"views": [3]})).shows("/media/sd"), None);
        assert_eq!(read(json!({"views": 3})).shows("/media/sd"), None);
    }

    #[test]
    fn the_sd_disk_part_reads_leniently() {
        assert_eq!(read(json!({})).sd_disk, None);
        let lent = read(json!({"sd_disk": {"enabled": true, "state": "Lent"}}));
        assert!(lent
            .sd_disk
            .as_ref()
            .is_some_and(|d| d.enabled && d.on_computer()));
        let odd = read(json!({"sd_disk": {"enabled": "yes", "state": 3}})).sd_disk;
        assert_eq!(odd, Some(SdDisk::default()));
        assert_eq!(read(json!({"sd_disk": 3})).sd_disk, None);
        let kept = read(json!({"sd_disk": {"enabled": true, "state": "Kept"}}));
        assert!(!kept.sd_disk.is_some_and(|d| d.on_computer()));
    }

    #[test]
    fn a_receipt_counts_for_its_own_boot_only() {
        let receipt = read(json!({"schema": schema::USB_TRANSFER, "boot_id": "one"}));
        assert!(receipt.of_boot(Some("one")) && !receipt.of_boot(Some("two")));
        assert!(!read(json!({"schema": "other", "boot_id": "one"})).of_boot(Some("one")));
    }
}
