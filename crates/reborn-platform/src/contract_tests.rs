//! The platform contract as Reborn reads it (Y2Linux PLATFORM-RUST-IDIOMATIC-PLAN.md §2.3): every golden output of a
//! command Reborn runs and every golden file Reborn reads (exported by Y2Linux
//! `tools/development/reborn-contract.py` into tests/platform-contract.json) goes through Reborn's own parsers; the
//! typed results must equal the ones recorded before (tests/platform-contract.expected.json). A platform change that
//! keeps what Reborn sees passes; one that changes it fails here. `REBORN_CONTRACT_RECORD=1` records anew after a
//! reviewed change.
use super::*;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// A value's shape: object keys with their shapes, a list's first item, else the JSON type.
fn shape(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), shape(v))).collect())
        }
        Value::Array(items) => Value::Array(items.first().map(shape).into_iter().collect()),
        Value::Null => "null".into(),
        Value::Bool(_) => "bool".into(),
        Value::Number(n) if n.is_f64() => "float".into(),
        Value::Number(_) => "int".into(),
        Value::String(_) => "text".into(),
    }
}

/// What Reborn makes of one command's answer.
fn parsed(argv: &[String], out: &Value, caps: &Value) -> String {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["status", ..] => format!(
            "schema_ok={} {:?}",
            schema(out.clone(), STATUS_SCHEMA).is_ok(),
            snapshot(out, caps)
        ),
        ["capabilities", ..] => format!(
            "schema_ok={} {:?}",
            schema(out.clone(), CAPABILITIES_SCHEMA).is_ok(),
            snapshot(&Value::Object(Map::new()), out)
        ),
        ["health", ..] => format!(
            "schema_ok={} {:?}",
            schema(out.clone(), HEALTH_SCHEMA).is_ok(),
            health(out)
        ),
        ["sleep", "request", ..] => format!(
            "schema_ok={} {:?}",
            schema(out.clone(), schemas::SLEEP).is_ok(),
            sleep(out)
        ),
        ["report", ..] => format!("{:?} {:?}", report_state(out), operation(out)),
        ["media", "repair", ..] => format!("{:?}", sd_repair_result(out)),
        _ => format!("{:?}", operation(out)),
    }
}

/// What Reborn makes of one file it reads.
fn parsed_file(path: &str, content: &Value) -> String {
    let typed = match path {
        "/run/y2/update-progress.json" => format!("{:?}", parse_download(content, None)),
        "/run/y2/report-progress.json" => format!("{:?}", parse_report_progress(content)),
        "/run/y2/power.json" => format!(
            "{:?}",
            crate::power::battery(&serde_json::json!({"platform": content}))
        ),
        _ => String::new(),
    };
    format!("{typed} shape={}", shape(content))
}

#[test]
fn reborn_reads_the_platform_as_recorded() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let Ok(text) = fs::read_to_string(dir.join("platform-contract.json")) else {
        return;
    };
    let cases: Vec<Value> = serde_json::from_str(&text).unwrap();
    let caps = cases
        .iter()
        .find(|c| c["argv"] == serde_json::json!(["capabilities"]) && c["exit"] == 0)
        .map(|c| c["stdout"].clone())
        .unwrap_or(Value::Null);
    let mut seen = BTreeMap::new();
    for case in &cases {
        let name = case["name"].as_str().unwrap_or_default().to_owned();
        let argv: Vec<String> = serde_json::from_value(case["argv"].clone()).unwrap_or_default();
        if case["stdout"].is_object() {
            seen.insert(
                format!("{name} stdout"),
                parsed(&argv, &case["stdout"], &caps),
            );
        }
        if let Some(Value::Object(files)) = case.get("files") {
            for (path, content) in files {
                seen.insert(format!("{name} {path}"), parsed_file(path, content));
            }
        }
    }
    let expected_path = dir.join("platform-contract.expected.json");
    if std::env::var_os("REBORN_CONTRACT_RECORD").is_some() {
        fs::write(
            &expected_path,
            serde_json::to_string_pretty(&seen).unwrap() + "\n",
        )
        .unwrap();
        return;
    }
    let expected: BTreeMap<String, String> =
        serde_json::from_str(&fs::read_to_string(&expected_path).unwrap()).unwrap();
    let mut differences = Vec::new();
    for (key, value) in &seen {
        match expected.get(key) {
            Some(old) if old == value => {}
            Some(old) => differences.push(format!("{key}\n  was {old}\n  now {value}")),
            None => differences.push(format!("{key}: new case\n  now {value}")),
        }
    }
    for key in expected.keys().filter(|k| !seen.contains_key(*k)) {
        differences.push(format!("{key}: case gone"));
    }
    assert!(
        differences.is_empty(),
        "{} differences:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
