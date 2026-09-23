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
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
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
                    },
                )
            })
            .collect();
        Self {
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
        let mut preferred: Vec<&str> = ["LDAC", "aptX-HD", "aptX", "AAC"]
            .into_iter()
            .filter(|name| {
                self.preference == CodecPreference::Auto && self.eligibility[*name].auto_eligible
            })
            .take(2)
            .collect();
        preferred.push("SBC");
        let next = preferred.into_iter().find(|name| {
            let e = &self.eligibility[*name];
            let allowed = if self.preference == CodecPreference::Auto {
                e.auto_eligible
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
    pub fn accepted(&mut self, codec: &str) {
        self.selected = Some(codec.into());
        self.state = "AwaitingNegotiatedObservation".into();
    }
    pub fn fail(&mut self, reason: &str) {
        self.state = "Failed".into();
        self.reason = Some(reason.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            },
        );
        let available = vec!["SBC".into()];
        let mut auto = Session::new(9, CodecPreference::Auto, &inv, &available, &available);
        assert!(auto.next(9, 0).is_none());
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
