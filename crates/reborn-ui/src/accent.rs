//! The colour a cover lends to Now Playing's backdrop, the Home cover card and the accent skins.
//!
//! The cover is reduced to 16 swatches (median cut, the way Android's Palette starts). The swatch
//! that covers most of the picture in a real colour wins and keeps its own hue and saturation;
//! only its lightness is raised, as far as text on the near-black background needs. A picture of
//! grey, black and white only gets a calm cool white.
use crate::theme::color;

/// The accent of a cover with no colour in it.
const NEUTRAL: u32 = 0xBECDE1FF;
const SWATCHES: usize = 16;
/// Every third pixel is enough: a 180 x 180 cover gives 10 800 samples.
const STEP: usize = 3;
/// A swatch is a colour when its HSL saturation and lightness are inside these bounds. Below is
/// grey, near-black or near-white, where the hue is noise (white paper is "saturated" in HSL).
const MIN_SATURATION: f32 = 0.25;
const MIN_LIGHTNESS: f32 = 0.06;
const MAX_LIGHTNESS: f32 = 0.9;
/// A picture of neutral swatches only can still have a signature colour: a vivid area (HSV) of
/// at least this share of the picture, such as red stamps on white paper (about 1.5 %).
const SIGNATURE_SHARE: f32 = 0.01;
const SIGNATURE_SATURATION: f32 = 0.5;
const SIGNATURE_VALUE: f32 = 0.35;
const HUES: usize = 12;
/// An accent starts at this lightness and is raised until text in it reads on the background.
const LIGHTNESS_FLOOR: f32 = 0.55;
const MIN_CONTRAST: f32 = 4.5;

struct Swatch {
    rgb: [f32; 3],
    count: usize,
}

/// Accent colour for an RGBA cover. Opaque pixels only; none or nothing but grey gives `NEUTRAL`.
pub fn accent_from_rgba(pixels: &[u8]) -> u32 {
    let samples: Vec<[u8; 3]> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .step_by(STEP)
        .filter(|p| p[3] >= 128)
        .map(|p| [p[0], p[1], p[2]])
        .collect();
    dominant(&median_cut(&samples))
        .or_else(|| signature(&samples))
        .map_or(NEUTRAL, readable)
}

/// A box of the cut: samples and the channel they differ most in (found once, when it is made).
struct Cell {
    samples: Vec<[u8; 3]>,
    channel: usize,
    spread: u8,
}

impl Cell {
    fn new(samples: Vec<[u8; 3]>) -> Self {
        let mut low = [255u8; 3];
        let mut high = [0u8; 3];
        for p in &samples {
            for c in 0..3 {
                low[c] = low[c].min(p[c]);
                high[c] = high[c].max(p[c]);
            }
        }
        let (channel, spread) = (0..3)
            .map(|c| (c, high[c].saturating_sub(low[c])))
            .fold((0, 0), |best, now| if now.1 > best.1 { now } else { best });
        Cell {
            samples,
            channel,
            spread,
        }
    }

    /// Cut at the median along the widest channel; the two halves.
    fn halve(mut self) -> (Cell, Cell) {
        sort_by_channel(&mut self.samples, self.channel);
        let upper = self.samples.split_off(self.samples.len() / 2);
        (Cell::new(self.samples), Cell::new(upper))
    }
}

/// The sample colours merged into at most `SWATCHES` boxes by repeatedly halving the most
/// populated one along its widest channel. A box of one colour is never split.
fn median_cut(samples: &[[u8; 3]]) -> Vec<Swatch> {
    let mut boxes = vec![Cell::new(samples.to_vec())];
    while boxes.len() < SWATCHES {
        let mut target: Option<(usize, usize)> = None;
        for (i, b) in boxes.iter().enumerate() {
            let populated = target.is_none_or(|(_, len)| b.samples.len() > len);
            if populated && b.samples.len() > 1 && b.spread > 0 {
                target = Some((i, b.samples.len()));
            }
        }
        let Some((at, _)) = target else {
            break;
        };
        let (lower, upper) = boxes.swap_remove(at).halve();
        boxes.push(lower);
        boxes.push(upper);
    }
    boxes
        .iter()
        .filter(|b| !b.samples.is_empty())
        .map(|b| {
            let mut sum = [0f32; 3];
            for p in &b.samples {
                for (s, c) in sum.iter_mut().zip(p) {
                    *s += f32::from(*c);
                }
            }
            Swatch {
                rgb: sum.map(|s| s / b.samples.len() as f32),
                count: b.samples.len(),
            }
        })
        .collect()
}

/// A stable sort by one channel: a counting sort, linear where a comparison sort of ten thousand
/// samples took 17 ms per cover on the player (measured on the player).
fn sort_by_channel(samples: &mut Vec<[u8; 3]>, channel: usize) {
    let mut start = [0usize; 256];
    for p in samples.iter() {
        start[usize::from(p[channel])] += 1;
    }
    let mut run = 0;
    for slot in &mut start {
        let n = *slot;
        *slot = run;
        run += n;
    }
    let mut sorted = vec![[0u8; 3]; samples.len()];
    for p in samples.iter() {
        let slot = &mut start[usize::from(p[channel])];
        sorted[*slot] = *p;
        *slot += 1;
    }
    *samples = sorted;
}

/// The colour swatch that matters most: its share of the picture, weighted by how colourful it is
/// (a vivid swatch beats a dull one of the same size, but a tiny vivid one does not beat a
/// large dull one).
fn dominant(swatches: &[Swatch]) -> Option<[f32; 3]> {
    let mut best: Option<(f32, [f32; 3])> = None;
    for s in swatches {
        let (_, saturation, lightness) = hsl(s.rgb);
        if saturation < MIN_SATURATION || !(MIN_LIGHTNESS..=MAX_LIGHTNESS).contains(&lightness) {
            continue;
        }
        let score = s.count as f32 * (0.3 + saturation);
        if best.is_none_or(|(top, _)| score > top) {
            best = Some((score, s.rgb));
        }
    }
    best.map(|(_, rgb)| rgb)
}

/// The mean colour of the most common vivid hue, when it covers `SIGNATURE_SHARE` of the picture.
fn signature(samples: &[[u8; 3]]) -> Option<[f32; 3]> {
    let mut count = [0usize; HUES];
    let mut sum = [[0f32; 3]; HUES];
    for p in samples {
        let rgb = p.map(f32::from);
        let max = rgb[0].max(rgb[1]).max(rgb[2]);
        let min = rgb[0].min(rgb[1]).min(rgb[2]);
        let saturation = if max == 0. { 0. } else { (max - min) / max };
        if saturation < SIGNATURE_SATURATION || max / 255. < SIGNATURE_VALUE {
            continue;
        }
        let hue = hue_of(rgb[0], rgb[1], rgb[2], max, min);
        let bucket = (hue / 360. * HUES as f32) as usize % HUES;
        count[bucket] += 1;
        for (s, c) in sum[bucket].iter_mut().zip(rgb) {
            *s += c;
        }
    }
    let mut best = 0;
    for (i, n) in count.iter().enumerate() {
        if *n > count[best] {
            best = i;
        }
    }
    let n = count[best];
    (n > 0 && n as f32 >= samples.len() as f32 * SIGNATURE_SHARE)
        .then(|| sum[best].map(|s| s / n as f32))
}

/// The colour with its hue and saturation kept and its lightness raised to `LIGHTNESS_FLOOR`, and
/// further until it has `MIN_CONTRAST` on the background. It is never made more vivid.
fn readable(rgb: [f32; 3]) -> u32 {
    let (hue, saturation, lightness) = hsl(rgb);
    let background = unpack(color::BG);
    let mut lightness = lightness.max(LIGHTNESS_FLOOR);
    loop {
        let out = from_hsl(hue, saturation, lightness);
        if lightness >= 1. || contrast(out, background) >= MIN_CONTRAST {
            return out.iter().fold(0, |packed, c| packed << 8 | u32::from(*c)) << 8 | 0xFF;
        }
        lightness = (lightness + 0.02).min(1.);
    }
}

fn unpack(rgba: u32) -> [u8; 3] {
    [(rgba >> 24) as u8, (rgba >> 16) as u8, (rgba >> 8) as u8]
}

/// WCAG contrast ratio of two colours.
fn contrast(a: [u8; 3], b: [u8; 3]) -> f32 {
    let luminance = |c: [u8; 3]| {
        let lin = |v: u8| {
            let v = f32::from(v) / 255.;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
    };
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn hue_of(r: f32, g: f32, b: f32, max: f32, min: f32) -> f32 {
    let d = max - min;
    if d == 0. {
        return 0.;
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.)
    } else if max == g {
        (b - r) / d + 2.
    } else {
        (r - g) / d + 4.
    };
    h * 60.
}

/// Hue in degrees, saturation and lightness (0..=1) of an RGB colour with channels 0..=255.
fn hsl(rgb: [f32; 3]) -> (f32, f32, f32) {
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let lightness = (max + min) / 510.;
    let d = max - min;
    if d == 0. {
        return (0., 0., lightness);
    }
    let saturation = d / (255. * (1. - (2. * lightness - 1.).abs()));
    (
        hue_of(rgb[0], rgb[1], rgb[2], max, min),
        saturation,
        lightness,
    )
}

fn from_hsl(hue: f32, saturation: f32, lightness: f32) -> [u8; 3] {
    let chroma = (1. - (2. * lightness - 1.).abs()) * saturation;
    let sector = hue / 60.;
    let x = chroma * (1. - (sector.rem_euclid(2.) - 1.).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, x, 0.),
        1 => (x, chroma, 0.),
        2 => (0., chroma, x),
        3 => (0., x, chroma),
        4 => (x, 0., chroma),
        _ => (chroma, 0., x),
    };
    let m = lightness - chroma / 2.;
    [r, g, b].map(|c| ((c + m) * 255.).round().clamp(0., 255.) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: usize = 160;

    fn picture(pixel: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        (0..SIZE * SIZE)
            .flat_map(|i| {
                let [r, g, b] = pixel(i % SIZE, i / SIZE);
                [r, g, b, 255]
            })
            .collect()
    }

    fn flat(r: u8, g: u8, b: u8) -> Vec<u8> {
        picture(|_, _| [r, g, b])
    }

    fn rgb(accent: u32) -> [u8; 3] {
        unpack(accent)
    }

    /// Hue family of an accent: its hue in degrees.
    fn hue(accent: u32) -> f32 {
        hsl(rgb(accent).map(f32::from)).0
    }

    fn is_red(accent: u32) -> bool {
        let h = hue(accent);
        !(15. ..345.).contains(&h)
    }

    #[test]
    fn accent_follows_the_cover_and_stays_readable() {
        let orange = accent_from_rgba(&flat(210, 110, 40));
        let [r, g, b] = rgb(orange);
        assert!(r > g && g > b, "orange stays orange: {orange:08x}");
        let teal = accent_from_rgba(&flat(30, 120, 130));
        assert!(rgb(teal)[2] > rgb(teal)[0], "teal stays blue-green");
    }

    #[test]
    fn every_accent_reads_on_the_background() {
        for step in 0..36 {
            let (hue, lightness) = (step as f32 * 10., 0.2 + (step % 4) as f32 * 0.15);
            let [r, g, b] = from_hsl(hue, 0.9, lightness);
            let accent = accent_from_rgba(&flat(r, g, b));
            if accent != NEUTRAL {
                let ratio = contrast(rgb(accent), unpack(color::BG));
                assert!(ratio >= MIN_CONTRAST, "hue {hue}: {ratio} for {accent:08x}");
            }
        }
    }

    #[test]
    fn a_dark_cover_with_red_lettering_is_red_not_neutral() {
        // Dark maroon ground (three quarters of the picture), red lettering on it.
        let cover = picture(|x, y| {
            if (50..70).contains(&y) && x % 4 != 0 {
                [205, 30, 45]
            } else if x < 40 {
                [60, 55, 56]
            } else {
                [42, 4, 8]
            }
        });
        let accent = accent_from_rgba(&cover);
        assert_ne!(accent, NEUTRAL);
        assert!(is_red(accent), "{accent:08x}");
    }

    #[test]
    fn white_paper_with_small_red_stamps_takes_the_red() {
        // Paper and grey scribble; red stamps on about 1.5 % of the picture.
        let cover = picture(|x, y| {
            if (30..34).contains(&y) && (20..80).contains(&x) {
                [200, 25, 45]
            } else if (x / 8 + y / 8) % 3 == 0 {
                [150, 160, 175]
            } else {
                [236, 233, 237]
            }
        });
        let accent = accent_from_rgba(&cover);
        assert_ne!(accent, NEUTRAL);
        assert!(is_red(accent), "{accent:08x}");
    }

    #[test]
    fn a_muted_brown_stays_brown_and_is_not_made_vivid() {
        let accent = accent_from_rgba(&flat(110, 80, 60));
        let [r, g, b] = rgb(accent);
        assert!(r > g && g > b, "{accent:08x}");
        let saturation = f32::from(r - b) / f32::from(r);
        assert!(
            saturation < 0.45,
            "not lifted to a loud orange: {accent:08x}"
        );
    }

    #[test]
    fn the_large_blue_area_wins_over_a_small_gold_one() {
        let cover = picture(|x, y| {
            if x < 100 {
                [60, 110, 190]
            } else if y < 12 {
                [220, 170, 40]
            } else {
                [30, 34, 40]
            }
        });
        let [r, _, b] = rgb(accent_from_rgba(&cover));
        assert!(b > r, "blue");
    }

    #[test]
    fn grey_black_white_and_empty_covers_get_the_neutral_accent() {
        assert_eq!(accent_from_rgba(&flat(110, 112, 114)), NEUTRAL);
        assert_eq!(accent_from_rgba(&flat(8, 8, 8)), NEUTRAL);
        assert_eq!(accent_from_rgba(&flat(240, 240, 240)), NEUTRAL);
        assert_eq!(accent_from_rgba(&[]), NEUTRAL);
        let clear: Vec<u8> = flat(200, 40, 40)
            .chunks(4)
            .flat_map(|p| [p[0], p[1], p[2], 0])
            .collect();
        assert_eq!(
            accent_from_rgba(&clear),
            NEUTRAL,
            "transparent pixels do not count"
        );
    }

    #[test]
    fn median_cut_keeps_two_distinct_colours_apart() {
        let cover = picture(|x, _| if x < 80 { [200, 30, 30] } else { [30, 30, 200] });
        let samples: Vec<[u8; 3]> = cover.chunks(4).map(|p| [p[0], p[1], p[2]]).collect();
        let swatches = median_cut(&samples);
        assert_eq!(
            swatches.len(),
            2,
            "a box of one colour is not split further"
        );
        assert_eq!(
            swatches.iter().map(|s| s.count).sum::<usize>(),
            samples.len()
        );
    }
}
