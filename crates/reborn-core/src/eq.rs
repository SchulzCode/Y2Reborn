//! Parametric equalizer profiles: the `ParametricEQ.txt` files AutoEQ writes
//! (Equalizer APO syntax), and a preamp that keeps the filters from lifting a
//! full-scale signal above 0 dBFS. The filters are RBJ cookbook biquads with a
//! Q width, exactly what FFmpeg's `equalizer`, `lowshelf` and `highshelf`
//! compute, so the response evaluated here is the one the player plays.
use crate::{EqBand, EqKind};
use serde::{Deserialize, Serialize};

/// Filters one profile may hold. AutoEQ writes ten; other tools write more (Y2Linux #76). Each is
/// one FFmpeg biquad: twenty cost about 6 % of one Cortex-A7 core while playing, ten about 3 %.
pub const MAX_BANDS: usize = 20;
/// Larger files are not equalizer profiles.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
/// File names Reborn offers as profiles, matched without letter case.
pub const FILE_SUFFIX: &str = ".txt";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EqProfile {
    /// Shown in Settings, from the file name ("HD 650").
    pub name: String,
    /// Gain before the filters (dB, ≤ 0): the file's preamp, lowered further
    /// when the filters could still lift a full-scale signal above 0 dBFS.
    pub preamp_db: f32,
    pub bands: Vec<EqBand>,
}

/// A profile file found on Internal Storage or the SD card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFile {
    pub name: String,
    pub path: std::path::PathBuf,
}

/// The profile name for a file name: "HD 650 ParametricEQ.txt" → "HD 650".
pub fn profile_name(file_name: &str) -> String {
    let stem = file_name
        .get(..file_name.len().saturating_sub(FILE_SUFFIX.len()))
        .filter(|_| file_name.to_ascii_lowercase().ends_with(FILE_SUFFIX))
        .unwrap_or(file_name);
    let lower = stem.to_ascii_lowercase();
    let stem = lower
        .strip_suffix("parametriceq")
        .map_or(stem, |rest| &stem[..rest.len()]);
    let name = stem.trim_end_matches([' ', '_', '-', '.']).trim();
    if name.is_empty() {
        "Profile".into()
    } else {
        name.into()
    }
}

fn number(token: Option<&str>, what: &str, line: usize) -> Result<f32, String> {
    token
        .and_then(|t| t.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("Line {line}: {what} needs a number"))
}

/// Read a `ParametricEQ.txt`: one `Preamp: -6.1 dB` line (optional) and up to
/// ten `Filter N: ON PK|LSC|HSC Fc <Hz> Hz Gain <dB> dB Q <Q>` lines. Filters
/// marked OFF and `#` comments are skipped; anything else is refused rather
/// than played differently from what the file means.
pub fn parse(name: &str, text: &str) -> Result<EqProfile, String> {
    let mut file_preamp: Option<f32> = None;
    let mut bands = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let content = raw.split('#').next().unwrap_or("").trim();
        if content.is_empty() {
            continue;
        }
        let (head, rest) = content
            .split_once(':')
            .ok_or_else(|| format!("Line {line} is not a Preamp or Filter line"))?;
        let head = head.trim().to_ascii_lowercase();
        let mut tokens = rest.split_whitespace();
        if head == "preamp" {
            if file_preamp.is_some() {
                return Err(format!("Line {line}: a second Preamp line"));
            }
            let value = number(tokens.next(), "Preamp", line)?;
            if !(-60.0..=30.0).contains(&value) {
                return Err(format!("Line {line}: Preamp {value} dB is out of range"));
            }
            file_preamp = Some(value);
            continue;
        }
        if head != "filter"
            && !head
                .strip_prefix("filter")
                .is_some_and(|n| n.trim().parse::<u32>().is_ok())
        {
            return Err(format!("Line {line} is not a Preamp or Filter line"));
        }
        match tokens.next().map(str::to_ascii_uppercase).as_deref() {
            Some("ON") => {}
            Some("OFF") => continue,
            _ => return Err(format!("Line {line}: a filter is ON or OFF")),
        }
        let kind = match tokens.next().map(str::to_ascii_uppercase).as_deref() {
            Some("PK") | Some("PEQ") => EqKind::Peak,
            Some("LSC") => EqKind::LowShelf,
            Some("HSC") => EqKind::HighShelf,
            Some(other) => {
                return Err(format!(
                    "Line {line}: filter type {other} is not supported (PK, LSC, HSC)"
                ))
            }
            None => return Err(format!("Line {line}: the filter has no type")),
        };
        let (mut frequency, mut gain, mut q) = (None, None, None);
        while let Some(token) = tokens.next() {
            match token.to_ascii_lowercase().as_str() {
                "fc" => frequency = Some(number(tokens.next(), "Fc", line)?),
                "gain" => gain = Some(number(tokens.next(), "Gain", line)?),
                "q" => q = Some(number(tokens.next(), "Q", line)?),
                "hz" | "db" => {}
                other => return Err(format!("Line {line}: unexpected \"{other}\"")),
            }
        }
        let frequency = frequency.ok_or_else(|| format!("Line {line}: the filter has no Fc"))?;
        let gain = gain.ok_or_else(|| format!("Line {line}: the filter has no Gain"))?;
        let q = q.ok_or_else(|| format!("Line {line}: the filter has no Q"))?;
        if !(20.0..=20_000.0).contains(&frequency) {
            return Err(format!(
                "Line {line}: Fc {frequency} Hz is outside 20 Hz–20 kHz"
            ));
        }
        if !(-30.0..=30.0).contains(&gain) {
            return Err(format!("Line {line}: Gain {gain} dB is outside ±30 dB"));
        }
        if !(0.05..=30.0).contains(&q) {
            return Err(format!("Line {line}: Q {q} is outside 0.05–30"));
        }
        bands.push(EqBand {
            frequency_hz: frequency,
            gain_db: gain,
            q,
            kind,
        });
    }
    if bands.is_empty() {
        return Err("The file has no filters that are on".into());
    }
    if bands.len() > MAX_BANDS {
        return Err(format!(
            "The file has {} filters; Reborn takes up to {MAX_BANDS}",
            bands.len()
        ));
    }
    let preamp_db = safe_preamp_db(file_preamp, &bands);
    Ok(EqProfile {
        name: name.into(),
        preamp_db,
        bands,
    })
}

/// The file's preamp or the one that keeps the filters' highest point at
/// 0 dB, whichever is lower; never a boost. Rounded down to 0.1 dB.
pub fn safe_preamp_db(file_preamp: Option<f32>, bands: &[EqBand]) -> f32 {
    let needed = -peak_db(bands);
    let preamp = file_preamp
        .map_or(needed, |p| f64::from(p).min(needed))
        .min(0.0);
    // The small offset keeps a written -6.1 at -6.1 (f32: -61.000001 × 0.1);
    // a step down follows when that rounding left the peak above 0 dB.
    let mut rounded = (preamp * 10.0 + 1e-3).floor() / 10.0;
    if rounded > needed {
        rounded -= 0.1;
    }
    rounded.max(-60.0) as f32
}

/// Biquad coefficients (b0, b1, b2, a0, a1, a2), as FFmpeg's af_biquads.
fn coefficients(band: &EqBand, rate: f64) -> [f64; 6] {
    let a = 10f64.powf(f64::from(band.gain_db) / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * f64::from(band.frequency_hz) / rate;
    let (sin, cos) = w0.sin_cos();
    let alpha = sin / (2.0 * f64::from(band.q));
    let beta = 2.0 * a.sqrt() * alpha;
    match band.kind {
        EqKind::Peak => [
            1.0 + alpha * a,
            -2.0 * cos,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos,
            1.0 - alpha / a,
        ],
        EqKind::LowShelf => [
            a * ((a + 1.0) - (a - 1.0) * cos + beta),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
            a * ((a + 1.0) - (a - 1.0) * cos - beta),
            (a + 1.0) + (a - 1.0) * cos + beta,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos),
            (a + 1.0) + (a - 1.0) * cos - beta,
        ],
        EqKind::HighShelf => [
            a * ((a + 1.0) + (a - 1.0) * cos + beta),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
            a * ((a + 1.0) + (a - 1.0) * cos - beta),
            (a + 1.0) - (a - 1.0) * cos + beta,
            2.0 * ((a - 1.0) - (a + 1.0) * cos),
            (a + 1.0) - (a - 1.0) * cos - beta,
        ],
    }
}

/// The combined response of `bands` at `frequency` (dB) for one sample rate.
pub fn response_db(bands: &[EqBand], frequency: f64, rate: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * frequency / rate;
    let (s1, c1) = w.sin_cos();
    let (s2, c2) = (2.0 * w).sin_cos();
    bands
        .iter()
        .filter(|b| b.gain_db != 0.0)
        .map(|band| {
            let [b0, b1, b2, a0, a1, a2] = coefficients(band, rate);
            let num = (b0 + b1 * c1 + b2 * c2).powi(2) + (b1 * s1 + b2 * s2).powi(2);
            let den = (a0 + a1 * c1 + a2 * c2).powi(2) + (a1 * s1 + a2 * s2).powi(2);
            10.0 * (num / den).log10()
        })
        .sum()
}

/// How much louder `new` plays than `old` where it rises most, 20 Hz–20 kHz
/// (dB; negative when it is quieter everywhere). Each side is a preamp with
/// its filters, or None for no equalizer. As in the player, a preamp counts
/// only while one of its filters has a gain. Turning a profile off rises by
/// its preamp; a bass preset rises by its bass.
pub fn level_rise_db(old: Option<(f32, &[EqBand])>, new: Option<(f32, &[EqBand])>) -> f64 {
    const RATE: f64 = 48_000.0;
    let level = |eq: Option<(f32, &[EqBand])>, f: f64| {
        eq.filter(|(_, bands)| bands.iter().any(|b| b.gain_db != 0.0))
            .map_or(0.0, |(preamp, bands)| {
                f64::from(preamp.min(0.0)) + response_db(bands, f, RATE)
            })
    };
    let points = 600;
    let mut frequencies: Vec<f64> = (0..=points)
        .map(|i| 20.0 * 1000f64.powf(f64::from(i) / f64::from(points)))
        .collect();
    for (_, bands) in [old, new].into_iter().flatten() {
        frequencies.extend(bands.iter().map(|b| f64::from(b.frequency_hz)));
    }
    frequencies
        .into_iter()
        .map(|f| level(new, f) - level(old, f))
        .fold(f64::NEG_INFINITY, f64::max)
}

/// Source rates the filters run at (the graph filters before resampling).
/// Hi-res rates warp less near 20 kHz, so a combined peak there can sit up to
/// 0.45 dB above the 44.1/48 kHz one (Sony WF-1000XM4). Rates below 44.1 kHz
/// are left out: FFmpeg skips filters above their Nyquist frequency, and
/// covering that would lower many profiles by several dB for rare files; the
/// limiter catches those peaks.
const PEAK_RATES: [f64; 8] = [
    44_100.0, 48_000.0, 88_200.0, 96_000.0, 176_400.0, 192_000.0, 352_800.0, 384_000.0,
];

/// The highest point of the combined response from 10 Hz to just below the
/// Nyquist frequency at every source rate from 44.1 to 384 kHz (dB; 0 when
/// nothing lifts).
pub fn peak_gain_db(bands: &[EqBand]) -> f32 {
    peak_db(bands) as f32
}

fn peak_db(bands: &[EqBand]) -> f64 {
    let mut peak = 0f64;
    for rate in PEAK_RATES {
        let top = rate * 0.4999;
        let points = 1200;
        let mut frequencies: Vec<f64> = (0..=points)
            .map(|i| 10.0 * (top / 10.0f64).powf(f64::from(i) / f64::from(points)))
            .collect();
        frequencies.extend(bands.iter().map(|b| f64::from(b.frequency_hz)));
        for f in frequencies.into_iter().filter(|f| *f < top) {
            peak = peak.max(response_db(bands, f, rate));
        }
    }
    peak
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD650: &str = "Preamp: -6.1 dB
Filter 1: ON LSC Fc 105 Hz Gain 6.4 dB Q 0.70
Filter 2: ON PK Fc 8800 Hz Gain 5.1 dB Q 1.42
Filter 3: ON PK Fc 118 Hz Gain -3.1 dB Q 0.50
Filter 4: ON PK Fc 37 Hz Gain 0.7 dB Q 3.96
Filter 5: ON PK Fc 3169 Hz Gain -1.7 dB Q 3.89
Filter 6: ON HSC Fc 10000 Hz Gain -2.1 dB Q 0.70
Filter 7: ON PK Fc 1227 Hz Gain -1.2 dB Q 2.53
Filter 8: ON PK Fc 2055 Hz Gain 1.2 dB Q 3.23
Filter 9: ON PK Fc 587 Hz Gain 0.4 dB Q 1.19
Filter 10: ON PK Fc 5332 Hz Gain -1.1 dB Q 5.75
";

    fn band(kind: EqKind, f: f32, g: f32, q: f32) -> EqBand {
        EqBand {
            frequency_hz: f,
            gain_db: g,
            q,
            kind,
        }
    }

    #[test]
    fn autoeq_file_imports_all_ten_filters_with_their_shapes() {
        let p = parse("Sennheiser HD 650", HD650).unwrap();
        assert_eq!(p.bands.len(), 10);
        assert_eq!(p.bands[0], band(EqKind::LowShelf, 105., 6.4, 0.7));
        assert_eq!(p.bands[5], band(EqKind::HighShelf, 10_000., -2.1, 0.7));
        assert_eq!(p.bands[9], band(EqKind::Peak, 5332., -1.1, 5.75));
        // AutoEQ's preamp (-6.1) leaves the low shelf's 6.3 dB peak a little
        // above full scale over 10 Hz–Nyquist: lowered to cover it.
        let peak = peak_gain_db(&p.bands);
        assert!(peak > 6.1 && peak < 6.4, "{peak}");
        assert_eq!(p.preamp_db, -6.4);
        assert!(f64::from(p.preamp_db) + f64::from(peak) <= 0.0);
    }

    #[test]
    fn preamp_is_lowered_until_the_highest_point_stays_at_full_scale() {
        // The file under-states the boost: +9 dB peak needs at least -9 dB.
        let p = parse(
            "x",
            "Preamp: -2 dB\nFilter: ON PK Fc 1000 Hz Gain 9 dB Q 1\n",
        )
        .unwrap();
        assert!(p.preamp_db <= -9.0 && p.preamp_db > -9.2, "{}", p.preamp_db);
        assert!(f64::from(p.preamp_db) + response_db(&p.bands, 1000., 48_000.) <= 0.0);
        // No preamp line: computed. Only cuts: no boost is ever added.
        let p = parse("x", "Filter 1: ON LSC Fc 100 Hz Gain 4 dB Q 0.7\n").unwrap();
        assert!(p.preamp_db <= -4.0 && p.preamp_db > -4.2, "{}", p.preamp_db);
        let p = parse(
            "x",
            "Preamp: 3 dB\nFilter 1: ON PK Fc 100 Hz Gain -4 dB Q 0.7\n",
        )
        .unwrap();
        assert_eq!(p.preamp_db, 0.0);
    }

    #[test]
    fn filter_shapes_match_their_definitions() {
        let rate = 48_000.;
        let peak = [band(EqKind::Peak, 1000., 6., 1.)];
        assert!((response_db(&peak, 1000., rate) - 6.0).abs() < 1e-9);
        assert!(response_db(&peak, 20., rate).abs() < 0.05);
        let low = [band(EqKind::LowShelf, 200., -5., 0.7)];
        assert!((response_db(&low, 10., rate) + 5.0).abs() < 0.05);
        assert!(response_db(&low, 10_000., rate).abs() < 0.05);
        // Half the shelf gain at its centre frequency (RBJ definition).
        assert!((response_db(&low, 200., rate) + 2.5).abs() < 1e-6);
        let high = [band(EqKind::HighShelf, 4000., 3., 0.7)];
        assert!((response_db(&high, 23_000., rate) - 3.0).abs() < 0.05);
        assert!(response_db(&high, 50., rate).abs() < 0.05);
        assert!((response_db(&high, 4000., rate) - 1.5).abs() < 1e-6);
    }

    #[test]
    fn off_filters_comments_and_case_are_accepted() {
        let p = parse(
            "x",
            "# AutoEQ\npreamp: -1.5 dB\n\nFilter 1: OFF PK Fc 10 Hz Gain 99 dB Q 0\nfilter 2: on pk fc 300 hz gain -2 db q 1.41 # mids\n",
        )
        .unwrap();
        assert_eq!(p.bands, vec![band(EqKind::Peak, 300., -2., 1.41)]);
        assert_eq!(p.preamp_db, -1.5);
    }

    #[test]
    fn anything_that_would_play_differently_is_refused() {
        let cases = [
            ("", "no filters"),
            ("Preamp: -3 dB\n", "no filters"),
            ("Filter 1: ON LS Fc 100 Hz Gain 3 dB\n", "type LS"),
            ("Filter 1: ON PK Fc 100 Hz Gain 3 dB\n", "no Q"),
            ("Filter 1: ON PK Fc 100 Hz Q 1\n", "no Gain"),
            (
                "Filter 1: ON PK Fc 30000 Hz Gain 3 dB Q 1\n",
                "20 Hz–20 kHz",
            ),
            ("Filter 1: ON PK Fc 100 Hz Gain 40 dB Q 1\n", "±30 dB"),
            ("Filter 1: ON PK Fc 100 Hz Gain 3 dB Q 0\n", "Q 0"),
            (
                "Filter 1: ON PK Fc abc Hz Gain 3 dB Q 1\n",
                "Fc needs a number",
            ),
            (
                "Channel: L\nFilter 1: ON PK Fc 100 Hz Gain 3 dB Q 1\n",
                "Line 1 is not",
            ),
            ("GraphicEQ 20 0; 30 1\n", "Line 1 is not"),
            ("Preamp: -1 dB\nPreamp: -2 dB\n", "second Preamp"),
            ("Filter 1: MAYBE PK Fc 100 Hz Gain 3 dB Q 1\n", "ON or OFF"),
            (
                "Filter 1: ON PK Fc 100 Hz Gain 3 dB Q 1 Delay 4\n",
                "unexpected",
            ),
        ];
        for (text, needle) in cases {
            let error = parse("x", text).unwrap_err();
            assert!(error.contains(needle), "{text:?}: {error}");
        }
        let filters = |n: u32| -> String {
            (1..=n)
                .map(|i| format!("Filter {i}: ON PK Fc {} Hz Gain 1 dB Q 1\n", 100 * i))
                .collect()
        };
        // Y2Linux #76: more than AutoEQ's ten are taken, up to twenty.
        assert_eq!(parse("x", &filters(20)).unwrap().bands.len(), 20);
        assert!(parse("x", &filters(21)).unwrap_err().contains("21 filters"));
    }

    /// Every profile Reborn ships (assets/eq) reads with the real parser and
    /// keeps AutoEq's ten filters.
    #[test]
    fn shipped_profiles_all_import() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/eq");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            if !file.ends_with("ParametricEQ.txt") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let p = parse(&profile_name(&file), &text).unwrap_or_else(|e| panic!("{file}: {e}"));
            assert_eq!(p.bands.len(), 10, "{file}: AutoEQ writes ten");
            assert!(
                p.preamp_db <= 0.0 && p.preamp_db > -20.0,
                "{file}: {}",
                p.preamp_db
            );
            count += 1;
        }
        assert_eq!(count, 47);
    }

    fn shipped(file: &str) -> EqProfile {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/eq")
            .join(format!("{file} ParametricEQ.txt"));
        parse(file, &std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// The filters run at the source rate: the preamp must also cover a
    /// hi-res file, where the 44.1/48 kHz peak alone was 0.45 dB short.
    #[test]
    fn preamp_covers_hi_res_source_rates() {
        let p = shipped("Sony WF-1000XM4");
        assert_eq!(p.preamp_db, -8.1);
        for rate in [44_100.0, 96_000.0, 192_000.0] {
            for i in 0..=2000 {
                let f = 10.0 * (rate * 0.4999 / 10.0f64).powf(f64::from(i) / 2000.0);
                let level = f64::from(p.preamp_db) + response_db(&p.bands, f, rate);
                assert!(level <= 0.0, "{rate} Hz rate, {f:.0} Hz: {level:+.3} dB");
            }
        }
        // The rounding keeps a written preamp that already covers its filters.
        assert_eq!(shipped("Sennheiser HD 650").preamp_db, -6.4);
    }

    #[test]
    fn level_rise_finds_what_plays_louder() {
        let hd650 = shipped("Sennheiser HD 650");
        let profile = Some((hd650.preamp_db, hd650.bands.as_slice()));
        // Off: the preamp and the profile's deepest cut come back at once.
        let off = level_rise_db(profile, None);
        assert!(off > 8.0 && off < 9.0, "{off}");
        // On: a clip-safe profile is never louder than flat.
        assert!(level_rise_db(None, profile) <= 1e-9);
        assert_eq!(level_rise_db(profile, profile), 0.0);
        let bass = [band(EqKind::Peak, 60., 6., 1.)];
        let treble = [band(EqKind::Peak, 12_000., 6., 1.)];
        let rise = level_rise_db(None, Some((0.0, &bass)));
        assert!((rise - 6.0).abs() < 1e-6, "{rise}");
        // Moving a boost elsewhere rises there.
        let moved = level_rise_db(Some((0.0, &bass)), Some((0.0, &treble)));
        assert!((moved - 6.0).abs() < 0.01, "{moved}");
        // A preamp without a band with gain does nothing, as in the graph.
        let flat = [band(EqKind::Peak, 1000., 0., 1.)];
        assert_eq!(level_rise_db(Some((-12.0, &flat)), None), 0.0);
    }

    /// Settings re-sanitize the stored preamp at every start: doing it again
    /// must never step it further down.
    #[test]
    fn sanitizing_a_preamp_again_keeps_it() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/eq");
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            if let Some(name) = file.strip_suffix(" ParametricEQ.txt") {
                let p = shipped(name);
                let again = safe_preamp_db(Some(p.preamp_db), &p.bands);
                assert_eq!(again, p.preamp_db, "{name}");
                assert_eq!(safe_preamp_db(Some(again), &p.bands), again, "{name}");
            }
        }
        let cut = [band(EqKind::Peak, 1000., -3., 1.)];
        for tenths in 0..=600 {
            let preamp = -(tenths as f32) / 10.0;
            assert_eq!(safe_preamp_db(Some(preamp), &cut), preamp.max(-60.0));
        }
    }

    #[test]
    fn profile_names_come_from_the_file_name() {
        assert_eq!(
            profile_name("Sennheiser HD 650 ParametricEQ.txt"),
            "Sennheiser HD 650"
        );
        assert_eq!(
            profile_name("Moondrop Aria parametriceq.TXT"),
            "Moondrop Aria"
        );
        assert_eq!(profile_name("my_iem.txt"), "my_iem");
        assert_eq!(profile_name("ParametricEQ.txt"), "Profile");
    }
}
