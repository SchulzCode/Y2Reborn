//! Headphone correction from AutoEq parametric profiles.
//!
//! AutoEq (github.com/jaakkopasanen/AutoEq, `results/`) publishes, for many
//! measured headphones, a `ParametricEQ.txt`: a negative preamp and up to ten
//! peaking or shelf filters that bring the headphone towards a target
//! response. Listeners copy those files into an `EQ` folder on the SD card;
//! the FFmpeg graph runs the chosen one ahead of the listener's own
//! equalizer bands.
use crate::{MediaSource, Source};
use serde::{Deserialize, Serialize};
use std::fs;

/// Shape of one correction filter (AutoEq's PK, LSC and HSC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    #[default]
    Peaking,
    LowShelf,
    HighShelf,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Filter {
    pub kind: FilterKind,
    pub frequency_hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

/// A parsed profile: the preamp to apply and the filters in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Correction {
    pub preamp_db: f32,
    pub filters: Vec<Filter>,
}

/// Most filters the media graph accepts for a correction.
pub const MAX_FILTERS: usize = 12;

/// Folder at the top of the SD card holding profile files: each
/// `<Headphone name>.txt` is an AutoEq `ParametricEQ.txt`.
pub const FOLDER: &str = "EQ";

/// Profile files larger than this are not profiles.
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// A profile file from the `EQ` folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// The file name without `.txt`, shown as the headphone's name and kept
    /// in settings.
    pub name: String,
    /// The correction, or why the file can't be used.
    pub correction: Result<Correction, String>,
}

impl Profile {
    /// Parse a profile file; the preamp is made clip-safe.
    pub fn new(name: &str, text: &str) -> Self {
        let correction = parse_parametric_eq(text).map(|mut c| {
            c.preamp_db = safe_preamp_db(&c);
            c
        });
        Self {
            name: name.to_owned(),
            correction,
        }
    }
}

/// The correction chosen by name, if its file is present and readable.
pub fn correction(name: &str, profiles: &[Profile]) -> Option<Correction> {
    profiles
        .iter()
        .find(|p| p.name == name)?
        .correction
        .clone()
        .ok()
}

/// Every `*.txt` profile in the `EQ` folder of the SD card, by name. Only
/// the card is read (the first online card source): profiles are files the
/// listener copies there, not something internal storage carries.
pub fn read_profiles(sources: &[Source]) -> Vec<Profile> {
    let Some(card) = sources
        .iter()
        .find(|s| s.online && matches!(s.kind, MediaSource::SdCard(_)))
    else {
        return vec![];
    };
    let Ok(entries) = fs::read_dir(card.root.join(FOLDER)) else {
        return vec![];
    };
    let mut found: Vec<Profile> = vec![];
    for entry in entries.flatten() {
        let path = entry.path();
        let is_txt = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("txt"));
        let small = entry
            .metadata()
            .is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE_BYTES);
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_txt || !small || name.starts_with('.') {
            continue;
        }
        // Files saved on Windows may start with a byte-order mark.
        let text = fs::read(&path)
            .map(|b| {
                String::from_utf8_lossy(&b)
                    .trim_start_matches('\u{feff}')
                    .to_owned()
            })
            .unwrap_or_default();
        found.push(Profile::new(name, &text));
    }
    found.sort_by_key(|p| p.name.to_lowercase());
    found
}

/// Parse AutoEq's `ParametricEQ.txt` (Equalizer APO syntax):
/// `Preamp: -6.2 dB` then `Filter N: ON PK Fc 105 Hz Gain -2.7 dB Q 0.70`.
/// Filters that are OFF are skipped; an unknown type or a malformed line
/// rejects the whole profile rather than playing half a correction.
pub fn parse_parametric_eq(text: &str) -> Result<Correction, String> {
    let mut out = Correction::default();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(rest) = line.strip_prefix("Preamp:") {
            let value = rest.trim().trim_end_matches("dB").trim();
            out.preamp_db = value.parse().map_err(|_| format!("Bad preamp: {line}"))?;
            continue;
        }
        let Some((head, body)) = line.split_once(':') else {
            return Err(format!("Unknown line: {line}"));
        };
        if !head.trim_start().starts_with("Filter") {
            return Err(format!("Unknown line: {line}"));
        }
        let words: Vec<&str> = body.split_whitespace().collect();
        match words.first() {
            Some(&"ON") => {}
            Some(&"OFF") => continue,
            _ => return Err(format!("Bad filter: {line}")),
        }
        let kind = match words.get(1) {
            Some(&"PK") | Some(&"PEQ") => FilterKind::Peaking,
            Some(&"LSC") | Some(&"LS") => FilterKind::LowShelf,
            Some(&"HSC") | Some(&"HS") => FilterKind::HighShelf,
            _ => return Err(format!("Unsupported filter type: {line}")),
        };
        let value = |name: &str| -> Result<f32, String> {
            let at = words
                .iter()
                .position(|w| *w == name)
                .ok_or_else(|| format!("{name} missing: {line}"))?;
            words
                .get(at + 1)
                .and_then(|v| v.parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("Bad {name}: {line}"))
        };
        let filter = Filter {
            kind,
            frequency_hz: value("Fc")?,
            gain_db: value("Gain")?,
            q: value("Q")?,
        };
        if !(10.0..=24_000.0).contains(&filter.frequency_hz)
            || !(0.05..=20.0).contains(&filter.q)
            || filter.gain_db.abs() > 30.0
        {
            return Err(format!("Filter out of range: {line}"));
        }
        out.filters.push(filter);
    }
    if out.filters.is_empty() {
        return Err("No filters".into());
    }
    if out.filters.len() > MAX_FILTERS {
        return Err(format!("More than {MAX_FILTERS} filters"));
    }
    if !(-30.0..=0.0).contains(&out.preamp_db) {
        return Err("Preamp out of range".into());
    }
    Ok(out)
}

/// Gain of one RBJ cookbook biquad (the formulas FFmpeg's `equalizer`,
/// `lowshelf` and `highshelf` use with `t=q`) at `hz`, in dB.
pub fn filter_response_db(f: &Filter, hz: f64, rate: f64) -> f64 {
    use std::f64::consts::PI;
    let a = 10f64.powf(f64::from(f.gain_db) / 40.0);
    let w0 = 2.0 * PI * f64::from(f.frequency_hz) / rate;
    let (sin, cos) = w0.sin_cos();
    let alpha = sin / (2.0 * f64::from(f.q));
    let (b0, b1, b2, a0, a1, a2) = match f.kind {
        FilterKind::Peaking => (
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        ),
        FilterKind::LowShelf => {
            let beta = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) - (a - 1.0) * cos + beta),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - beta),
                (a + 1.0) + (a - 1.0) * cos + beta,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - beta,
            )
        }
        FilterKind::HighShelf => {
            let beta = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) + (a - 1.0) * cos + beta),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - beta),
                (a + 1.0) - (a - 1.0) * cos + beta,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - beta,
            )
        }
    };
    // |H(e^jw)| with z^-1 = e^-jw.
    let w = 2.0 * PI * hz / rate;
    let (s1, c1) = w.sin_cos();
    let (s2, c2) = (2.0 * w).sin_cos();
    let num = (b0 + b1 * c1 + b2 * c2).hypot(b1 * s1 + b2 * s2);
    let den = (a0 + a1 * c1 + a2 * c2).hypot(a1 * s1 + a2 * s2);
    20.0 * (num / den).log10()
}

/// Highest gain of all filters together, 20 Hz to 20 kHz, at 44.1 and
/// 48 kHz, in dB (zero when the filters only cut).
pub fn peak_boost_db(filters: &[Filter]) -> f64 {
    let mut peak = 0f64;
    for rate in [44_100.0, 48_000.0] {
        // 1/48-octave steps across the audible band.
        for step in 0..=(10 * 48) {
            let hz = 20.0 * 2f64.powf(f64::from(step) / 48.0);
            if hz > 20_000.0 {
                break;
            }
            let total: f64 = filters
                .iter()
                .map(|f| filter_response_db(f, hz, rate))
                .sum();
            peak = peak.max(total);
        }
    }
    peak
}

/// AutoEq's preamp, lowered if our own sweep finds a higher combined peak,
/// so a full-scale signal never leaves the correction above 0 dBFS.
pub fn safe_preamp_db(c: &Correction) -> f32 {
    let needed = -(peak_boost_db(&c.filters) as f32);
    // Round the extra cut down to 0.1 dB so the result is never short.
    let needed = (needed * 10.0).floor() / 10.0;
    c.preamp_db.min(needed).min(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AutoEq's file layout with generic numbers.
    const EXAMPLE: &str = "Preamp: -4.0 dB\n\
         Filter 1: ON LSC Fc 105 Hz Gain 3.0 dB Q 0.70\n\
         Filter 2: ON PK Fc 2500 Hz Gain -2.0 dB Q 1.40\n\
         Filter 3: ON PK Fc 6000 Hz Gain 2.0 dB Q 3.00\n\
         Filter 4: ON HSC Fc 10000 Hz Gain -1.5 dB Q 0.70\n";

    #[test]
    fn parses_autoeq_parametric_text() {
        let c = parse_parametric_eq(
            "Preamp: -6.2 dB\n\
             Filter 1: ON LSC Fc 105 Hz Gain 5.5 dB Q 0.70\n\
             Filter 2: OFF PK Fc 200 Hz Gain 1.0 dB Q 1.00\n\
             Filter 3: ON PK Fc 3000 Hz Gain -2.5 dB Q 2.00\n\
             Filter 4: ON HSC Fc 10000 Hz Gain -0.0 dB Q 0.70\n",
        )
        .unwrap();
        assert_eq!(c.preamp_db, -6.2);
        assert_eq!(c.filters.len(), 3);
        assert_eq!(
            c.filters[0],
            Filter {
                kind: FilterKind::LowShelf,
                frequency_hz: 105.0,
                gain_db: 5.5,
                q: 0.7
            }
        );
        assert_eq!(c.filters[1].kind, FilterKind::Peaking);
        assert_eq!(c.filters[1].gain_db, -2.5);
        assert_eq!(c.filters[2].kind, FilterKind::HighShelf);
    }

    #[test]
    fn rejects_broken_profiles() {
        for bad in [
            "",
            "Preamp: -3 dB\n",
            "Preamp: loud\nFilter 1: ON PK Fc 100 Hz Gain 1 dB Q 1\n",
            "Filter 1: ON BP Fc 100 Hz Gain 1 dB Q 1\n",
            "Filter 1: ON PK Fc 100 Hz Gain dB Q 1\n",
            "Filter 1: ON PK Fc 100 Hz Q 1\n",
            "Filter 1: ON PK Fc 100 Hz Gain 1 dB Q 0\n",
            "Filter 1: ON PK Fc 100 Hz Gain 99 dB Q 1\n",
            "Preamp: +3 dB\nFilter 1: ON PK Fc 100 Hz Gain 1 dB Q 1\n",
            "Hello\nFilter 1: ON PK Fc 100 Hz Gain 1 dB Q 1\n",
        ] {
            assert!(parse_parametric_eq(bad).is_err(), "accepted {bad:?}");
        }
        let many: String = (1..=13)
            .map(|i| format!("Filter {i}: ON PK Fc {} Hz Gain 1 dB Q 1\n", 100 * i))
            .collect();
        assert!(parse_parametric_eq(&many).is_err());
    }

    #[test]
    fn profiles_are_chosen_by_name_and_never_clip() {
        let good = Profile::new("Example Buds", EXAMPLE);
        let bad = Profile::new("Notes", "Hello");
        assert!(bad.correction.is_err());
        let profiles = [good, bad];
        let c = correction("Example Buds", &profiles).unwrap();
        assert_eq!(c.filters.len(), 4);
        assert!(c.preamp_db <= -4.0);
        // Preamp plus the loudest point of the corrected response stays at
        // or below full scale.
        let peak = peak_boost_db(&c.filters);
        assert!(peak > 0.0);
        assert!(f64::from(c.preamp_db) + peak <= 1e-3);
        assert!(correction("Notes", &profiles).is_none());
        assert!(correction("Gone", &profiles).is_none());
        assert!(correction("", &profiles).is_none());
    }

    #[test]
    fn safe_preamp_covers_a_preamp_that_is_too_small() {
        let c = Correction {
            preamp_db: -1.0,
            filters: vec![Filter {
                kind: FilterKind::Peaking,
                frequency_hz: 1000.0,
                gain_db: 6.0,
                q: 1.0,
            }],
        };
        let safe = safe_preamp_db(&c);
        assert!((-6.2..=-6.0).contains(&safe), "{safe}");
        // A cut-only profile keeps AutoEq's (zero or negative) preamp.
        let cut = Correction {
            preamp_db: 0.0,
            filters: vec![Filter {
                gain_db: -4.0,
                ..c.filters[0]
            }],
        };
        assert_eq!(safe_preamp_db(&cut), 0.0);
    }

    #[test]
    fn biquad_response_matches_the_cookbook_shapes() {
        let pk = Filter {
            kind: FilterKind::Peaking,
            frequency_hz: 1000.0,
            gain_db: 6.0,
            q: 1.0,
        };
        assert!((filter_response_db(&pk, 1000.0, 48_000.0) - 6.0).abs() < 0.01);
        assert!(filter_response_db(&pk, 50.0, 48_000.0).abs() < 0.1);
        let ls = Filter {
            kind: FilterKind::LowShelf,
            frequency_hz: 105.0,
            gain_db: -3.0,
            q: 0.7,
        };
        assert!((filter_response_db(&ls, 20.0, 48_000.0) + 3.0).abs() < 0.2);
        assert!(filter_response_db(&ls, 5000.0, 48_000.0).abs() < 0.05);
        let hs = Filter {
            kind: FilterKind::HighShelf,
            frequency_hz: 10_000.0,
            gain_db: 4.0,
            ..ls
        };
        assert!(filter_response_db(&hs, 100.0, 48_000.0).abs() < 0.05);
        assert!((filter_response_db(&hs, 20_000.0, 48_000.0) - 4.0).abs() < 0.5);
    }

    #[test]
    fn profiles_are_read_from_the_sd_cards_eq_folder_only() {
        let base = std::env::temp_dir().join(format!("reborn-hpeq-{}", std::process::id()));
        let (card, internal) = (base.join("card"), base.join("internal"));
        fs::create_dir_all(card.join(FOLDER)).unwrap();
        fs::create_dir_all(internal.join(FOLDER)).unwrap();
        let bom = format!("\u{feff}{EXAMPLE}");
        fs::write(card.join(FOLDER).join("Example Buds.txt"), bom).unwrap();
        fs::write(card.join(FOLDER).join("notes.TXT"), "Hello").unwrap();
        fs::write(card.join(FOLDER).join("cover.jpg"), "x").unwrap();
        fs::write(card.join(FOLDER).join(".hidden.txt"), EXAMPLE).unwrap();
        fs::write(internal.join(FOLDER).join("another pair.txt"), EXAMPLE).unwrap();
        let source = |kind: MediaSource, root: &std::path::Path, online: bool| Source {
            id: root.display().to_string(),
            kind,
            root: root.to_owned(),
            online,
            mount: String::new(),
            mount_id: None,
        };
        let mut sources = vec![
            source(MediaSource::Internal, &internal, true),
            source(MediaSource::SdCard("CARD".into()), &card, true),
        ];
        let found = read_profiles(&sources);
        let names: Vec<_> = found.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Example Buds", "notes"]);
        // The byte-order mark is removed; a file that isn't a profile stays
        // listed, unusable.
        assert!(found[0].correction.is_ok());
        assert!(found[1].correction.is_err());
        // Card removed: internal storage is not a fallback.
        sources[1].online = false;
        assert!(read_profiles(&sources).is_empty());
        fs::remove_dir_all(&base).unwrap();
    }
}
