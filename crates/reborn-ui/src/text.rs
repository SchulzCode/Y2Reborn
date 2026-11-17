//! Text metrics over the exact-size Open Sans glyph sets (`fontdata`).
//!
//! Every `Face` is one weight at one pixel size, rendered once at build time,
//! so glyphs are drawn 1:1 on the pixel grid. Characters a face lacks fall back
//! to the 24 px page, scaled; then to the CJK font, rasterised at the face's
//! size at runtime (`glyph_cache`); characters nobody has become U+FFFD.
use crate::{
    fontdata::{self, Glyph, SetData},
    glyph_cache::{self, Runtime},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    R11,
    R13,
    R15,
    R17,
    L23,
    L44,
    S10,
    S13,
    S15,
    S17,
    S20,
    S25,
    S28,
    S36,
}

pub const FALLBACK_PX: f32 = 24.;
pub const FALLBACK_CELL: f32 = 32.;
pub const FALLBACK_PAD: f32 = 2.;
pub const FALLBACK_ASCENT: f32 = 25.;

impl Face {
    pub fn set(self) -> &'static SetData {
        use fontdata::*;
        match self {
            Self::R11 => &R11,
            Self::R13 => &R13,
            Self::R15 => &R15,
            Self::R17 => &R17,
            Self::L23 => &L23,
            Self::L44 => &L44,
            Self::S10 => &S10,
            Self::S13 => &S13,
            Self::S15 => &S15,
            Self::S17 => &S17,
            Self::S20 => &S20,
            Self::S25 => &S25,
            Self::S28 => &S28,
            Self::S36 => &S36,
        }
    }
    pub fn px(self) -> f32 {
        f32::from(self.set().size)
    }
    /// Pixels from the baseline to the vertical middle of a line of this
    /// face (the midpoint of the ascender and descender, as the design
    /// mock-ups centre text).
    pub fn mid_offset(self) -> f32 {
        (self.px() * 0.388).round()
    }
}

/// Where a character is drawn from.
#[derive(Clone, Copy, Debug)]
pub enum Source {
    /// An exact-size glyph in `face`'s page.
    Exact(&'static Glyph),
    /// A scaled 24 px glyph from the fallback page.
    Fallback(&'static Glyph),
    /// A CJK-font glyph at the face's pixel size, rasterised on first draw.
    Runtime(Runtime),
}

fn find(glyphs: &'static [Glyph], ch: char) -> Option<&'static Glyph> {
    glyphs
        .binary_search_by_key(&(ch as u32), |g| g.0)
        .ok()
        .map(|i| &glyphs[i])
}

pub fn lookup(face: Face, ch: char) -> Option<Source> {
    if let Some(g) = find(face.set().glyphs, ch) {
        return Some(Source::Exact(g));
    }
    if let Some(g) = find(fontdata::FALLBACK, ch) {
        return Some(Source::Fallback(g));
    }
    if let Some(g) = glyph_cache::lookup(ch, face.set().size) {
        return Some(Source::Runtime(g));
    }
    find(fontdata::FALLBACK, '\u{FFFD}').map(Source::Fallback)
}

/// Pen advance of `ch` in pixels.
pub fn advance(face: Face, ch: char) -> f32 {
    match lookup(face, ch) {
        Some(Source::Exact(g)) => f32::from(g.3) / 64.,
        Some(Source::Fallback(g)) => f32::from(g.3) / 64. * face.px() / FALLBACK_PX,
        Some(Source::Runtime(g)) => g.advance,
        None => 0.,
    }
}

pub fn width_tracked(face: Face, text: &str, tracking: f32) -> f32 {
    let mut total = 0f32;
    let mut count = 0f32;
    for ch in text.chars().filter(|c| !c.is_control()) {
        total += advance(face, ch);
        count += 1.;
    }
    total + tracking * (count - 1.).max(0.)
}

pub fn width(face: Face, text: &str) -> f32 {
    width_tracked(face, text, 0.)
}

/// `text` cut to `max_width` with a trailing ellipsis when it does not fit.
pub fn fit(face: Face, text: &str, max_width: f32) -> String {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(1024)
        .collect();
    if width(face, &clean) <= max_width {
        return clean;
    }
    let ellipsis = advance(face, '…');
    let mut out = String::new();
    let mut used = ellipsis;
    for ch in clean.chars() {
        used += advance(face, ch);
        if used > max_width {
            break;
        }
        out.push(ch);
    }
    if max_width >= ellipsis {
        out.push('…');
    }
    out
}

/// Split `text` into at most `lines` lines of at most `max_width`; the last
/// line is cut with an ellipsis when text remains.
pub fn wrap(face: Face, text: &str, max_width: f32, lines: usize) -> Vec<String> {
    let mut out = vec![];
    let mut remaining = text.trim();
    while !remaining.is_empty() && out.len() < lines {
        if out.len() == lines - 1 || width(face, remaining) <= max_width {
            out.push(fit(face, remaining, max_width));
            break;
        }
        let mut split = remaining.len();
        let mut last_space = 0;
        let mut used = 0.;
        for (i, ch) in remaining.char_indices() {
            used += advance(face, ch);
            if ch == ' ' {
                last_space = i;
            }
            if used > max_width {
                split = if last_space > 0 { last_space } else { i };
                break;
            }
        }
        if split == 0 {
            break;
        }
        out.push(remaining[..split].trim_end().to_owned());
        remaining = remaining[split..].trim_start();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_faces_draw_the_czech_alphabet_in_their_own_weight() {
        // Song, album and artist names use S25 (Now Playing), S20 and S10. A letter they lack
        // is drawn from the lighter 24 px fallback page, scaled, and looks blurred.
        for face in [Face::S10, Face::S20, Face::S25, Face::R15, Face::S15] {
            // Every letter Czech adds to ASCII, and the acute vowels.
            for ch in "ČčĎďĚěŇňŘřŠšŤťŮůŽž".chars().chain("ÁáÉéÍíÓóÚúÝý".chars())
            {
                assert!(
                    matches!(lookup(face, ch), Some(Source::Exact(_))),
                    "{ch} in {face:?}"
                );
            }
        }
    }

    #[test]
    fn exact_sets_cover_latin_and_fall_back_for_the_rest() {
        assert!(matches!(lookup(Face::R15, 'a'), Some(Source::Exact(_))));
        assert!(matches!(lookup(Face::R15, 'é'), Some(Source::Exact(_))));
        assert!(matches!(lookup(Face::R15, 'ł'), Some(Source::Exact(_))));
        assert!(matches!(lookup(Face::R15, 'Ж'), Some(Source::Fallback(_))));
        assert!(matches!(lookup(Face::S36, 'a'), Some(Source::Fallback(_))));
        // CJK comes from the CJK font at the face's size; only without the
        // font (host, older image) does it show the replacement glyph.
        if glyph_cache::available() {
            assert!(matches!(lookup(Face::R15, '音'), Some(Source::Runtime(g)) if g.px == 15));
        } else {
            assert!(matches!(lookup(Face::R15, '音'), Some(Source::Fallback(g)) if g.0 == 0xFFFD));
        }
        // Characters no font has still show the replacement glyph.
        assert!(
            matches!(lookup(Face::R15, '\u{10FFFD}'), Some(Source::Fallback(g)) if g.0 == 0xFFFD)
        );
    }

    #[test]
    fn cjk_widths_fit_and_wrap_with_the_font() {
        if !glyph_cache::available() {
            eprintln!("CJK font not available; skipped");
            return;
        }
        // Ideographs and kana advance one em; Hangul 0.92 em.
        assert!((width(Face::R15, "宇多田ヒカル") - 90.).abs() < 0.1);
        assert!((width(Face::S20, "아이유") - 3. * 18.4).abs() < 0.1);
        let mixed = width(Face::R15, "残酷な天使のテーゼ (Director's Edit)");
        assert!(mixed > width(Face::R15, "残酷な天使のテーゼ") + width(Face::R15, "(Director's"));
        let cut = fit(
            Face::R15,
            "「残酷な天使のテーゼ」とても長いタイトルの曲です",
            120.,
        );
        assert!(
            cut.ends_with('…') && width(Face::R15, &cut) <= 120.,
            "{cut}"
        );
        assert_eq!(cut.chars().count(), 8, "{cut}");
        // No spaces: lines break between characters, each within the box.
        let lines = wrap(Face::S25, "残酷な天使のテーゼ魂のルフラン", 150., 2);
        assert_eq!(lines.len(), 2);
        assert!(
            lines.iter().all(|l| width(Face::S25, l) <= 150.),
            "{lines:?}"
        );
        assert_eq!(lines[0], "残酷な天使の");
    }

    #[test]
    fn widths_grow_with_size_and_tracking() {
        assert!(width(Face::R11, "Reborn") < width(Face::R15, "Reborn"));
        assert!(width_tracked(Face::S10, "ARTISTS", 1.5) > width(Face::S10, "ARTISTS"));
        assert_eq!(width(Face::R13, ""), 0.);
    }

    #[test]
    fn fit_and_wrap_respect_the_box() {
        let long = "A very long title that extends well beyond the edge of a small player";
        let cut = fit(Face::R15, long, 200.);
        assert!(cut.ends_with('…') && width(Face::R15, &cut) <= 200.);
        let lines = wrap(Face::S25, long, 220., 2);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| width(Face::S25, l) <= 220.));
        assert_eq!(fit(Face::R15, "Short", 200.), "Short");
    }
}
