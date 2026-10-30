//! The radio firmware's state (`firmware status|scan|install`, `status`'s `system.radio_firmware`):
//! written by the platform, read by Reborn's Wi-Fi and Bluetooth pages.
// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};

/// What `firmware status|scan|install` answers: the state, why, and for a missing copy where it goes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FirmwareStatus {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

impl FirmwareStatus {
    pub fn of(state: &str, reason: Option<String>) -> FirmwareStatus {
        FirmwareStatus {
            state: state.into(),
            reason,
            folder: None,
            file: None,
            location: None,
        }
    }
}
