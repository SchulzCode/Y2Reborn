//! The Python platform's own output (tools/development/platform-goldens.py) read into the contract
//! types and written back: nothing may be lost, renamed or retyped.
// SPDX-License-Identifier: MIT
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use y2p_contract::*;

fn golden(name: &str) -> Value {
    let path = format!("{}/tests/golden/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect(&path)).expect(&path)
}

fn round_trip<T: Serialize + DeserializeOwned>(name: &str) -> T {
    let original = golden(name);
    let typed: T =
        serde_json::from_value(original.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(
        serde_json::to_value(&typed).unwrap(),
        original,
        "{name} changed on the way through"
    );
    typed
}

#[test]
fn python_records_survive_the_types() {
    let capabilities: Capabilities = round_trip("capabilities");
    assert_eq!(capabilities.schema, schema::CAPABILITIES);
    assert_eq!(
        capabilities.feature_contract_version,
        Some(FEATURE_CONTRACT_VERSION)
    );
    assert!(capabilities.capabilities.unwrap().contains_key("bluetooth"));

    let health: Health = round_trip("health");
    assert_eq!(health.schema, schema::HEALTH);
    assert_eq!(health.state, CheckState::Failed);

    let status: Status = round_trip("status");
    assert_eq!(status.schema, schema::STATUS);
    assert!(status.sections.contains_key("power"));
    let wifi: Status = round_trip("status-wifi");
    assert_eq!(wifi.sections.keys().collect::<Vec<_>>(), ["wifi"]);

    let printed: BootRecord = round_trip("boot-record");
    let written: BootRecord = round_trip("boot-record-file");
    assert_eq!(printed.last_stage, BootStage::PlatformStart);
    assert_eq!(printed.boot_id, written.boot_id);
    let line: BootStageLine = round_trip("boot-stage-line");
    assert_eq!(
        BootStage::parse(&line.stage),
        Some(BootStage::PlatformStart)
    );

    let error: ErrorRecord = round_trip("error");
    assert_eq!(
        (error.schema.as_str(), error.reason.as_str()),
        (schema::ERROR, "unknown_boot_stage")
    );
}
