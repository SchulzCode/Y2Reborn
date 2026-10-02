//! Explicit, bounded codec policy. Negotiated PCM remains a separate observation.
#![forbid(unsafe_code)]
use reborn_core::CodecPreference;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Capability {
    pub compiled_locally: bool,
    pub distribution_approved: bool,
    pub platform_qualified: bool,
    #[serde(default)]
    pub owner_private_experiment: bool,
}
#[derive(Clone, Default, Deserialize)]
pub struct Inventory {
    pub schema: u32,
    pub codecs: BTreeMap<String, Capability>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Eligibility {
    pub compiled_locally: bool,
    pub runtime_enabled: bool,
    // GetCodecs is the intersection, not a raw remote advertisement.
    pub mutually_usable: bool,
    pub remote_advertised: Option<bool>,
    pub distribution_approved: bool,
    pub platform_qualified: bool,
    pub auto_eligible: bool,
    pub experimental_eligible: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PolicyContext {
    /// Measured load informs the next connection attempt only; never switch a
    /// playing transport simply because a scan or transient burst occurred.
    pub wifi_heavy_transfer: bool,
    pub cpu_pressure: bool,
    pub last_negotiated: Option<String>,
    pub repeated_failures: BTreeSet<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub context: PolicyContext,
    pub generation: u64,
    pub preference: CodecPreference,
    pub eligibility: BTreeMap<String, Eligibility>,
    pub visited: BTreeSet<String>,
    pub selected: Option<String>,
    pub requested: Option<String>,
    pub state: String,
    pub reason: Option<String>,
}
impl Session {
    pub fn new(
        generation: u64,
        preference: CodecPreference,
        inventory: &Inventory,
        runtime: &[String],
        mutual: &[String],
    ) -> Self {
        Self::new_experimental(generation, preference, inventory, runtime, mutual, false)
    }
    pub fn new_experimental(
        generation: u64,
        preference: CodecPreference,
        inventory: &Inventory,
        runtime: &[String],
        mutual: &[String],
        experimental: bool,
    ) -> Self {
        let eligibility = ["SBC", "AAC", "aptX", "aptX-HD", "LDAC"]
            .into_iter()
            .map(|name| {
                let cap = inventory.codecs.get(name).cloned().unwrap_or_default();
                let runtime_enabled = runtime.iter().any(|c| c == name);
                let mutually_usable = mutual.iter().any(|c| c == name);
                let auto_eligible = inventory.schema == 1
                    && cap.compiled_locally
                    && runtime_enabled
                    && mutually_usable
                    && cap.distribution_approved
                    && cap.platform_qualified;
                let experimental_eligible = experimental
                    && inventory.schema == 1
                    && cap.compiled_locally
                    && runtime_enabled
                    && mutually_usable
                    && (cap.distribution_approved || cap.owner_private_experiment);
                (
                    name.into(),
                    Eligibility {
                        compiled_locally: cap.compiled_locally,
                        runtime_enabled,
                        mutually_usable,
                        remote_advertised: None,
                        distribution_approved: cap.distribution_approved,
                        platform_qualified: cap.platform_qualified,
                        auto_eligible,
                        experimental_eligible,
                    },
                )
            })
            .collect();
        Self {
            context: PolicyContext::default(),
            generation,
            preference,
            eligibility,
            visited: BTreeSet::new(),
            selected: None,
            requested: None,
            state: "Ready".into(),
            reason: None,
        }
    }
    pub fn next(&mut self, generation: u64, elapsed_ms: u64) -> Option<String> {
        if generation != self.generation || self.generation == 0 {
            self.fail("transport_generation_changed");
            return None;
        }
        if self.state != "Ready" {
            return None;
        }
        if elapsed_ms >= 12_000 || self.visited.len() >= 3 {
            self.fail("attempt_budget_exhausted");
            return None;
        }
        // At most two preferred attempts plus conformant SBC fallback. No XQ,
        // nonconformant bitpool, opaque codec blobs or unsupported high rates.
        let mut ranked = vec!["LDAC", "aptX-HD", "aptX", "AAC"];
        if let Some(previous) = self.context.last_negotiated.as_deref() {
            if let Some(index) = ranked.iter().position(|name| *name == previous) {
                let stable = ranked.remove(index);
                ranked.insert(0, stable);
            }
        }
        let mut preferred: Vec<&str> = ranked
            .into_iter()
            .filter(|name| {
                !(self.preference != CodecPreference::Auto
                    || self.context.repeated_failures.contains(*name)
                    || (self.context.wifi_heavy_transfer && matches!(*name, "LDAC" | "aptX-HD"))
                    || (self.context.cpu_pressure && matches!(*name, "LDAC" | "AAC")))
                    && (self.eligibility[*name].auto_eligible
                        || self.eligibility[*name].experimental_eligible)
            })
            .take(2)
            .collect();
        if !matches!(
            self.preference,
            CodecPreference::Auto | CodecPreference::Sbc | CodecPreference::SbcXq
        ) {
            preferred.push(self.preference.label());
        }
        preferred.push("SBC");
        let next = preferred.into_iter().find(|name| {
            let e = &self.eligibility[*name];
            let allowed = if self.preference == CodecPreference::Auto {
                e.auto_eligible
                    || e.experimental_eligible
                    || (*name == "SBC"
                        && e.compiled_locally
                        && e.runtime_enabled
                        && e.mutually_usable
                        && e.distribution_approved)
            } else if self.preference == CodecPreference::SbcXq {
                // XQ is a quality policy of SBC. If its startup policy or peer
                // mode is absent, retain conformant SBC instead of no audio.
                e.compiled_locally
                    && e.runtime_enabled
                    && e.mutually_usable
                    && e.distribution_approved
            } else if self.preference != CodecPreference::Sbc && *name != "SBC" {
                e.auto_eligible || e.experimental_eligible
            } else {
                e.compiled_locally
                    && e.runtime_enabled
                    && e.mutually_usable
                    && e.distribution_approved
            };
            allowed && !self.visited.contains(*name)
        });
        let Some(next) = next else {
            self.fail("no_eligible_codec_PHYSICAL_GATE_or_distribution_gate");
            return None;
        };
        self.visited.insert(next.into());
        self.requested = Some(next.into());
        Some(next.into())
    }
    pub fn apply_context(&mut self, context: PolicyContext) {
        if self.state == "Ready" && self.visited.is_empty() {
            self.context = context;
        }
    }
    pub fn accepted(&mut self, codec: &str) {
        self.selected = Some(codec.into());
        self.state = "AwaitingNegotiatedObservation".into();
    }
    pub fn fail(&mut self, reason: &str) {
        self.state = "Failed".into();
        self.reason = Some(reason.into());
    }
}
fn read_value(path: &str) -> Option<serde_json::Value> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(65_537)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= 65_536)
        .then(|| serde_json::from_slice(&bytes).ok())
        .flatten()
}
pub fn experimental_enabled() -> bool {
    if let Some(v) = read_value("/data/bluetooth/codec-policy.json") {
        return v["schema"] == 1 && v["experimental"] == true;
    }
    read_value("/etc/y2linux/bluetooth-codecs.json")
        .is_some_and(|v| v["schema"] == 1 && v["private_integration_enabled"] == true)
}
pub fn experimental_xq_enabled() -> bool {
    // Requested daemon quality may differ until restart. Expose XQ only when
    // the current boot's startup receipt has actually selected that policy.
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    read_value("/run/y2/codec-runtime.json").is_some_and(|v| {
        v["boot_id"].as_str() == Some(boot.trim())
            && v["settings"]["experimental"] == true
            && matches!(v["settings"]["sbc_quality"].as_str(), Some("xq" | "xq+"))
    })
}
/// Read-only policy inputs: stale radio records never constrain a new session.
pub fn context(address: &str, observed_codec: Option<&str>) -> PolicyContext {
    let mut result = PolicyContext {
        last_negotiated: observed_codec.map(str::to_owned),
        ..Default::default()
    };
    if !(address.len() == 17
        && address.split(':').count() == 6
        && address
            .split(':')
            .all(|v| v.len() == 2 && v.bytes().all(|b| b.is_ascii_hexdigit())))
    {
        return result;
    }
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    if let (Some(value), Some(now)) = (
        read_value("/run/y2/coexistence.json"),
        crate::native::monotonic_seconds(),
    ) {
        let age = value["monotonic_s"].as_f64().map(|stamp| now - stamp);
        if value["boot_id"].as_str() == Some(boot.trim())
            && age.is_some_and(|v| (0.0..10.0).contains(&v))
        {
            result.wifi_heavy_transfer = value["wifi_heavy_transfer"] == true;
            result.cpu_pressure = value["cpu_pressure"].as_str().is_some_and(|psi| {
                psi.lines()
                    .find(|line| line.starts_with("some "))
                    .is_some_and(|line| {
                        line.split_whitespace()
                            .find_map(|part| part.strip_prefix("avg10=")?.parse::<f64>().ok())
                            .is_some_and(|v| v >= 20.0)
                    })
            });
        }
    }
    if let Some(value) = read_value("/data/bluetooth/codec-history.json") {
        if value["schema"] == 1 {
            let peer = &value["peers"][address.to_ascii_uppercase()];
            if result.last_negotiated.is_none() {
                result.last_negotiated = peer["last_negotiated"].as_str().map(str::to_owned);
            }
            if let Some(failures) = peer["failures"].as_object() {
                result.repeated_failures = failures
                    .iter()
                    .filter(|(_, v)| v.as_u64().is_some_and(|n| n >= 2))
                    .map(|(k, _)| k.clone())
                    .collect();
            }
        }
    }
    result
}
/// Store only observed negotiation or explicit supported-method rejection.
/// No periodic writes and no inference that a selected request sounded good.
pub fn history(address: &str, negotiated: Option<&str>, rejected: Option<&str>) {
    if !(address.len() == 17
        && address.split(':').count() == 6
        && address
            .split(':')
            .all(|v| v.len() == 2 && v.bytes().all(|b| b.is_ascii_hexdigit())))
    {
        return;
    }
    let path = "/data/bluetooth/codec-history.json";
    if let Some(value) = update_history(read_value(path), address, negotiated, rejected) {
        if let Ok(bytes) = serde_json::to_vec(&value) {
            if bytes.len() <= 65_536 {
                let _ = reborn_core::atomic_write(std::path::Path::new(path), &bytes);
            }
        }
    }
}

fn update_history(
    retained: Option<serde_json::Value>,
    address: &str,
    negotiated: Option<&str>,
    rejected: Option<&str>,
) -> Option<serde_json::Value> {
    const CODECS: [&str; 5] = ["SBC", "AAC", "aptX", "aptX-HD", "LDAC"];
    let negotiated = negotiated.filter(|name| CODECS.contains(name));
    let rejected = rejected.filter(|name| CODECS.contains(name));
    if negotiated.is_none() && rejected.is_none() {
        return None;
    }
    // Preserve only the bounded schema. Restored/edited files can contain
    // arbitrary extra data; never grow or recopy it into the next receipt.
    let mut value = serde_json::json!({"schema":1,"peers":{}});
    let peers = value["peers"].as_object_mut()?;
    if let Some(retained) = retained.filter(|value| value["schema"] == 1) {
        if let Some(saved) = retained["peers"].as_object() {
            for (address, peer) in saved
                .iter()
                .filter(|(address, _)| {
                    address.len() == 17
                        && address.split(':').count() == 6
                        && address
                            .split(':')
                            .all(|v| v.len() == 2 && v.bytes().all(|b| b.is_ascii_hexdigit()))
                })
                .take(64)
            {
                let mut clean = serde_json::json!({"failures":{}});
                if let Some(codec) = peer["last_negotiated"]
                    .as_str()
                    .filter(|c| CODECS.contains(c))
                {
                    clean["last_negotiated"] = codec.into();
                }
                for codec in CODECS {
                    if let Some(count) = peer["failures"][codec].as_u64() {
                        clean["failures"][codec] = count.min(255).into();
                    }
                }
                peers.insert(address.to_ascii_uppercase(), clean);
            }
        }
    }
    let address = address.to_ascii_uppercase();
    if peers.len() >= 64 && !peers.contains_key(&address) {
        return None;
    }
    let peer = peers
        .entry(address)
        .or_insert_with(|| serde_json::json!({"failures":{}}));
    if let Some(codec) = negotiated {
        peer["last_negotiated"] = codec.into();
        peer["failures"][codec] = 0.into();
    }
    if let Some(codec) = rejected {
        let count = peer["failures"][codec]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(1)
            .min(255);
        peer["failures"][codec] = count.into();
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_retains_only_bounded_codec_events_and_observation_clears_failure() {
        let address = "AA:BB:CC:DD:EE:FF";
        let retained = serde_json::json!({"schema":1, "owner_notes":"x".repeat(65_000),
            "peers":{address:{"last_negotiated":"invented", "name":"Private headphones",
                "failures":{"LDAC":999999, "owner secret":999999}}}});
        let value = update_history(Some(retained), address, None, Some("LDAC")).unwrap();
        assert_eq!(value["peers"][address]["failures"]["LDAC"], 255);
        assert!(serde_json::to_vec(&value).unwrap().len() < 150);
        assert!(value["owner_notes"].is_null());
        assert!(value["peers"][address]["name"].is_null());
        let observed = update_history(Some(value), address, Some("LDAC"), None).unwrap();
        assert_eq!(observed["peers"][address]["last_negotiated"], "LDAC");
        assert_eq!(observed["peers"][address]["failures"]["LDAC"], 0);
        assert!(update_history(None, address, Some("unknown"), None).is_none());
        let mut too_many = serde_json::json!({"schema":1,"peers":{}});
        for id in 0..100 {
            too_many["peers"][format!("00:00:00:00:00:{id:02X}")] = serde_json::json!({});
        }
        assert!(update_history(Some(too_many.clone()), address, Some("SBC"), None).is_none());
        let bounded =
            update_history(Some(too_many), "00:00:00:00:00:00", None, Some("LDAC")).unwrap();
        assert_eq!(bounded["peers"].as_object().unwrap().len(), 64);
        assert!(serde_json::to_vec(&bounded).unwrap().len() < 65_536);
    }
    #[test]
    fn auto_uses_history_and_current_load_once_without_flapping() {
        let names: Vec<String> = ["SBC", "AAC", "aptX", "aptX-HD", "LDAC"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let inv = Inventory {
            schema: 1,
            codecs: names
                .iter()
                .map(|name| {
                    (
                        name.clone(),
                        Capability {
                            compiled_locally: true,
                            distribution_approved: true,
                            platform_qualified: true,
                            owner_private_experiment: false,
                        },
                    )
                })
                .collect(),
        };
        let mut session = Session::new(2, CodecPreference::Auto, &inv, &names, &names);
        session.apply_context(PolicyContext {
            last_negotiated: Some("AAC".into()),
            ..Default::default()
        });
        assert_eq!(session.next(2, 0).as_deref(), Some("AAC"));
        session.accepted("AAC");
        session.apply_context(PolicyContext {
            wifi_heavy_transfer: true,
            cpu_pressure: true,
            ..Default::default()
        });
        assert!(session.next(2, 1).is_none());
        let mut busy = Session::new(3, CodecPreference::Auto, &inv, &names, &names);
        busy.apply_context(PolicyContext {
            wifi_heavy_transfer: true,
            cpu_pressure: true,
            repeated_failures: BTreeSet::from(["aptX".into()]),
            ..Default::default()
        });
        assert_eq!(busy.next(3, 0).as_deref(), Some("SBC"));
        let mut explicit = Session::new(4, CodecPreference::Ldac, &inv, &names, &names);
        explicit.apply_context(PolicyContext {
            repeated_failures: BTreeSet::from(["LDAC".into()]),
            ..Default::default()
        });
        assert_eq!(explicit.next(4, 0).as_deref(), Some("LDAC"));
    }
    #[test]
    fn experimental_gate_allows_private_compiled_codec_without_faking_qualification() {
        let mut inventory = Inventory {
            schema: 1,
            ..Default::default()
        };
        for name in ["SBC", "AAC", "LDAC"] {
            inventory.codecs.insert(
                name.into(),
                Capability {
                    compiled_locally: true,
                    distribution_approved: name == "SBC",
                    platform_qualified: false,
                    owner_private_experiment: name != "SBC",
                },
            );
        }
        let all = vec!["SBC".into(), "AAC".into(), "LDAC".into()];
        let mut production = Session::new(1, CodecPreference::Auto, &inventory, &all, &all);
        assert_eq!(production.next(1, 0).as_deref(), Some("SBC"));
        let mut experimental =
            Session::new_experimental(1, CodecPreference::Auto, &inventory, &all, &all, true);
        assert_eq!(experimental.next(1, 0).as_deref(), Some("LDAC"));
        assert!(!experimental.eligibility["LDAC"].platform_qualified);
        assert!(!experimental.eligibility["LDAC"].distribution_approved);
        let mut unavailable = Session::new_experimental(
            1,
            CodecPreference::Ldac,
            &inventory,
            &all,
            &["SBC".into()],
            true,
        );
        assert_eq!(unavailable.next(1, 0).as_deref(), Some("SBC"));
    }
    #[test]
    fn auto_needs_every_gate_and_never_treats_request_as_negotiation() {
        let mut inv = Inventory {
            schema: 1,
            ..Default::default()
        };
        inv.codecs.insert(
            "SBC".into(),
            Capability {
                compiled_locally: true,
                distribution_approved: true,
                platform_qualified: false,
                owner_private_experiment: false,
            },
        );
        let available = vec!["SBC".into()];
        let mut auto = Session::new(9, CodecPreference::Auto, &inv, &available, &available);
        assert_eq!(auto.next(9, 0).as_deref(), Some("SBC"));
        let mut baseline = Session::new(9, CodecPreference::Sbc, &inv, &available, &available);
        assert_eq!(baseline.next(9, 0).as_deref(), Some("SBC"));
        baseline.accepted("SBC");
        assert_eq!(baseline.state, "AwaitingNegotiatedObservation");
        assert!(baseline.next(9, 1).is_none());
        inv.codecs.get_mut("SBC").unwrap().platform_qualified = true;
        assert!(
            Session::new(9, CodecPreference::Auto, &inv, &[], &available)
                .next(9, 0)
                .is_none()
        );
        assert!(
            Session::new(9, CodecPreference::Auto, &inv, &available, &[])
                .next(9, 0)
                .is_none()
        );
    }
    #[test]
    fn fallback_is_bounded_and_peer_change_timeout_and_unknown_completion_stop_it() {
        let mut inv = Inventory {
            schema: 1,
            ..Default::default()
        };
        let all: Vec<String> = ["SBC", "AAC", "aptX", "aptX-HD", "LDAC"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        for codec in &all {
            inv.codecs.insert(
                codec.clone(),
                Capability {
                    compiled_locally: true,
                    distribution_approved: true,
                    platform_qualified: true,
                    owner_private_experiment: false,
                },
            );
        }
        let mut auto = Session::new(7, CodecPreference::Auto, &inv, &all, &all);
        assert_eq!(auto.next(7, 0).as_deref(), Some("LDAC"));
        assert_eq!(auto.next(7, 1).as_deref(), Some("aptX-HD"));
        assert_eq!(auto.next(7, 2).as_deref(), Some("SBC"));
        assert!(auto.next(7, 3).is_none());
        for (generation, time, unknown) in [(8, 0, false), (7, 12_000, false), (7, 1, true)] {
            let mut s = Session::new(7, CodecPreference::Auto, &inv, &all, &all);
            if unknown {
                s.fail("completion_unknown");
            }
            assert!(s.next(generation, time).is_none());
        }
    }
}
