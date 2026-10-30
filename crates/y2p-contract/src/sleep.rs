//! The product sleep state (`y2-platform sleep status`, the receipt's `product`, `status`'s
//! `power.sleep`): written by the platform, read by Reborn.
// SPDX-License-Identifier: MIT
use crate::lenient;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// What woke the player (a same-boot Power or RTC interrupt delta; both: `multiple`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeSource {
    Power,
    Rtc,
    Multiple,
    Unknown,
}

/// The product sleep state (`product.state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductState {
    Idle,
    Requested,
    Preparing,
    Refused,
    Sleeping,
    Restoring,
    Waking,
    Restored,
    RestoreFailed,
}

/// The product state a receipt keeps (`product`) and `sleep status` prints: proven only by the same
/// boot's kernel journal and Reborn's acknowledgement. Read leniently: a state no version writes is
/// none (the receipt then reads as idle); fields of other versions are kept.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Product {
    #[serde(deserialize_with = "lenient::or_none")]
    pub state: Option<ProductState>,
    #[serde(deserialize_with = "lenient::text")]
    pub reason: Option<String>,
    #[serde(deserialize_with = "lenient::text")]
    pub request_boot_id: Option<String>,
    #[serde(deserialize_with = "lenient::or_none")]
    pub same_boot: Option<bool>,
    #[serde(deserialize_with = "lenient::is_true")]
    pub kernel_completed: bool,
    #[serde(deserialize_with = "lenient::or_none")]
    pub wake_reason: Option<WakeSource>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::text"
    )]
    pub schema: Option<String>,
    /// The boot that read it (`sleep status`).
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient::there"
    )]
    pub boot_id: Option<Option<String>>,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl Product {
    /// The product of a receipt that holds no known state.
    pub fn idle() -> Product {
        Product {
            state: Some(ProductState::Idle),
            wake_reason: Some(WakeSource::Unknown),
            ..Product::default()
        }
    }

    pub fn state_is(&self, state: ProductState) -> bool {
        self.state == Some(state)
    }

    /// Set the state and its reason.
    pub fn set(&mut self, state: ProductState, reason: Option<String>) {
        self.state = Some(state);
        self.reason = reason;
    }
}
