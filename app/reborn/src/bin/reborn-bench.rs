#![forbid(unsafe_code)]
//! Invoked only with an inherited, empty scratch directory descriptor.
use reborn_core::{Action, AppModel, Screen};
use reborn_library::benchmark::{database, scanner, summary, timed};
use reborn_ui::Ui;
use serde_json::json;
use std::{fs, path::PathBuf};

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 || args[1] != "--scratch-fd" || args[3] != "--tracks" {
        return Err("use y2-platform bench-library (inherited empty scratch FD required)".into());
    }
    let fd = args[2].parse::<u32>().map_err(|e| e.to_string())?;
    if fd < 3 {
        return Err("scratch directory fd must be >=3".into());
    }
    let root = PathBuf::from(format!("/proc/self/fd/{fd}"));
    let entries = fs::read_dir(&root).map_err(|e| e.to_string())?;
    if entries.count() != 0 {
        return Err("scratch directory must be empty".into());
    }
    let tracks = args[4].parse::<usize>().map_err(|e| e.to_string())?;
    if ![1000, 10000, 20000].contains(&tracks) {
        return Err("tracks must be 1000, 10000 or 20000".into());
    }
    let (db, library) = database(&root, tracks)?;
    let mut ui = Ui::default();
    let mut model = AppModel::default();
    let mut measurements = vec![];
    for (name, screen) in [
        ("album_listing", Screen::Albums),
        ("artist_listing", Screen::Artists),
        ("folder_listing", Screen::Folders),
        ("track_listing", Screen::Tracks),
    ] {
        model.screen = screen;
        let mut samples = vec![];
        for _ in 0..16 {
            timed(&mut samples, || Ok(ui.rows(&model, &library)))?;
        }
        measurements.push(summary(name, &samples));
    }
    model.screen = Screen::Tracks;
    let mut samples = vec![];
    for _ in 0..128 {
        timed(&mut samples, || {
            Ok(ui.action(&mut model, &library, Action::WheelClockwise(1)))
        })?;
    }
    measurements.push(summary("wheel_navigation", &samples));
    samples.clear();
    timed(&mut samples, || model.replace_queue(library.clone(), 0))?;
    measurements.push(summary("queue_construction", &samples));
    let scan = if args.iter().any(|a| a == "--scan") {
        Some(scanner(&root, tracks)?)
    } else {
        None
    };
    println!(
        "{}",
        json!({"database":db,"ui":measurements,"scan":scan,
        "memory":fs::read_to_string("/proc/self/status").ok(),
        "evidence":"userspace measurement only; no electrical durability or target qualification"})
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        println!("{}", json!({"result":"FAILED","failure":error}));
        std::process::exit(1);
    }
}
