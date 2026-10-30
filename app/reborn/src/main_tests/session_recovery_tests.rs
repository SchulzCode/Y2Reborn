use super::recover_settings;
use reborn_core::{AppModel, CodecPreference, SESSION_SCHEMA_VERSION};
use serde_json::json;
use std::fs;

#[test]
fn settings_survive_a_session_from_a_newer_reborn() {
    let path = std::env::temp_dir().join(format!("reborn-newer-{}.json", std::process::id()));
    let mut model = AppModel::default();
    model.settings.volume = 62;
    model.settings.theme = "classic".into();
    let mut value = serde_json::to_value(&model).unwrap();
    value["settings"]["codec_preference"] = "Opus-Future".into();
    value["settings"]["a_later_setting"] = true.into();
    fs::write(
        &path,
        serde_json::to_vec(&json!({"schema": SESSION_SCHEMA_VERSION + 1, "model": value})).unwrap(),
    )
    .unwrap();
    assert!(AppModel::restore(&path).is_err());
    let settings = recover_settings(&path).unwrap();
    assert_eq!(settings.volume, 62);
    assert_eq!(settings.theme, "classic");
    assert_eq!(settings.codec_preference, CodecPreference::Auto);
    fs::remove_file(path).unwrap();
}
