//! Skins: packages that give Reborn a different look without changing what it
//! does. A skin is a folder with `skin.json`, pictures (PNG, JPEG, WebP) and
//! optional TrueType/OpenType fonts. It describes screens as layers of simple
//! elements (rectangles, gradients, pictures, text, icons, bars, artwork,
//! lists) bound to the player's state; behaviour, navigation and every built-in
//! component (Cover Flow, the equalizer, text entry) stay in Reborn.
//!
//! Loading is strict: an unknown field, a bad colour, a missing file, a
//! picture or font over its limit, or text that cannot be read on its
//! background refuses the whole skin with one message. A screen the skin does
//! not describe is drawn by Reborn itself in the skin's palette.
//! Without a skin nothing here runs and Reborn draws exactly as before.
//! The format is documented for authors in `docs/skins.md`.
pub(crate) mod atlas;
mod compile;
mod draw;

use crate::{canvas::Icon, text::Face, theme};
use serde::Deserialize;
use std::{collections::HashMap, path::PathBuf};

pub use compile::{compile, relative_name};
pub(crate) use draw::{draw_layout, remap, Scene, Sheet};

/// The only format version this Reborn reads.
pub const FORMAT: u32 = 1;
pub const MANIFEST: &str = "skin.json";
/// Largest `skin.json`, in bytes.
pub const MANIFEST_MAX: u64 = 256 * 1024;
/// Largest font file, in bytes.
pub const FONT_FILE_MAX: u64 = 8 * 1024 * 1024;
/// Skins one scan lists.
pub const MAX_SKINS: usize = 64;
const MAX_PICTURES: usize = 64;
const PICTURE_MAX: (u32, u32) = (480, 360);
const MAX_FONTS: usize = 8;
const MAX_ELEMENTS: usize = 4000;

/// How a skin's files reach the compiler. Names are checked relative paths
/// (`relative_name`); the reader keeps them inside the skin's folder.
pub trait Files {
    /// The bytes of `name`, refusing files larger than `limit`.
    fn read(&self, name: &str, limit: u64) -> Result<Vec<u8>, String>;
    /// The picture `name` decoded at its own size as straight-alpha RGBA,
    /// refusing pictures larger than `max_w` x `max_h`.
    fn picture(&self, name: &str, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String>;
}

/// An installed skin as the picker lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkinInfo {
    /// The folder name; stored in the settings.
    pub id: String,
    pub name: String,
    pub author: String,
    pub version: String,
    pub description: String,
    pub dir: PathBuf,
    /// Why the skin cannot be used, when its header already says so.
    pub problem: Option<String>,
    /// The palette's background and accent, shown as the skin's swatch in
    /// the picker (UI v4); `None` when the manifest does not give a background.
    pub swatch: Option<(u32, u32)>,
}

#[derive(Deserialize, Default)]
struct Header {
    #[serde(default)]
    format: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    palette: HeaderPalette,
}

/// The two palette colours the picker's swatch needs; the rest is compiled
/// with the skin.
#[derive(Deserialize, Default)]
struct HeaderPalette {
    #[serde(default)]
    bg: Option<String>,
    #[serde(default)]
    accent: Option<String>,
}

/// A folder name that can identify a skin (it is stored in the settings).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
}

/// What the picker shows for skin folder `id` from its manifest text (or the
/// reason it could not be read).
pub fn describe(id: &str, dir: PathBuf, manifest: Result<&str, String>) -> SkinInfo {
    let mut info = SkinInfo {
        id: id.to_owned(),
        name: id.to_owned(),
        dir,
        ..Default::default()
    };
    match manifest.and_then(|text| {
        serde_json::from_str::<Header>(text).map_err(|e| format!("{MANIFEST}: {e}"))
    }) {
        Ok(h) => {
            if !h.name.trim().is_empty() {
                info.name = clip(h.name.trim(), 40);
            }
            info.author = clip(h.author.trim(), 40);
            info.version = clip(h.version.trim(), 20);
            info.description = clip(h.description.trim(), 200);
            let colour = |c: &Option<String>| c.as_deref().and_then(compile::hex);
            info.swatch = colour(&h.palette.bg).map(|bg| {
                (
                    bg,
                    colour(&h.palette.accent).unwrap_or(theme::color::ACCENT),
                )
            });
            if h.format != FORMAT {
                info.problem = Some(format!(
                    "Made for skin format {}; this Reborn reads {FORMAT}",
                    h.format
                ));
            }
        }
        Err(e) => info.problem = Some(e),
    }
    info
}

fn clip(s: &str, n: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(n).collect()
}

// ------------------------------------------------------------ compiled model

/// A colour: fixed RGBA, or the accent (the cover's colour or the skin's own)
/// at an opacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Paint {
    Fixed(u32),
    Accent(u8),
}

/// Palette tokens; Reborn's own drawing is recoloured through them.
pub(crate) const TOKENS: [&str; 12] = [
    "bg", "text", "muted", "dim", "accent", "glass", "scrim", "sheet", "track", "danger",
    "success", "warning",
];

#[derive(Clone, Debug)]
pub(crate) struct Palette {
    pub bg: u32,
    pub text: u32,
    pub muted: u32,
    pub dim: u32,
    /// `None`: the accent follows the cover that is playing.
    pub accent: Option<u32>,
    pub glass: u32,
    pub scrim: u32,
    pub sheet: u32,
    pub track: u32,
    pub danger: u32,
    pub success: u32,
    pub warning: u32,
}

impl Default for Palette {
    fn default() -> Self {
        use theme::color::*;
        Self {
            bg: BG,
            text: TEXT,
            muted: MUTED,
            dim: DIM,
            accent: None,
            glass: 0xFFFFFFFF,
            scrim: BLACK,
            sheet: SHEET | 0xFF,
            track: RING_TRACK,
            danger: DANGER,
            success: SUCCESS,
            warning: WARNING,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FontRef {
    Builtin(Face),
    Skin(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Case {
    Normal,
    Upper,
    Lower,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Center,
    Right,
}

/// State a text element shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Var {
    Sleep,
    Title,
    Artist,
    Album,
    AlbumArtist,
    Genre,
    Elapsed,
    Remaining,
    Duration,
    TrackNumber,
    QueueIndex,
    QueueLength,
    Quality,
    Output,
    Screen,
    Battery,
    Clock,
    Station,
    Volume,
    Status,
    PageTitle,
    PageBody,
    EmptyTitle,
    EmptyBody,
    EmptyAction,
    Message,
    SheetTitle,
    SheetBody,
    Skin,
    Playback,
    // Row context.
    Label,
    Detail,
    Number,
    Count,
}

const VARS: &[(&str, Var, bool)] = &[
    ("title", Var::Title, false),
    ("artist", Var::Artist, false),
    ("album", Var::Album, false),
    ("album_artist", Var::AlbumArtist, false),
    ("genre", Var::Genre, false),
    ("elapsed", Var::Elapsed, false),
    ("remaining", Var::Remaining, false),
    ("duration", Var::Duration, false),
    ("track_number", Var::TrackNumber, false),
    ("queue_index", Var::QueueIndex, false),
    ("queue_length", Var::QueueLength, false),
    ("quality", Var::Quality, false),
    ("output", Var::Output, false),
    ("screen", Var::Screen, false),
    ("battery", Var::Battery, false),
    ("clock", Var::Clock, false),
    ("station", Var::Station, false),
    ("volume", Var::Volume, false),
    ("status", Var::Status, false),
    ("page_title", Var::PageTitle, false),
    ("page_body", Var::PageBody, false),
    ("empty_title", Var::EmptyTitle, false),
    ("empty_body", Var::EmptyBody, false),
    ("empty_action", Var::EmptyAction, false),
    ("message", Var::Message, false),
    ("sheet_title", Var::SheetTitle, false),
    ("sheet_body", Var::SheetBody, false),
    ("skin", Var::Skin, false),
    ("playback", Var::Playback, false),
    ("sleep", Var::Sleep, false),
    ("label", Var::Label, true),
    ("detail", Var::Detail, true),
    ("number", Var::Number, true),
    ("count", Var::Count, true),
];

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Seg {
    Lit(String),
    Var(Var),
}

/// Yes/no state an element can depend on (`"if"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flag {
    Sleep,
    Playing,
    Paused,
    HasTrack,
    HasArt,
    Shuffle,
    ShuffleAlbums,
    Repeat,
    RepeatOne,
    Charging,
    BatteryLow,
    Bluetooth,
    BluetoothConnected,
    Wifi,
    WifiConnected,
    Back,
    Empty,
    Error,
    Danger,
    Eq,
    Gapless,
    Crossfade,
    Normalise,
    HasStatus,
    HasClock,
    Live,
    Spotify,
    Favorite,
    HasBody,
    HasTitle,
    HasAction,
    HasFacts,
    Interactive,
    // Row context.
    Focused,
    Disabled,
    Toggle,
    On,
    Check,
    Checked,
    Chevron,
    HasDetail,
    RowArt,
    Current,
    First,
    Last,
    Odd,
}

const FLAGS: &[(&str, Flag, bool)] = &[
    ("playing", Flag::Playing, false),
    ("paused", Flag::Paused, false),
    ("has_track", Flag::HasTrack, false),
    ("has_art", Flag::HasArt, false),
    ("shuffle", Flag::Shuffle, false),
    ("shuffle_albums", Flag::ShuffleAlbums, false),
    ("repeat", Flag::Repeat, false),
    ("repeat_one", Flag::RepeatOne, false),
    ("charging", Flag::Charging, false),
    ("battery_low", Flag::BatteryLow, false),
    ("bluetooth", Flag::Bluetooth, false),
    ("bluetooth_connected", Flag::BluetoothConnected, false),
    ("wifi", Flag::Wifi, false),
    ("wifi_connected", Flag::WifiConnected, false),
    ("back", Flag::Back, false),
    ("empty", Flag::Empty, false),
    ("error", Flag::Error, false),
    ("danger", Flag::Danger, false),
    ("eq", Flag::Eq, false),
    ("gapless", Flag::Gapless, false),
    ("crossfade", Flag::Crossfade, false),
    ("normalise", Flag::Normalise, false),
    ("sleep", Flag::Sleep, false),
    ("has_status", Flag::HasStatus, false),
    ("has_clock", Flag::HasClock, false),
    ("live", Flag::Live, false),
    ("spotify", Flag::Spotify, false),
    ("favorite", Flag::Favorite, false),
    ("has_body", Flag::HasBody, false),
    ("has_title", Flag::HasTitle, false),
    ("has_action", Flag::HasAction, false),
    ("has_facts", Flag::HasFacts, false),
    ("interactive", Flag::Interactive, false),
    ("focused", Flag::Focused, true),
    ("disabled", Flag::Disabled, true),
    ("toggle", Flag::Toggle, true),
    ("on", Flag::On, true),
    ("check", Flag::Check, true),
    ("checked", Flag::Checked, true),
    ("chevron", Flag::Chevron, true),
    ("has_detail", Flag::HasDetail, true),
    ("row_art", Flag::RowArt, true),
    ("current", Flag::Current, true),
    ("first", Flag::First, true),
    ("last", Flag::Last, true),
    ("odd", Flag::Odd, true),
];

/// Alternatives (`|`) of conjunctions (`&`) of possibly negated flags.
pub(crate) type Cond = Vec<Vec<(bool, Flag)>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Progress,
    Volume,
    Battery,
    Brightness,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IconRef {
    Fixed(Icon),
    /// Pause while playing, Play otherwise.
    PlayPause {
        large: bool,
    },
    /// Repeat or Repeat One, as set.
    RepeatMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtSource {
    Current,
    Row,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListSource {
    Rows,
    Facts,
    Sheet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativePart {
    Screen,
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bx {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A picture's atlas rectangle and its optional nine-slice insets.
pub(crate) type Sliced = ([u16; 4], Option<[u16; 4]>);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Scroll {
    pub x: f32,
    pub w: f32,
    pub track: Option<Paint>,
    pub thumb: Paint,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum El {
    Rect {
        b: Bx,
        paint: Paint,
        radius: f32,
        stroke: f32,
    },
    Gradient {
        b: Bx,
        top: Paint,
        bottom: Paint,
    },
    Picture {
        b: Bx,
        src: [u16; 4],
        slice: Option<[u16; 4]>,
        tint: Paint,
    },
    Text {
        b: Bx,
        text: Vec<Seg>,
        font: FontRef,
        paint: Paint,
        align: Align,
        lines: usize,
        line: f32,
        tracking: f32,
        case: Case,
    },
    Icon {
        x: f32,
        y: f32,
        size: f32,
        icon: IconRef,
        paint: Paint,
    },
    Bar {
        b: Bx,
        level: Level,
        track: Option<Paint>,
        fill: Paint,
        radius: f32,
        vertical: bool,
        knob: Option<(Paint, f32)>,
        track_picture: Option<Sliced>,
        fill_picture: Option<Sliced>,
    },
    Art {
        b: Bx,
        source: ArtSource,
        radius: f32,
    },
    List {
        b: Bx,
        rows: usize,
        row_h: f32,
        source: ListSource,
        row: Vec<Node>,
        focused: Vec<Node>,
        scroll: Option<Scroll>,
    },
    Group {
        dx: f32,
        dy: f32,
        children: Vec<Node>,
    },
    Native {
        part: NativePart,
        dx: f32,
        dy: f32,
        /// Draw in the skin's palette (default) or exactly as Reborn does.
        recolor: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Node {
    pub when: Option<Cond>,
    pub el: El,
}

/// The screens and overlays a skin may describe.
pub const LAYOUTS: &[&str] = &[
    // Categories.
    "home",
    "now_playing",
    "list",
    "page",
    // One screen; takes precedence over its category.
    "music",
    "artists",
    "albums",
    "cover_flow",
    "genres",
    "songs",
    "folders",
    "album",
    "artist",
    "queue",
    "song_info",
    "settings",
    "wifi",
    "bluetooth",
    "computer",
    "sound",
    "equalizer",
    "playback",
    "crossfade",
    "balance",
    "screen_off",
    "output",
    "headphone_level",
    "dac_filter",
    "error_reports",
    "theme",
    "time_zone",
    "date_time",
    "home_menu",
    "library",
    "display",
    "customize",
    "audiobooks",
    "book",
    "service",
    "battery",
    "sleep",
    "storage",
    "updates",
    "about",
    "maintenance",
    "diagnostics",
    "diagnostic",
    "value",
    "search",
    "playlists",
    "playlist",
    "radio",
    "radio_stations",
    "radio_categories",
    "radio_info",
    // Overlays.
    "sheet",
    "dialog",
    "toast",
    "volume",
    "keyboard",
];

/// A loaded, validated skin with its atlas ready to upload.
pub struct Skin {
    pub id: String,
    pub name: String,
    pub(crate) palette: Palette,
    pub(crate) layouts: HashMap<&'static str, Vec<Node>>,
    pub(crate) fonts: Vec<atlas::Font>,
    pub atlas: Vec<u8>,
    pub atlas_width: usize,
    pub atlas_height: usize,
    /// Advice that did not stop the skin (low contrast, unused files).
    pub warnings: Vec<String>,
}

impl std::fmt::Debug for Skin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Skin")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("layouts", &self.layouts.len())
            .field("fonts", &self.fonts.len())
            .finish()
    }
}

impl Skin {
    pub(crate) fn layout(&self, key: &str) -> Option<&[Node]> {
        self.layouts.get(key).map(Vec::as_slice)
    }
    pub(crate) fn accent(&self) -> Option<u32> {
        self.palette.accent
    }
    pub(crate) fn bg(&self) -> u32 {
        self.palette.bg
    }
    /// Visible rows of the first list in `key`'s layout.
    pub(crate) fn list_rows(&self, key: &str) -> Option<usize> {
        fn find(nodes: &[Node]) -> Option<usize> {
            nodes.iter().find_map(|n| match &n.el {
                El::List {
                    rows,
                    source: ListSource::Rows,
                    ..
                } => Some(*rows),
                El::Group { children, .. } => find(children),
                _ => None,
            })
        }
        self.layouts.get(key).and_then(|l| find(l))
    }
    pub fn layout_names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.layouts.keys().copied().collect();
        names.sort_unstable();
        names
    }
}

#[cfg(test)]
mod tests;
