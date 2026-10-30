//! Equalizer profiles on disk: the shipped and owner folders, importing one, the DSP rise.
use crate::*;

/// The profiles that come with the player (AutoEq, `assets/eq` in Reborn).
pub(crate) fn shipped_eq_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![];
    if let Ok(dev) = std::env::var("REBORN_EQ_PROFILES") {
        dirs.push(PathBuf::from(dev));
    }
    dirs.push(PathBuf::from("/usr/share/reborn/eq"));
    dirs
}

/// Normalize Volume's mode and the equalizer (preamp and filters) a load plays.
pub(crate) type LoadedDsp = (
    reborn_core::ReplayGainMode,
    Option<(f32, Vec<reborn_core::EqBand>)>,
);

/// Whether a load fades in like a raised headphone level: its sound settings
/// play at least 1 dB louder somewhere than the previous load's (a profile
/// turned off, a boosting preset), or Normalize changed (its gain comes from
/// each song's tags, so count it as a rise). Not the first load: that plays
/// what was saved, as the headphone level does.
pub(crate) fn dsp_rise_fades(previous: Option<&LoadedDsp>, next: &LoadedDsp) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    if previous == next {
        return false;
    }
    pub(crate) fn eq(dsp: &LoadedDsp) -> Option<(f32, &[reborn_core::EqBand])> {
        dsp.1
            .as_ref()
            .map(|(preamp, bands)| (*preamp, bands.as_slice()))
    }
    previous.0 != next.0 || reborn_core::eq::level_rise_db(eq(previous), eq(next)) >= 1.0
}

/// Profile files (not hidden, name ending in `suffix`, any letter case)
/// directly in `dir`.
pub(crate) fn eq_files_in(dir: &Path, suffix: &str, files: &mut Vec<reborn_core::eq::ProfileFile>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.')
            && name.to_ascii_lowercase().ends_with(suffix)
            && entry.file_type().is_ok_and(|t| t.is_file())
        {
            files.push(reborn_core::eq::ProfileFile {
                name: reborn_core::eq::profile_name(&name),
                path: entry.path(),
            });
        }
    }
}

/// `ParametricEQ.txt` files in an `EQ` folder (any letter case) on each
/// storage root, then the shipped ones, sorted by name. A profile on the
/// storage replaces a shipped one of the same name (a newer AutoEq file).
pub(crate) fn scan_eq_profiles(
    shipped: &[PathBuf],
    roots: &[PathBuf],
) -> Vec<reborn_core::eq::ProfileFile> {
    let mut files = vec![];
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for folder in entries.flatten().filter(|e| {
            e.file_name().to_string_lossy().eq_ignore_ascii_case("eq")
                && e.file_type().is_ok_and(|t| t.is_dir())
        }) {
            eq_files_in(&folder.path(), reborn_core::eq::FILE_SUFFIX, &mut files);
        }
    }
    // The shipped folder also holds AutoEq's license and SOURCES.md.
    for dir in shipped {
        eq_files_in(dir, "parametriceq.txt", &mut files);
    }
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| seen.insert(f.name.to_lowercase()));
    files.sort_by_key(|f| f.name.to_lowercase());
    files
}

/// Read and check one profile file; the error is a message for the screen.
pub(crate) fn import_eq_profile(path: &Path) -> Result<reborn_core::eq::EqProfile, String> {
    let size = fs::metadata(path)
        .map_err(|_| "That profile file is gone".to_owned())?
        .len();
    if size > reborn_core::eq::MAX_FILE_BYTES {
        return Err("That file is too large for an equalizer profile".into());
    }
    let text = fs::read(path).map_err(|_| "That profile file could not be read".to_owned())?;
    let text = String::from_utf8_lossy(&text);
    let name = path
        .file_name()
        .map(|n| reborn_core::eq::profile_name(&n.to_string_lossy()))
        .unwrap_or_else(|| "Profile".into());
    reborn_core::eq::parse(&name, &text)
}
