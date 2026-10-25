//! `skin.json` to a checked, drawable `Skin`. Pure: every file is fetched
//! through `Files`, so the caller owns reading, path containment and decoding.
use super::{
    atlas, Align, ArtSource, Bx, Case, Cond, El, Files, FontRef, IconRef, Level, ListSource,
    NativePart, Node, Paint, Palette, Scroll, Seg, Skin, Sliced, FLAGS, FONT_FILE_MAX, FORMAT,
    LAYOUTS, MANIFEST, MAX_ELEMENTS, MAX_FONTS, MAX_PICTURES, PICTURE_MAX, TOKENS, VARS,
};
use crate::{canvas::Icon, text::Face};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Text must reach this contrast on the skin's background; below the
/// recommended level a skin still loads but carries a warning.
const CONTRAST_MIN: f32 = 3.0;
const CONTRAST_RECOMMENDED: f32 = 4.5;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    author: String,
    #[serde(default)]
    #[allow(dead_code)]
    version: String,
    #[serde(default)]
    #[allow(dead_code)]
    description: String,
    #[serde(default)]
    palette: BTreeMap<String, String>,
    #[serde(default)]
    colors: BTreeMap<String, String>,
    #[serde(default)]
    fonts: BTreeMap<String, FontSpec>,
    #[serde(default)]
    images: BTreeMap<String, String>,
    #[serde(default)]
    parts: BTreeMap<String, Vec<Spec>>,
    #[serde(default)]
    screens: BTreeMap<String, Vec<Spec>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FontSpec {
    file: String,
    size: f32,
    /// "latin" (default) or "ascii": only printable ASCII, for large display
    /// sizes; anything else falls back to Reborn's font.
    #[serde(default)]
    charset: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ScrollSpec {
    x: f32,
    w: f32,
    track: Option<String>,
    thumb: Option<String>,
}

/// One element as written; which fields a type takes is checked in `compile`.
#[derive(Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
struct Spec {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "if")]
    when: Option<String>,
    x: Option<f32>,
    y: Option<f32>,
    w: Option<f32>,
    h: Option<f32>,
    size: Option<f32>,
    radius: Option<f32>,
    stroke: Option<f32>,
    lines: Option<f32>,
    line_height: Option<f32>,
    tracking: Option<f32>,
    knob_radius: Option<f32>,
    rows: Option<f32>,
    row_height: Option<f32>,
    color: Option<String>,
    top: Option<String>,
    bottom: Option<String>,
    image: Option<String>,
    tint: Option<String>,
    text: Option<String>,
    font: Option<String>,
    align: Option<String>,
    case: Option<String>,
    icon: Option<String>,
    value: Option<String>,
    track: Option<String>,
    fill: Option<String>,
    knob: Option<String>,
    track_image: Option<String>,
    fill_image: Option<String>,
    source: Option<String>,
    name: Option<String>,
    part: Option<String>,
    slice: Option<[u16; 4]>,
    vertical: Option<bool>,
    recolor: Option<bool>,
    row: Option<Vec<Spec>>,
    focused: Option<Vec<Spec>>,
    elements: Option<Vec<Spec>>,
    scrollbar: Option<ScrollSpec>,
}

macro_rules! present {
    ($spec:expr, $($field:ident),*) => {{
        let mut out: Vec<&'static str> = vec![];
        $(if $spec.$field.is_some() { out.push(stringify!($field)); })*
        out
    }};
}

impl Spec {
    /// Fields given besides `type` and `if`.
    fn given(&self) -> Vec<&'static str> {
        present!(
            self,
            x,
            y,
            w,
            h,
            size,
            radius,
            stroke,
            lines,
            line_height,
            tracking,
            knob_radius,
            rows,
            row_height,
            color,
            top,
            bottom,
            image,
            tint,
            text,
            font,
            align,
            case,
            icon,
            value,
            track,
            fill,
            knob,
            track_image,
            fill_image,
            source,
            name,
            part,
            slice,
            vertical,
            recolor,
            row,
            focused,
            elements,
            scrollbar
        )
    }
}

/// Fields each element type takes, besides `type` and `if`.
fn fields(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "rect" => &["x", "y", "w", "h", "color", "radius", "stroke"],
        "gradient" => &["x", "y", "w", "h", "top", "bottom"],
        "image" => &["x", "y", "w", "h", "image", "slice", "tint"],
        "text" => &[
            "x",
            "y",
            "w",
            "h",
            "text",
            "font",
            "color",
            "align",
            "lines",
            "line_height",
            "tracking",
            "case",
        ],
        "icon" => &["x", "y", "size", "icon", "color"],
        "bar" => &[
            "x",
            "y",
            "w",
            "h",
            "value",
            "track",
            "fill",
            "radius",
            "vertical",
            "knob",
            "knob_radius",
            "track_image",
            "fill_image",
            "slice",
        ],
        "art" => &["x", "y", "size", "source", "radius"],
        "list" => &[
            "x",
            "y",
            "w",
            "rows",
            "row_height",
            "source",
            "row",
            "focused",
            "scrollbar",
        ],
        "group" => &["x", "y", "elements"],
        "part" => &["name", "x", "y"],
        "native" => &["part", "x", "y", "recolor"],
        _ => return None,
    })
}

/// Parse `#RRGGBB` / `#RRGGBBAA`.
pub(crate) fn hex(s: &str) -> Option<u32> {
    let h = s.strip_prefix('#')?;
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match h.len() {
        6 => u32::from_str_radix(h, 16).ok().map(|v| v << 8 | 0xFF),
        8 => u32::from_str_radix(h, 16).ok(),
        _ => None,
    }
}

/// WCAG relative luminance of an RGBA colour.
fn luminance(c: u32) -> f32 {
    let ch = |shift: u32| {
        let v = ((c >> shift) & 0xFF) as f32 / 255.;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(24) + 0.7152 * ch(16) + 0.0722 * ch(8)
}

pub(crate) fn contrast(a: u32, b: u32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn builtin_face(name: &str) -> Option<Face> {
    Some(match name {
        "R11" => Face::R11,
        "R13" => Face::R13,
        "R15" => Face::R15,
        "R17" => Face::R17,
        "L23" => Face::L23,
        "L44" => Face::L44,
        "S10" => Face::S10,
        "S13" => Face::S13,
        "S15" => Face::S15,
        "S17" => Face::S17,
        "S20" => Face::S20,
        "S25" => Face::S25,
        "S28" => Face::S28,
        "S36" => Face::S36,
        _ => return None,
    })
}

pub(crate) const ICONS: &[(&str, Icon)] = &[
    ("back", Icon::Back),
    ("chevron", Icon::Chevron),
    ("check", Icon::Check),
    ("close", Icon::Close),
    ("play", Icon::Play),
    ("pause", Icon::Pause),
    ("prev", Icon::Prev),
    ("next", Icon::Next),
    ("play_large", Icon::PlayLarge),
    ("pause_large", Icon::PauseLarge),
    ("prev_large", Icon::PrevLarge),
    ("next_large", Icon::NextLarge),
    ("shuffle", Icon::Shuffle),
    ("repeat", Icon::Repeat),
    ("repeat_one", Icon::RepeatOne),
    ("gapless", Icon::Gapless),
    ("eq", Icon::Eq),
    ("normalise", Icon::Normalise),
    ("moon", Icon::Moon),
    ("bluetooth", Icon::Bluetooth),
    ("wifi", Icon::Wifi),
    ("lock", Icon::Lock),
    ("bolt", Icon::Bolt),
    ("folder", Icon::Folder),
    ("note", Icon::Note),
    ("speaker", Icon::Speaker),
    ("headphones", Icon::Headphones),
    ("sd", Icon::Sd),
    ("usb", Icon::Usb),
    ("warning", Icon::Warning),
    ("info", Icon::Info),
    ("sun", Icon::Sun),
    ("drive", Icon::Drive),
    ("power", Icon::Power),
    ("download", Icon::Download),
    ("refresh", Icon::Refresh),
    ("search", Icon::Search),
    ("vinyl", Icon::Vinyl),
    ("person", Icon::Person),
    ("crossfade", Icon::Crossfade),
    ("heart", Icon::HeartOn),
    ("palette", Icon::Palette),
];

pub(crate) fn token(p: &Palette, name: &str) -> Option<u32> {
    Some(match name {
        "bg" => p.bg,
        "text" => p.text,
        "muted" => p.muted,
        "dim" => p.dim,
        "glass" => p.glass,
        "scrim" => p.scrim,
        "sheet" => p.sheet,
        "track" => p.track,
        "danger" => p.danger,
        "success" => p.success,
        "warning" => p.warning,
        _ => return None,
    })
}

fn condition(s: &str, at: &str, row: bool) -> Result<Cond, String> {
    let mut out = vec![];
    for alt in s.split('|') {
        let mut all = vec![];
        for term in alt.split('&') {
            let term = term.trim();
            let (neg, name) = match term.strip_prefix('!') {
                Some(n) => (true, n.trim()),
                None => (false, term),
            };
            let (_, flag, row_only) = FLAGS
                .iter()
                .find(|(n, _, _)| *n == name)
                .ok_or_else(|| format!("{at}: unknown condition \"{name}\""))?;
            if *row_only && !row {
                return Err(format!("{at}: \"{name}\" is only known inside list rows"));
            }
            all.push((neg, *flag));
        }
        out.push(all);
    }
    Ok(out)
}

fn template(s: &str, at: &str, row: bool) -> Result<Vec<Seg>, String> {
    let mut out = vec![];
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        if open > 0 {
            out.push(Seg::Lit(rest[..open].to_owned()));
        }
        let close = rest[open..]
            .find('}')
            .ok_or_else(|| format!("{at}: \"{{\" without \"}}\" in \"{s}\""))?
            + open;
        let name = &rest[open + 1..close];
        let (_, var, row_only) = VARS
            .iter()
            .find(|(n, _, _)| *n == name)
            .ok_or_else(|| format!("{at}: unknown value \"{{{name}}}\""))?;
        if *row_only && !row {
            return Err(format!(
                "{at}: \"{{{name}}}\" is only known inside list rows"
            ));
        }
        out.push(Seg::Var(*var));
        rest = &rest[close + 1..];
    }
    if !rest.is_empty() {
        out.push(Seg::Lit(rest.to_owned()));
    }
    Ok(out)
}

/// A file named in the manifest must be a plain relative path (no `..`, no
/// root, no hidden tricks); the reader also checks where it really lands.
pub fn relative_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

struct Compiler<'a> {
    palette: Palette,
    colors: &'a BTreeMap<String, String>,
    fonts: HashMap<String, usize>,
    pictures: HashMap<String, [u16; 4]>,
    parts: &'a BTreeMap<String, Vec<Spec>>,
    elements: usize,
    stack: Vec<String>,
    used: HashSet<String>,
}

impl Compiler<'_> {
    fn paint(&self, s: &str, at: &str) -> Result<Paint, String> {
        let (name, percent) = match s.split_once('/') {
            Some((n, p)) => {
                let p: u32 = p
                    .trim()
                    .parse()
                    .ok()
                    .filter(|p| *p <= 100)
                    .ok_or_else(|| format!("{at}: opacity in \"{s}\" must be 0 to 100"))?;
                (n.trim(), Some(p))
            }
            None => (s.trim(), None),
        };
        let scale = |c: u32| match percent {
            Some(p) => (c & 0xFFFF_FF00) | ((c & 0xFF) * p / 100),
            None => c,
        };
        if name == "accent" {
            return Ok(match self.palette.accent {
                Some(c) => Paint::Fixed(scale(c)),
                None => Paint::Accent(scale(0xFF) as u8),
            });
        }
        if let Some(c) = hex(name) {
            return Ok(Paint::Fixed(scale(c)));
        }
        if let Some(value) = self.colors.get(name) {
            let c = hex(value)
                .ok_or_else(|| format!("colors.{name}: \"{value}\" is not #RRGGBB or #RRGGBBAA"))?;
            return Ok(Paint::Fixed(scale(c)));
        }
        if let Some(c) = token(&self.palette, name) {
            return Ok(Paint::Fixed(scale(c)));
        }
        Err(format!("{at}: unknown colour \"{s}\""))
    }

    fn list(&mut self, items: &[Spec], at: &str, row: bool) -> Result<Vec<Node>, String> {
        items
            .iter()
            .enumerate()
            .map(|(i, e)| self.compile(e, &format!("{at}[{i}]"), row))
            .collect()
    }

    fn picture(&self, name: &str, at: &str) -> Result<[u16; 4], String> {
        self.pictures
            .get(name)
            .copied()
            .ok_or_else(|| format!("{at}: no image named \"{name}\" in \"images\""))
    }

    fn compile(&mut self, e: &Spec, at: &str, row: bool) -> Result<Node, String> {
        self.elements += 1;
        if self.elements > MAX_ELEMENTS {
            return Err(format!("more than {MAX_ELEMENTS} elements"));
        }
        let kind = e.kind.as_str();
        let allowed = fields(kind).ok_or_else(|| format!("{at}: unknown type \"{kind}\""))?;
        if let Some(extra) = e.given().into_iter().find(|f| !allowed.contains(f)) {
            return Err(format!("{at}: \"{kind}\" has no field \"{extra}\""));
        }
        let at = format!("{at} ({kind})");
        let num = |v: Option<f32>, key: &str, default: Option<f32>| -> Result<f32, String> {
            match v {
                Some(v) if v.is_finite() && v.abs() <= 4096. => Ok(v),
                Some(_) => Err(format!("{at}: \"{key}\" is out of range")),
                None => default.ok_or_else(|| format!("{at}: \"{key}\" is missing")),
            }
        };
        let required = |v: &Option<String>, key: &str| -> Result<String, String> {
            v.clone()
                .ok_or_else(|| format!("{at}: \"{key}\" is missing"))
        };
        let bx = || -> Result<Bx, String> {
            let b = Bx {
                x: num(e.x, "x", Some(0.))?,
                y: num(e.y, "y", Some(0.))?,
                w: num(e.w, "w", None)?,
                h: num(e.h, "h", None)?,
            };
            if b.w < 0. || b.h < 0. {
                return Err(format!("{at}: negative size"));
            }
            Ok(b)
        };
        let when = e
            .when
            .as_deref()
            .map(|w| condition(w, &at, row))
            .transpose()?;
        let slice_for = |src: [u16; 4], name: &str| -> Result<Option<[u16; 4]>, String> {
            if let Some(s) = e.slice {
                if s[0] + s[2] >= src[2] || s[1] + s[3] >= src[3] {
                    return Err(format!("{at}: \"slice\" leaves no middle in \"{name}\""));
                }
            }
            Ok(e.slice)
        };
        let el = match kind {
            "rect" => El::Rect {
                b: bx()?,
                paint: self.paint(&required(&e.color, "color")?, &at)?,
                radius: num(e.radius, "radius", Some(0.))?.clamp(0., 512.),
                stroke: num(e.stroke, "stroke", Some(0.))?.clamp(0., 3.),
            },
            "gradient" => El::Gradient {
                b: bx()?,
                top: self.paint(&required(&e.top, "top")?, &at)?,
                bottom: self.paint(&required(&e.bottom, "bottom")?, &at)?,
            },
            "image" => {
                let name = required(&e.image, "image")?;
                let src = self.picture(&name, &at)?;
                El::Picture {
                    b: bx()?,
                    src,
                    slice: slice_for(src, &name)?,
                    tint: match &e.tint {
                        Some(t) => self.paint(t, &at)?,
                        None => Paint::Fixed(0xFFFF_FFFF),
                    },
                }
            }
            "text" => {
                let name = e.font.as_deref().unwrap_or("R15");
                let font = match builtin_face(name) {
                    Some(f) => FontRef::Builtin(f),
                    None => FontRef::Skin(*self.fonts.get(name).ok_or_else(|| {
                        format!(
                            "{at}: no font named \"{name}\" (use one from \"fonts\" or R11…S36)"
                        )
                    })?),
                };
                let lines = num(e.lines, "lines", Some(1.))?;
                if !(1. ..=6.).contains(&lines) || lines.fract() != 0. {
                    return Err(format!("{at}: \"lines\" is 1 to 6"));
                }
                El::Text {
                    b: bx()?,
                    text: template(&required(&e.text, "text")?, &at, row)?,
                    font,
                    paint: self.paint(e.color.as_deref().unwrap_or("text"), &at)?,
                    align: match e.align.as_deref().unwrap_or("left") {
                        "left" => Align::Left,
                        "center" => Align::Center,
                        "right" => Align::Right,
                        other => {
                            return Err(format!("{at}: align \"{other}\" is left, center or right"))
                        }
                    },
                    lines: lines as usize,
                    line: num(e.line_height, "line_height", Some(0.))?.clamp(0., 200.),
                    tracking: num(e.tracking, "tracking", Some(0.))?.clamp(-4., 20.),
                    case: match e.case.as_deref() {
                        None | Some("normal") => Case::Normal,
                        Some("upper") => Case::Upper,
                        Some("lower") => Case::Lower,
                        Some(other) => {
                            return Err(format!("{at}: case \"{other}\" is normal, upper or lower"))
                        }
                    },
                }
            }
            "icon" => {
                let name = required(&e.icon, "icon")?;
                let icon = match name.as_str() {
                    "play_pause" => IconRef::PlayPause { large: false },
                    "play_pause_large" => IconRef::PlayPause { large: true },
                    "repeat_mode" => IconRef::RepeatMode,
                    _ => IconRef::Fixed(
                        ICONS
                            .iter()
                            .find(|(n, _)| *n == name)
                            .map(|(_, i)| *i)
                            .ok_or_else(|| format!("{at}: unknown icon \"{name}\""))?,
                    ),
                };
                El::Icon {
                    x: num(e.x, "x", Some(0.))?,
                    y: num(e.y, "y", Some(0.))?,
                    size: num(e.size, "size", None)?.clamp(1., 360.),
                    icon,
                    paint: self.paint(e.color.as_deref().unwrap_or("text"), &at)?,
                }
            }
            "bar" => {
                let pic = |c: &Self, v: &Option<String>| -> Result<Option<Sliced>, String> {
                    match v {
                        Some(name) => {
                            let src = c.picture(name, &at)?;
                            Ok(Some((src, slice_for(src, name)?)))
                        }
                        None => Ok(None),
                    }
                };
                El::Bar {
                    b: bx()?,
                    level: match required(&e.value, "value")?.as_str() {
                        "progress" => Level::Progress,
                        "volume" => Level::Volume,
                        "battery" => Level::Battery,
                        "brightness" => Level::Brightness,
                        other => {
                            return Err(format!(
                                "{at}: value \"{other}\" is progress, volume, battery or brightness"
                            ))
                        }
                    },
                    track: e.track.as_deref().map(|t| self.paint(t, &at)).transpose()?,
                    fill: self.paint(e.fill.as_deref().unwrap_or("accent"), &at)?,
                    radius: num(e.radius, "radius", Some(0.))?.clamp(0., 512.),
                    vertical: e.vertical.unwrap_or(false),
                    knob: match &e.knob {
                        Some(k) => Some((
                            self.paint(k, &at)?,
                            num(e.knob_radius, "knob_radius", Some(6.))?.clamp(1., 64.),
                        )),
                        None => None,
                    },
                    track_picture: pic(self, &e.track_image)?,
                    fill_picture: pic(self, &e.fill_image)?,
                }
            }
            "art" => {
                let size = num(e.size, "size", None)?.clamp(1., 360.);
                El::Art {
                    b: Bx {
                        x: num(e.x, "x", Some(0.))?,
                        y: num(e.y, "y", Some(0.))?,
                        w: size,
                        h: size,
                    },
                    source: match e.source.as_deref().unwrap_or("current") {
                        "current" => ArtSource::Current,
                        "row" if row => ArtSource::Row,
                        "row" => return Err(format!("{at}: source \"row\" is only for list rows")),
                        other => return Err(format!("{at}: source \"{other}\" is current or row")),
                    },
                    radius: num(e.radius, "radius", Some(0.))?.clamp(0., 180.),
                }
            }
            "list" => {
                if row {
                    return Err(format!("{at}: a list cannot be inside a list row"));
                }
                let rows = num(e.rows, "rows", None)?;
                if !(1. ..=24.).contains(&rows) || rows.fract() != 0. {
                    return Err(format!("{at}: \"rows\" is 1 to 24"));
                }
                let row_h = num(e.row_height, "row_height", None)?;
                if !(8. ..=360.).contains(&row_h) {
                    return Err(format!("{at}: \"row_height\" is 8 to 360"));
                }
                let row_items = e.row.clone().unwrap_or_default();
                if row_items.is_empty() {
                    return Err(format!("{at}: \"row\" needs at least one element"));
                }
                let plain = self.list(&row_items, &format!("{at}.row"), true)?;
                let focused = match &e.focused {
                    Some(f) if !f.is_empty() => self.list(f, &format!("{at}.focused"), true)?,
                    _ => plain.clone(),
                };
                let scroll = match &e.scrollbar {
                    None => None,
                    Some(s) => Some(Scroll {
                        x: num(Some(s.x), "scrollbar x", None)?,
                        w: num(Some(s.w), "scrollbar w", None)?.clamp(1., 64.),
                        track: s.track.as_deref().map(|t| self.paint(t, &at)).transpose()?,
                        thumb: match &s.thumb {
                            Some(t) => self.paint(t, &at)?,
                            None => Paint::Accent(0xFF),
                        },
                    }),
                };
                El::List {
                    b: Bx {
                        x: num(e.x, "x", Some(0.))?,
                        y: num(e.y, "y", Some(0.))?,
                        w: num(e.w, "w", None)?.clamp(1., 960.),
                        h: rows * row_h,
                    },
                    rows: rows as usize,
                    row_h,
                    source: match e.source.as_deref().unwrap_or("rows") {
                        "rows" => ListSource::Rows,
                        "facts" => ListSource::Facts,
                        "sheet" => ListSource::Sheet,
                        other => {
                            return Err(format!("{at}: source \"{other}\" is rows, facts or sheet"))
                        }
                    },
                    row: plain,
                    focused,
                    scroll,
                }
            }
            "group" => {
                let items = e
                    .elements
                    .clone()
                    .ok_or_else(|| format!("{at}: \"elements\" is missing"))?;
                El::Group {
                    dx: num(e.x, "x", Some(0.))?,
                    dy: num(e.y, "y", Some(0.))?,
                    children: self.list(&items, &format!("{at}.elements"), row)?,
                }
            }
            "part" => {
                let name = required(&e.name, "name")?;
                if self.stack.contains(&name) || self.stack.len() >= 6 {
                    return Err(format!("{at}: part \"{name}\" includes itself"));
                }
                let items = self
                    .parts
                    .get(&name)
                    .ok_or_else(|| format!("{at}: no part named \"{name}\""))?
                    .clone();
                self.used.insert(name.clone());
                self.stack.push(name.clone());
                let children = self.list(&items, &format!("parts.{name}"), row)?;
                self.stack.pop();
                El::Group {
                    dx: num(e.x, "x", Some(0.))?,
                    dy: num(e.y, "y", Some(0.))?,
                    children,
                }
            }
            "native" => {
                if row {
                    return Err(format!("{at}: native parts cannot be inside a list row"));
                }
                El::Native {
                    part: match required(&e.part, "part")?.as_str() {
                        "screen" => NativePart::Screen,
                        "status" => NativePart::Status,
                        other => {
                            return Err(format!(
                                "{at}: native part \"{other}\" is screen or status"
                            ))
                        }
                    },
                    dx: num(e.x, "x", Some(0.))?,
                    dy: num(e.y, "y", Some(0.))?,
                    recolor: e.recolor.unwrap_or(true),
                }
            }
            _ => unreachable!("fields() knows every type"),
        };
        Ok(Node { when, el })
    }
}

fn clip(s: &str, n: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(n).collect()
}

/// Check and build the skin `id` from its manifest text; pictures and fonts
/// come through `files`. Nothing is kept on failure.
pub fn compile(id: &str, text: &str, files: &dyn Files) -> Result<Skin, String> {
    let manifest: Manifest = serde_json::from_str(text).map_err(|e| format!("{MANIFEST}: {e}"))?;
    if manifest.format != FORMAT {
        return Err(format!(
            "made for skin format {}; this Reborn reads {FORMAT}",
            manifest.format
        ));
    }
    if manifest.name.trim().is_empty() {
        return Err("\"name\" is empty".into());
    }
    let mut warnings = vec![];
    let mut palette = Palette::default();
    for (key, value) in &manifest.palette {
        if !TOKENS.contains(&key.as_str()) {
            return Err(format!(
                "palette: unknown token \"{key}\" ({})",
                TOKENS.join(", ")
            ));
        }
        if key == "accent" && value == "cover" {
            palette.accent = None;
            continue;
        }
        let c = hex(value)
            .ok_or_else(|| format!("palette.{key}: \"{value}\" is not #RRGGBB or #RRGGBBAA"))?;
        match key.as_str() {
            "bg" => palette.bg = c | 0xFF,
            "text" => palette.text = c,
            "muted" => palette.muted = c,
            "dim" => palette.dim = c,
            "accent" => palette.accent = Some(c),
            "glass" => palette.glass = c,
            "scrim" => palette.scrim = c,
            "sheet" => palette.sheet = c,
            "track" => palette.track = c,
            "danger" => palette.danger = c,
            "success" => palette.success = c,
            "warning" => palette.warning = c,
            _ => unreachable!("checked against TOKENS"),
        }
    }
    for (name, value) in &manifest.colors {
        if hex(value).is_none() {
            return Err(format!(
                "colors.{name}: \"{value}\" is not #RRGGBB or #RRGGBBAA"
            ));
        }
        if TOKENS.contains(&name.as_str()) {
            return Err(format!("colors.{name}: palette names cannot be reused"));
        }
    }
    let text_contrast = contrast(palette.text, palette.bg);
    if text_contrast < CONTRAST_MIN {
        return Err(format!(
            "palette: text on bg has a contrast of {text_contrast:.1}:1; at least {CONTRAST_MIN}:1 is needed"
        ));
    }
    if text_contrast < CONTRAST_RECOMMENDED {
        warnings.push(format!(
            "text on bg has a contrast of {text_contrast:.1}:1 (4.5:1 recommended)"
        ));
    }
    for (name, c) in [("muted", palette.muted), ("dim", palette.dim)] {
        let k = contrast(c, palette.bg);
        if k < CONTRAST_MIN {
            warnings.push(format!("{name} on bg has a contrast of {k:.1}:1"));
        }
    }
    let mut atlas = atlas::Atlas::default();
    // Pictures, tallest first for a tighter atlas.
    if manifest.images.len() > MAX_PICTURES {
        return Err(format!("more than {MAX_PICTURES} images"));
    }
    let mut decoded = vec![];
    for (name, file) in &manifest.images {
        let at = format!("images.{name}");
        if !relative_name(file) {
            return Err(format!(
                "{at}: \"{file}\" must be a path inside the skin folder"
            ));
        }
        let (w, h, rgba) = files.picture(file, PICTURE_MAX.0, PICTURE_MAX.1).map_err(|e| {
            format!("{at}: \"{file}\" cannot be used ({e}; pictures are PNG, JPEG or WebP up to 480 x 360)")
        })?;
        if w == 0
            || h == 0
            || w > PICTURE_MAX.0
            || h > PICTURE_MAX.1
            || rgba.len() != (w * h * 4) as usize
        {
            return Err(format!("{at}: \"{file}\" is larger than 480 x 360"));
        }
        decoded.push((name.clone(), w as usize, h as usize, rgba));
    }
    decoded.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
    let mut pictures = HashMap::new();
    for (name, w, h, rgba) in decoded {
        let src = atlas
            .picture(w, h, &rgba)
            .map_err(|e| format!("images.{name}: {e}"))?;
        pictures.insert(name, src);
    }
    if manifest.fonts.len() > MAX_FONTS {
        return Err(format!("more than {MAX_FONTS} fonts"));
    }
    let mut fonts = vec![];
    let mut font_index = HashMap::new();
    for (name, spec) in &manifest.fonts {
        let at = format!("fonts.{name}");
        if builtin_face(name).is_some() {
            return Err(format!("{at}: R11…S36 are Reborn's own font names"));
        }
        if !(8. ..=72.).contains(&spec.size) {
            return Err(format!("{at}: size is 8 to 72 pixels"));
        }
        if !relative_name(&spec.file) {
            return Err(format!(
                "{at}: \"{}\" must be a path inside the skin folder",
                spec.file
            ));
        }
        let data = files
            .read(&spec.file, FONT_FILE_MAX)
            .map_err(|e| format!("{at}: \"{}\" {e}", spec.file))?;
        let ascii = match spec.charset.as_deref() {
            None | Some("latin") => false,
            Some("ascii") => true,
            Some(other) => return Err(format!("{at}: charset \"{other}\" is latin or ascii")),
        };
        let font = atlas::rasterise(&mut atlas, &data, spec.size.round(), ascii)
            .map_err(|e| format!("{at}: \"{}\" {e}", spec.file))?;
        font_index.insert(name.clone(), fonts.len());
        fonts.push(font);
    }
    atlas.finish();
    let mut compiler = Compiler {
        palette: palette.clone(),
        colors: &manifest.colors,
        fonts: font_index,
        pictures,
        parts: &manifest.parts,
        elements: 0,
        stack: vec![],
        used: HashSet::new(),
    };
    let mut layouts = HashMap::new();
    for (key, items) in &manifest.screens {
        let name = LAYOUTS
            .iter()
            .find(|n| **n == key.as_str())
            .ok_or_else(|| format!("screens: unknown screen \"{key}\" (see docs/skins.md)"))?;
        let nodes = compiler.list(items, &format!("screens.{key}"), false)?;
        layouts.insert(*name, nodes);
    }
    for name in manifest.parts.keys() {
        if !compiler.used.contains(name) {
            warnings.push(format!("part \"{name}\" is never used"));
        }
    }
    Ok(Skin {
        id: id.to_owned(),
        name: clip(manifest.name.trim(), 40),
        palette,
        layouts,
        fonts,
        atlas: atlas.rgba,
        atlas_width: atlas.width,
        atlas_height: atlas.height,
        warnings,
    })
}
