use super::{import_eq_profile, scan_eq_profiles};
use std::fs;

#[test]
fn eq_folders_on_every_storage_are_listed_and_files_checked_on_import() {
    let root = std::env::temp_dir().join(format!("reborn-eq-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (internal, sd) = (root.join("internal"), root.join("sd"));
    fs::create_dir_all(internal.join("EQ")).unwrap();
    fs::create_dir_all(sd.join("eq")).unwrap();
    fs::create_dir_all(internal.join("Music")).unwrap();
    let good = "Preamp: -3.2 dB\nFilter 1: ON LSC Fc 105 Hz Gain 3 dB Q 0.7\nFilter 2: ON PK Fc 2000 Hz Gain -2 dB Q 2\n";
    fs::write(internal.join("EQ/Sennheiser HD 650 ParametricEQ.txt"), good).unwrap();
    fs::write(sd.join("eq/aria.TXT"), "GraphicEQ: 20 0\n").unwrap();
    fs::write(internal.join("EQ/.hidden.txt"), good).unwrap();
    fs::write(internal.join("EQ/readme.pdf"), "x").unwrap();
    fs::write(internal.join("Music/elsewhere.txt"), good).unwrap();
    fs::write(sd.join("eq/huge.txt"), "#".repeat(70 * 1024)).unwrap();
    // Shipped: one new model and one also on the storage (the storage wins).
    let shipped = root.join("shipped");
    fs::create_dir_all(&shipped).unwrap();
    fs::write(shipped.join("AKG K371 ParametricEQ.txt"), good).unwrap();
    fs::write(shipped.join("Sennheiser HD 650 ParametricEQ.txt"), good).unwrap();
    fs::write(shipped.join("SOURCES.md"), "x").unwrap();
    fs::write(shipped.join("LICENSE-AutoEq.txt"), "MIT License").unwrap();
    let files = scan_eq_profiles(
        std::slice::from_ref(&shipped),
        &[internal.clone(), sd.clone(), root.join("missing")],
    );
    let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["AKG K371", "aria", "huge", "Sennheiser HD 650"]);
    assert!(files[3].path.starts_with(&internal));
    let files: Vec<_> = files.into_iter().skip(1).collect();
    let profile = import_eq_profile(&files[2].path).unwrap();
    assert_eq!(profile.name, "Sennheiser HD 650");
    assert_eq!(profile.bands.len(), 2);
    assert!(profile.preamp_db <= -3.2);
    assert!(import_eq_profile(&files[0].path)
        .unwrap_err()
        .contains("Line 1"));
    assert!(import_eq_profile(&files[1].path)
        .unwrap_err()
        .contains("too large"));
    assert!(import_eq_profile(&root.join("gone.txt"))
        .unwrap_err()
        .contains("gone"));
    let _ = fs::remove_dir_all(&root);
}

/// The real shipped folder (installed as is to /usr/share/reborn/eq)
/// lists its 47 profiles and nothing else: not the license, not SOURCES.md.
#[test]
fn the_shipped_folder_lists_exactly_its_profiles() {
    let shipped = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/eq");
    assert!(shipped.join("LICENSE-AutoEq.txt").is_file());
    let files = scan_eq_profiles(&[shipped], &[]);
    assert_eq!(files.len(), 47);
    assert!(files
        .iter()
        .all(|f| !f.name.to_lowercase().contains("license")));
    assert!(files.iter().all(|f| import_eq_profile(&f.path).is_ok()));
}
