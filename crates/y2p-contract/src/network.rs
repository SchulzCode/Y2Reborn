//! `/run/y2/network.json`, the Wi-Fi observation the service daemon publishes every pass, as its
//! readers see it: the service daemon itself (the count), the connectivity supervisor (F53),
//! `status wifi` (the fresh signal reading), the update check and download (Online) and Reborn (stale
//! after 15 s, an Online record after 45 s).
// SPDX-License-Identifier: MIT
use crate::lenient::{self, BootId};
use serde::Deserialize;

/// The fields readers of a published record use, read leniently (an odd field reads as absent).
#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct Published {
    #[serde(deserialize_with = "lenient::integer")]
    pub schema: Option<i64>,
    pub boot_id: BootId,
    #[serde(deserialize_with = "lenient::number")]
    pub monotonic_s: Option<f64>,
    #[serde(deserialize_with = "lenient::text")]
    pub state: Option<String>,
    #[serde(deserialize_with = "lenient::text")]
    pub reason: Option<String>,
    #[serde(deserialize_with = "lenient::text")]
    pub association: Option<String>,
    /// The peer the record's addresses belong to.
    #[serde(deserialize_with = "lenient::text")]
    pub association_id: Option<String>,
    /// Its texts (other items left out).
    #[serde(deserialize_with = "lenient::some_texts")]
    pub ip_addresses: Option<Vec<String>>,
    /// The saved network whose password failed (wpa_supplicant's id).
    #[serde(deserialize_with = "lenient::count")]
    pub failed_network_id: Option<u64>,
    #[serde(deserialize_with = "lenient::is_true")]
    pub radio_present: bool,
    #[serde(deserialize_with = "lenient::count")]
    pub status_query_failures: Option<u64>,
}

impl Published {
    /// Whether the record was written this boot.
    pub fn is_this_boot(&self, boot: Option<&str>) -> bool {
        self.boot_id.is(boot)
    }

    /// How old the record is at `now` (CLOCK_MONOTONIC seconds); none without a time.
    pub fn age(&self, now: f64) -> Option<f64> {
        self.monotonic_s.map(|t| now - t)
    }

    /// Whether the record was renewed less than `seconds` before `now` (`0 <= now - monotonic_s <
    /// seconds`).
    pub fn fresh(&self, now: f64, seconds: f64) -> bool {
        self.age(now)
            .is_some_and(|age| (0.0..seconds).contains(&age))
    }

    /// Whether the record says Online.
    pub fn online(&self) -> bool {
        self.state.as_deref() == Some("Online")
    }

    /// Whether the record names `address` among its addresses.
    pub fn has_address(&self, address: &str) -> bool {
        self.ip_addresses
            .as_ref()
            .is_some_and(|addresses| addresses.iter().any(|a| a == address))
    }
}
