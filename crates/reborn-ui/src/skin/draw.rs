//! The skin interpreter: draws a compiled layout from the player's state, and
//! recolours Reborn's own drawing for the screens a skin leaves to Reborn.
use super::{
    atlas::Font, Align, ArtSource, Bx, Case, El, Flag, FontRef, IconRef, Level, ListSource,
    NativePart, Node, Paint, Palette, Seg, Skin, Var,
};
use crate::{
    canvas::{Canvas, Icon},
    components, pages,
    screens::{count_label, is_current, row_track, screen_title},
    text::{self, Face},
    theme::color,
    Item, RowKind, Ui,
};
use reborn_core::{
    platform::{ChargingState, LowBattery},
    AppModel, PlaybackState, RepeatMode, Screen, Track,
};
use reborn_graphics::{Quad, Tex};

/// What a layout is drawn from, besides the model.
pub(crate) struct Scene<'a> {
    pub ui: &'a Ui,
    pub m: &'a AppModel,
    pub tracks: &'a [Track],
    pub skin: &'a Skin,
    pub has_art: bool,
    /// The screen takes input (no sheet or dialog above it).
    pub interactive: bool,
    pub page: Option<&'a pages::Page>,
    pub sheet: Option<Sheet<'a>>,
    /// Toast text and whether it reports a problem.
    pub message: Option<(&'a str, bool)>,
    /// Multiplies every colour (the volume line fades).
    pub opacity: f32,
    /// Draws Reborn's own version of the current screen (`native` parts).
    pub native: Option<&'a dyn Fn(&mut Canvas)>,
    /// Where the row's `{label}` text ends: a right-aligned `{detail}` of the
    /// same row may use the room up to it (a long value, such as the song Up
    /// Next names, is not cut to its box when the label leaves room; #60).
    pub label_end: std::cell::Cell<Option<f32>>,
}

/// A sheet or dialog: its copy, choices and focus.
pub(crate) struct Sheet<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub rows: &'a [Item],
    pub focus: usize,
    pub danger: bool,
}

struct Row<'a> {
    item: &'a Item,
    index: usize,
    count: usize,
    focused: bool,
    current: bool,
}

impl Scene<'_> {
    fn paint(&self, p: Paint) -> u32 {
        let c = match p {
            Paint::Fixed(c) => c,
            Paint::Accent(a) => (self.ui.cover_accent() & 0xFFFF_FF00) | u32::from(a),
        };
        if self.opacity >= 1. {
            c
        } else {
            crate::theme::fade(c, self.opacity)
        }
    }

    fn flag(&self, f: Flag, row: Option<&Row>) -> bool {
        let m = self.m;
        let s = &m.settings;
        let playing = matches!(
            m.playback,
            PlaybackState::Playing | PlaybackState::Buffering
        );
        let b = m.platform.battery;
        match f {
            Flag::Playing => playing && m.current().is_some(),
            Flag::Paused => !playing && m.current().is_some(),
            Flag::HasTrack => m.current().is_some(),
            Flag::HasArt => self.has_art && m.current().is_some(),
            Flag::Shuffle => s.shuffle,
            Flag::ShuffleAlbums => s.shuffle_mode() == reborn_core::ShuffleMode::Albums,
            Flag::Repeat => s.repeat != RepeatMode::Off,
            Flag::RepeatOne => s.repeat == RepeatMode::Track,
            Flag::Charging => matches!(b.charging, ChargingState::Charging),
            Flag::BatteryLow => {
                matches!(
                    b.level,
                    LowBattery::Low | LowBattery::Critical | LowBattery::ShuttingDown
                ) || b.percent.is_some_and(|p| p <= 15)
            }
            Flag::Bluetooth => self.ui.bluetooth.powered,
            Flag::BluetoothConnected => {
                self.ui.bluetooth.powered && self.ui.bluetooth.devices.iter().any(|d| d.connected)
            }
            Flag::Wifi => self.ui.wifi.powered,
            Flag::WifiConnected => matches!(self.ui.wifi.status, crate::WifiStatus::Connected(_)),
            Flag::Back => m.screen != Screen::Home,
            Flag::Empty => self.row_count() == 0,
            Flag::Error => self.message.is_some_and(|(_, e)| e),
            Flag::Danger => self.sheet.as_ref().is_some_and(|s| s.danger),
            Flag::Eq => s.eq_enabled,
            Flag::Gapless => s.gapless_enabled,
            Flag::Crossfade => s.crossfade_ms > 0,
            Flag::Normalise => s.replay_gain != reborn_core::ReplayGainMode::Off,
            Flag::Sleep => m.sleep_timer.is_some(),
            Flag::HasStatus => self.page.and_then(|p| p.status.as_ref()).is_some(),
            Flag::HasClock => self.ui.clock(m).is_some(),
            Flag::Live => m.current().is_some_and(Track::is_radio),
            Flag::Spotify => m.current().is_some_and(Track::is_spotify),
            // The song playing is in Favorites (Reborn's own Now Playing heart).
            Flag::Favorite => m
                .current()
                .filter(|t| !t.is_radio())
                .and_then(|t| {
                    self.tracks
                        .iter()
                        .position(|x| x.id == t.id && x.source_id == t.source_id)
                })
                .is_some_and(|i| self.ui.is_favorite_track(m, self.tracks, i)),
            Flag::HasBody => !self.text(Var::PageBody, None).is_empty(),
            Flag::HasTitle => !screen_title(m).is_empty(),
            Flag::HasAction => self.empty_copy().2.is_some(),
            Flag::HasFacts => self.page.is_some_and(|p| !p.facts.is_empty()),
            Flag::Interactive => self.interactive,
            Flag::Focused => row.is_some_and(|r| r.focused),
            Flag::Disabled => row.is_some_and(|r| !r.item.enabled),
            Flag::Toggle => row.is_some_and(|r| matches!(r.item.kind, RowKind::Toggle(_))),
            Flag::On => row.is_some_and(|r| r.item.kind == RowKind::Toggle(true)),
            Flag::Check => row.is_some_and(|r| matches!(r.item.kind, RowKind::Check(_))),
            Flag::Checked => row.is_some_and(|r| r.item.kind == RowKind::Check(true)),
            Flag::Chevron => row.is_some_and(|r| r.item.kind == RowKind::Chevron),
            Flag::HasDetail => row.is_some_and(|r| !r.item.secondary.is_empty()),
            Flag::RowArt => {
                row.is_some_and(|r| r.item.art.is_some() || self.row_station(r).is_some())
            }
            Flag::Current => row.is_some_and(|r| r.current),
            Flag::First => row.is_some_and(|r| r.index == 0),
            Flag::Last => row.is_some_and(|r| r.index + 1 == r.count),
            Flag::Odd => row.is_some_and(|r| r.index % 2 == 1),
        }
    }

    /// The station a row of a station list stands for.
    fn row_station(&self, r: &Row) -> Option<&reborn_core::RadioStation> {
        r.item
            .key
            .strip_prefix("station:")
            .and_then(|i| i.parse::<usize>().ok())
            .and_then(|i| self.ui.radio.stations.get(i))
    }

    fn when(&self, node: &Node, row: Option<&Row>) -> bool {
        node.when.as_ref().is_none_or(|alts| {
            alts.iter()
                .any(|all| all.iter().all(|(neg, f)| self.flag(*f, row) != *neg))
        })
    }

    fn empty_copy(&self) -> (&'static str, &'static str, Option<&'static str>) {
        crate::views_library::empty_copy(self.m)
    }

    fn text(&self, v: Var, row: Option<&Row>) -> String {
        let m = self.m;
        let t = m.current();
        let track = |f: fn(&Track) -> String| t.map(f).unwrap_or_default();
        // A book: chapter, author and book, times within the chapter, the
        // book's place for the quality line, chapter n of m for the queue.
        if let Some(b) = crate::views_books::now(m) {
            let shown = match v {
                Var::Title => Some(b.chapter.clone()),
                Var::Artist | Var::AlbumArtist => Some(b.author.clone()),
                Var::Album => Some(b.book.clone()),
                Var::Elapsed => Some(components::time(b.elapsed(m.position_ms))),
                Var::Remaining => Some(format!(
                    "-{}",
                    components::time(b.chapter_ms.saturating_sub(b.elapsed(m.position_ms)))
                )),
                Var::Duration => Some(components::time(b.chapter_ms)),
                Var::Quality => Some(b.left()),
                Var::QueueIndex => Some((b.index + 1).to_string()),
                Var::QueueLength => Some(b.count.to_string()),
                _ => None,
            };
            if let Some(text) = shown {
                return text;
            }
        }
        match v {
            // A station: the song it announces, as Reborn's own Now Playing.
            Var::Title => t
                .map(|t| crate::views_playing::lines(self.ui, t).0)
                .unwrap_or_default(),
            Var::Artist => t
                .map(|t| crate::views_playing::lines(self.ui, t).1)
                .unwrap_or_default(),
            Var::Station => t
                .filter(|t| t.is_radio())
                .map(|t| {
                    self.ui
                        .radio
                        .now
                        .as_ref()
                        .map_or_else(|| crate::track_title(t).to_owned(), |s| s.name.clone())
                })
                .unwrap_or_default(),
            Var::Album => t
                .map(|t| crate::views_playing::lines(self.ui, t).2)
                .unwrap_or_default(),
            Var::AlbumArtist => track(|t| {
                crate::names_or_unknown(if t.album_artist.is_empty() {
                    &t.artist
                } else {
                    &t.album_artist
                })
                .into_owned()
            }),
            Var::Genre => track(|t| reborn_core::joined_names(&t.genre).into_owned()),
            Var::Elapsed => t
                .map(|_| components::time(m.position_ms))
                .unwrap_or_default(),
            // A station has no length: what is left reads LIVE.
            Var::Remaining | Var::Duration if t.is_some_and(Track::is_radio) => "LIVE".into(),
            Var::Remaining => t
                .map(|t| {
                    format!(
                        "-{}",
                        components::time(t.duration_ms.saturating_sub(m.position_ms))
                    )
                })
                .unwrap_or_default(),
            Var::Duration => track(|t| components::time(t.duration_ms)),
            Var::TrackNumber => t
                .filter(|t| t.track > 0)
                .map(|t| t.track.to_string())
                .unwrap_or_default(),
            Var::QueueIndex => t
                .map(|_| (m.queue_position + 1).to_string())
                .unwrap_or_default(),
            Var::QueueLength => m.queue.len().to_string(),
            Var::Quality => t
                .and_then(crate::views_playing::quality_label)
                .unwrap_or_default(),
            Var::Output => pages::output_name(self.ui, m),
            Var::Screen => screen_title(m),
            Var::Battery => m
                .platform
                .battery
                .percent
                .filter(|p| *p <= 100)
                .map(|p| p.to_string())
                .unwrap_or_default(),
            Var::Clock => self.ui.clock(m).unwrap_or_default(),
            Var::Volume => m.settings.volume.min(100).to_string(),
            Var::Status => self.page.and_then(|p| p.status.clone()).unwrap_or_default(),
            Var::PageTitle => self
                .page
                .and_then(|p| p.hero.as_ref().map(|h| h.title.clone()))
                .unwrap_or_else(|| screen_title(m)),
            Var::PageBody => self
                .page
                .and_then(|p| p.hero.as_ref().map(|h| h.body.clone()))
                .unwrap_or_default(),
            Var::EmptyTitle => self.empty_copy().0.to_owned(),
            Var::EmptyBody => self.empty_copy().1.to_owned(),
            Var::EmptyAction => self.empty_copy().2.unwrap_or_default().to_owned(),
            Var::Message => self.message.map(|(s, _)| s.to_owned()).unwrap_or_default(),
            Var::SheetTitle => self
                .sheet
                .as_ref()
                .map(|s| s.title.to_owned())
                .unwrap_or_default(),
            Var::SheetBody => self
                .sheet
                .as_ref()
                .map(|s| s.body.to_owned())
                .unwrap_or_default(),
            Var::Skin => self.skin.name.clone(),
            // "Sleeps in 23 min", "Sleeps after song", or nothing (#29).
            Var::Sleep => crate::views_playing::sleep_note(m).unwrap_or_default(),
            Var::Playback => match (m.current(), m.playback) {
                (None, _) => "Stopped",
                (_, PlaybackState::Playing | PlaybackState::Buffering) => "Playing",
                _ => "Paused",
            }
            .to_owned(),
            Var::Label => row.map(|r| r.item.label.clone()).unwrap_or_default(),
            Var::Detail => row.map(|r| r.item.secondary.clone()).unwrap_or_default(),
            Var::Number => row.map(|r| (r.index + 1).to_string()).unwrap_or_default(),
            Var::Count => row.map(|r| r.count.to_string()).unwrap_or_default(),
        }
    }

    fn level(&self, l: Level) -> f32 {
        let m = self.m;
        match l {
            // A station: a full bar, as in Reborn's own Now Playing.
            Level::Progress if crate::views_books::now(m).is_some() => crate::views_books::now(m)
                .map_or(0., |b| {
                    components::progress(b.elapsed(m.position_ms), b.chapter_ms)
                }),
            Level::Progress => m
                .current()
                .map(|t| {
                    if t.is_radio() {
                        1.
                    } else {
                        components::progress(m.position_ms, t.duration_ms)
                    }
                })
                .unwrap_or(0.),
            Level::Volume => f32::from(m.settings.volume.min(100)) / 100.,
            Level::Battery => m
                .platform
                .battery
                .percent
                .map(|p| f32::from(p.min(100)) / 100.)
                .unwrap_or(0.),
            Level::Brightness => f32::from(m.settings.brightness.min(100)) / 100.,
        }
    }

    fn row_count(&self) -> usize {
        self.ui.row_count(self.m, self.tracks)
    }

    /// Items of a list window: `(items, total, focus, first index)`.
    fn list(&self, source: ListSource, rows: usize) -> (Vec<Item>, usize, Option<usize>, usize) {
        let m = self.m;
        match source {
            ListSource::Rows => {
                let count = self.row_count();
                let focus = m.navigation.focus.min(count.saturating_sub(1));
                let start = m
                    .navigation
                    .scroll
                    .min(focus)
                    .max(focus.saturating_add(1).saturating_sub(rows))
                    .min(count.saturating_sub(rows));
                let mut items = self.ui.visible_rows(m, self.tracks, start, rows);
                if m.screen == Screen::Home {
                    let counts = self.ui.library_counts(m, self.tracks);
                    for item in &mut items {
                        let n = match item.key.as_str() {
                            "artists" => counts[0],
                            "albums" => counts[1],
                            "tracks" => counts[2],
                            "genres" => counts[3],
                            _ => 0,
                        };
                        if n > 0 && item.secondary.is_empty() {
                            item.secondary = count_label(n);
                        }
                    }
                }
                let focus = (self.interactive && count > 0).then_some(focus);
                (items, count, focus, start)
            }
            ListSource::Facts => {
                let facts: Vec<Item> = self
                    .page
                    .map(|p| {
                        p.facts
                            .iter()
                            .map(|(k, v)| Item::new(k.clone(), "").with_secondary(v.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let count = facts.len();
                (facts.into_iter().take(rows).collect(), count, None, 0)
            }
            ListSource::Sheet => match &self.sheet {
                Some(s) => {
                    let count = s.rows.len();
                    let focus = s.focus.min(count.saturating_sub(1));
                    let start = focus.saturating_add(1).saturating_sub(rows);
                    (
                        s.rows.iter().skip(start).take(rows).cloned().collect(),
                        count,
                        (count > 0).then_some(focus),
                        start,
                    )
                }
                None => (vec![], 0, None, 0),
            },
        }
    }

    fn row_current(&self, item: &Item) -> bool {
        // Up Next's rows include headings and the history (8.33), so its row
        // index is not the queue position (#60).
        if self.m.screen == Screen::Queue {
            return item.key == Ui::entry_key(self.m, self.m.queue_position);
        }
        row_track(item, self.tracks).is_some_and(|(_, t)| is_current(self.m, t))
    }
}

// --------------------------------------------------------------------- text

fn builtin_for(px: f32) -> Face {
    [
        Face::R11,
        Face::R13,
        Face::R15,
        Face::R17,
        Face::S20,
        Face::S25,
        Face::S28,
        Face::S36,
    ]
    .into_iter()
    .min_by(|a, b| (a.px() - px).abs().total_cmp(&(b.px() - px).abs()))
    .unwrap_or(Face::R15)
}

struct Writer<'a> {
    font: FontRef,
    fonts: &'a [Font],
    tracking: f32,
}

impl Writer<'_> {
    fn skin(&self) -> Option<&Font> {
        match self.font {
            FontRef::Skin(i) => self.fonts.get(i),
            FontRef::Builtin(_) => None,
        }
    }
    fn fallback(&self) -> Face {
        match self.font {
            FontRef::Builtin(f) => f,
            FontRef::Skin(_) => builtin_for(self.skin().map_or(15., |f| f.px)),
        }
    }
    fn advance(&self, ch: char) -> f32 {
        if let Some(g) = self.skin().and_then(|f| f.glyphs.get(&ch)) {
            return g.advance;
        }
        text::advance(self.fallback(), ch)
    }
    fn width(&self, s: &str) -> f32 {
        let mut total = 0.;
        let mut n = 0.;
        for ch in s.chars().filter(|c| !c.is_control()) {
            total += self.advance(ch);
            n += 1.;
        }
        total + self.tracking * (n - 1f32).max(0.)
    }
    fn mid(&self) -> f32 {
        match self.font {
            FontRef::Builtin(f) => f.mid_offset(),
            FontRef::Skin(_) => self.skin().map_or(6., |f| f.mid),
        }
    }
    fn line(&self) -> f32 {
        match self.font {
            FontRef::Builtin(f) => (f.px() * 1.4).round(),
            FontRef::Skin(_) => self.skin().map_or(20., |f| f.line),
        }
    }
    fn fit(&self, s: &str, max: f32) -> String {
        let clean: String = s.chars().filter(|c| !c.is_control()).take(1024).collect();
        if max <= 0. || self.width(&clean) <= max {
            return clean;
        }
        let ellipsis = self.advance('…');
        let mut out = String::new();
        let mut used = ellipsis;
        for ch in clean.chars() {
            used += self.advance(ch) + self.tracking;
            if used > max {
                break;
            }
            out.push(ch);
        }
        let out = out.trim_end().to_owned();
        if max >= ellipsis {
            out + "…"
        } else {
            out
        }
    }
    fn wrap(&self, s: &str, max: f32, lines: usize) -> Vec<String> {
        let mut out = vec![];
        let mut rest = s.trim();
        while !rest.is_empty() && out.len() < lines {
            if out.len() + 1 == lines || max <= 0. || self.width(rest) <= max {
                out.push(self.fit(rest, max));
                break;
            }
            let mut split = rest.len();
            let mut space = 0;
            let mut used = 0.;
            for (i, ch) in rest.char_indices() {
                used += self.advance(ch) + self.tracking;
                if ch == ' ' {
                    space = i;
                }
                if used > max {
                    split = if space > 0 { space } else { i };
                    break;
                }
            }
            if split == 0 {
                break;
            }
            out.push(rest[..split].trim_end().to_owned());
            rest = rest[split..].trim_start();
        }
        out
    }
    fn draw(&self, c: &mut Canvas, x: f32, baseline: f32, s: &str, color: u32) {
        if color & 0xFF == 0 {
            return;
        }
        let Some(font) = self.skin() else {
            c.text_tracked(x, baseline, s, self.fallback(), color, self.tracking);
            return;
        };
        let baseline = baseline.round();
        let mut pen = x;
        for ch in s.chars().filter(|c| !c.is_control()) {
            match font.glyphs.get(&ch) {
                Some(g) => {
                    if g.src[2] > 0 {
                        c.draw.push(Quad::masked(
                            Tex::Skin,
                            g.src,
                            pen.round() + f32::from(g.left),
                            baseline - f32::from(g.top),
                            f32::from(g.src[2]),
                            f32::from(g.src[3]),
                            color,
                        ));
                    }
                    pen += g.advance + self.tracking;
                }
                None => {
                    let mut buf = [0u8; 4];
                    pen += c.text(
                        pen,
                        baseline,
                        ch.encode_utf8(&mut buf),
                        self.fallback(),
                        color,
                    ) + self.tracking;
                }
            }
        }
    }
}

// ------------------------------------------------------------------ drawing

/// A picture region stretched into `b`, optionally nine-sliced.
fn picture(c: &mut Canvas, src: [u16; 4], slice: Option<[u16; 4]>, b: Bx, color: u32) {
    if b.w <= 0. || b.h <= 0. || color & 0xFF == 0 {
        return;
    }
    let Some([l, t, r, bo]) = slice else {
        c.draw
            .push(Quad::masked(Tex::Skin, src, b.x, b.y, b.w, b.h, color));
        return;
    };
    let [sx, sy, sw, sh] = src;
    // Source columns/rows and destination columns/rows.
    let scol = [(sx, l), (sx + l, sw - l - r), (sx + sw - r, r)];
    let srow = [(sy, t), (sy + t, sh - t - bo), (sy + sh - bo, bo)];
    let (fl, fr) = (f32::from(l).min(b.w / 2.), f32::from(r).min(b.w / 2.));
    let (ft, fb) = (f32::from(t).min(b.h / 2.), f32::from(bo).min(b.h / 2.));
    let dcol = [(b.x, fl), (b.x + fl, b.w - fl - fr), (b.x + b.w - fr, fr)];
    let drow = [(b.y, ft), (b.y + ft, b.h - ft - fb), (b.y + b.h - fb, fb)];
    for (j, (srcy, srch)) in srow.iter().enumerate() {
        for (i, (srcx, srcw)) in scol.iter().enumerate() {
            let (dx, dw) = dcol[i];
            let (dy, dh) = drow[j];
            if *srcw == 0 || *srch == 0 || dw <= 0. || dh <= 0. {
                continue;
            }
            c.draw.push(Quad::masked(
                Tex::Skin,
                [*srcx, *srcy, *srcw, *srch],
                dx,
                dy,
                dw,
                dh,
                color,
            ));
        }
    }
}

fn shape(c: &mut Canvas, b: Bx, radius: f32, stroke: f32, color: u32) {
    if stroke > 0. {
        c.rrect_outline(b.x, b.y, b.w, b.h, radius, stroke, color);
    } else if radius >= 2. {
        c.rrect(b.x, b.y, b.w, b.h, radius, color);
    } else {
        c.rect(b.x, b.y, b.w, b.h, color);
    }
}

fn at(b: Bx, ox: f32, oy: f32) -> Bx {
    Bx {
        x: b.x + ox,
        y: b.y + oy,
        ..b
    }
}

/// Draw `nodes` at offset (`ox`, `oy`).
fn nodes(c: &mut Canvas, s: &Scene, list: &[Node], ox: f32, oy: f32, row: Option<&Row>) {
    for node in list {
        if s.when(node, row) {
            element(c, s, &node.el, ox, oy, row);
        }
    }
}

fn element(c: &mut Canvas, s: &Scene, el: &El, ox: f32, oy: f32, row: Option<&Row>) {
    match el {
        El::Rect {
            b,
            paint,
            radius,
            stroke,
        } => shape(c, at(*b, ox, oy), *radius, *stroke, s.paint(*paint)),
        El::Gradient { b, top, bottom } => {
            let b = at(*b, ox, oy);
            c.rect(b.x, b.y, b.w, b.h, s.paint(*top));
            c.fade_from_bottom(b.x, b.y, b.w, b.h, s.paint(*bottom));
        }
        El::Picture {
            b,
            src,
            slice,
            tint,
        } => picture(c, *src, *slice, at(*b, ox, oy), s.paint(*tint)),
        El::Text {
            b,
            text,
            font,
            paint,
            align,
            lines,
            line,
            tracking,
            case,
        } => {
            let mut value = String::new();
            for seg in text {
                match seg {
                    Seg::Lit(l) => value.push_str(l),
                    Seg::Var(v) => value.push_str(&s.text(*v, row)),
                }
            }
            match case {
                Case::Normal => {}
                Case::Upper => value = value.to_uppercase(),
                Case::Lower => value = value.to_lowercase(),
            }
            if value.trim().is_empty() {
                return;
            }
            let w = Writer {
                font: *font,
                fonts: &s.skin.fonts,
                tracking: *tracking,
            };
            let mut b = at(*b, ox, oy);
            let only = |var: Var| matches!(text.as_slice(), [Seg::Var(v)] if *v == var);
            if row.is_some() && *lines <= 1 && only(Var::Detail) && matches!(align, Align::Right) {
                if let Some(end) = s.label_end.get().filter(|end| *end + 12. < b.x) {
                    b.w += b.x - (end + 12.);
                    b.x = end + 12.;
                }
            }
            let color = s.paint(*paint);
            let pitch = if *line > 0. { *line } else { w.line() };
            let shown = if *lines > 1 {
                w.wrap(&value, b.w, *lines)
            } else {
                vec![w.fit(&value, b.w)]
            };
            // Several lines are centred as a block in `h`, or start at `y`.
            let top = b.y + ((b.h - pitch * shown.len() as f32) / 2.).max(0.);
            for (i, l) in shown.iter().enumerate() {
                let cy = if *lines > 1 {
                    top + pitch * (i as f32 + 0.5)
                } else {
                    b.y + b.h / 2.
                };
                let width = w.width(l);
                let x = match align {
                    Align::Left => b.x,
                    Align::Center => b.x + (b.w - width) / 2.,
                    Align::Right => b.x + b.w - width,
                };
                if row.is_some() && only(Var::Label) {
                    s.label_end.set(Some(x + width));
                }
                w.draw(c, x.round(), cy + w.mid(), l, color);
            }
        }
        El::Icon {
            x,
            y,
            size,
            icon,
            paint,
        } => {
            let playing = matches!(
                s.m.playback,
                PlaybackState::Playing | PlaybackState::Buffering
            );
            let icon = match icon {
                IconRef::Fixed(i) => *i,
                IconRef::PlayPause { large } => match (playing, large) {
                    (true, true) => Icon::PauseLarge,
                    (true, false) => Icon::Pause,
                    (false, true) => Icon::PlayLarge,
                    (false, false) => Icon::Play,
                },
                IconRef::RepeatMode => {
                    if s.m.settings.repeat == RepeatMode::Track {
                        Icon::RepeatOne
                    } else {
                        Icon::Repeat
                    }
                }
            };
            c.icon(
                icon,
                x + ox + size / 2.,
                y + oy + size / 2.,
                *size,
                s.paint(*paint),
            );
        }
        El::Bar {
            b,
            level,
            track,
            fill,
            radius,
            vertical,
            knob,
            track_picture,
            fill_picture,
        } => {
            let b = at(*b, ox, oy);
            let v = s.level(*level).clamp(0., 1.);
            if let Some((src, slice)) = track_picture {
                picture(c, *src, *slice, b, s.paint(Paint::Fixed(0xFFFF_FFFF)));
            } else if let Some(t) = track {
                shape(c, b, *radius, 0., s.paint(*t));
            }
            let f = if *vertical {
                let h = (b.h * v).round();
                Bx {
                    y: b.y + b.h - h,
                    h,
                    ..b
                }
            } else {
                Bx {
                    w: (b.w * v).round(),
                    ..b
                }
            };
            if f.w >= 1. && f.h >= 1. {
                if let Some((src, slice)) = fill_picture {
                    picture(c, *src, *slice, f, s.paint(Paint::Fixed(0xFFFF_FFFF)));
                } else {
                    shape(c, f, radius.min(f.w / 2.).min(f.h / 2.), 0., s.paint(*fill));
                }
            }
            if let Some((paint, r)) = knob {
                let (cx, cy) = if *vertical {
                    (b.x + b.w / 2., f.y)
                } else {
                    (f.x + f.w, b.y + b.h / 2.)
                };
                c.disc(cx, cy, *r, s.paint(*paint));
            }
        }
        El::Art { b, source, radius } => {
            let b = at(*b, ox, oy);
            let tint = s.paint(Paint::Fixed(0xFFFF_FFFF));
            match source {
                ArtSource::Current => {
                    if s.has_art && s.m.current().is_some() {
                        c.draw.push(Quad {
                            tex: Tex::Art,
                            ..Quad::rect(b.x, b.y, b.w, b.w, tint)
                        });
                        if *radius >= 2. {
                            c.round_off(b.x, b.y, b.w, *radius, s.skin.bg());
                        }
                    } else if let Some(t) = s.m.current().filter(|t| t.is_radio()) {
                        // A station without its logo (yet): its initials.
                        components::initials_tile(c, crate::track_title(t), b.x, b.y, b.w);
                    } else {
                        components::placeholder(c, b.x, b.y, b.w, *radius, 0);
                    }
                }
                ArtSource::Row if row.and_then(|r| s.row_station(r)).is_some() => {
                    let station = row.and_then(|r| s.row_station(r)).unwrap();
                    components::station_logo(c, s.ui, station, b.x, b.y, b.w);
                    if *radius >= 2. {
                        c.round_off(b.x, b.y, b.w, *radius, s.skin.bg());
                    }
                }
                ArtSource::Row => {
                    let track = row
                        .and_then(|r| r.item.art)
                        .and_then(|i| s.tracks.get(i).map(|t| (i, t)));
                    components::cover_rounded(c, s.ui, track, b.x, b.y, b.w, *radius);
                }
            }
        }
        El::List {
            b,
            rows,
            row_h,
            source,
            row: plain,
            focused,
            scroll,
        } => {
            let (items, count, focus, start) = s.list(*source, *rows);
            let b = at(*b, ox, oy);
            for (i, item) in items.iter().enumerate() {
                let index = start + i;
                let r = Row {
                    item,
                    index,
                    count,
                    focused: focus == Some(index),
                    current: *source == ListSource::Rows && s.row_current(item),
                };
                let mark = c.draw.len();
                s.label_end.set(None);
                let template = if r.focused { focused } else { plain };
                nodes(c, s, template, b.x, b.y + i as f32 * row_h, Some(&r));
                if r.focused && c.draw.len() > mark {
                    c.mark_focus(mark);
                }
            }
            if let Some(sc) = scroll {
                if count > *rows {
                    let x = b.x + sc.x;
                    if let Some(t) = sc.track {
                        c.rect(x, b.y, sc.w, b.h, s.paint(t));
                    }
                    let th = (b.h * *rows as f32 / count as f32).max(8.).round();
                    let span = (count - rows) as f32;
                    let y = b.y + ((b.h - th) * start as f32 / span).round();
                    shape(
                        c,
                        Bx {
                            x,
                            y,
                            w: sc.w,
                            h: th,
                        },
                        sc.w / 2.,
                        0.,
                        s.paint(sc.thumb),
                    );
                }
            }
        }
        El::Group { dx, dy, children } => nodes(c, s, children, ox + dx, oy + dy, row),
        El::Native {
            part,
            dx,
            dy,
            recolor,
        } => {
            let start = c.draw.len();
            match part {
                NativePart::Screen => {
                    if let Some(native) = s.native {
                        native(c);
                    }
                }
                NativePart::Status => {
                    components::status_cluster(c, s.ui, s.m, color::MUTED, false);
                }
            }
            if *recolor {
                remap(c, start, &s.skin.palette, s.ui.cover_accent());
            }
            for q in &mut c.draw[start..] {
                q.x += ox + dx;
                q.y += oy + dy;
                if let Some(w) = &mut q.warp {
                    for p in &mut w.corners {
                        p[0] += ox + dx;
                        p[1] += oy + dy;
                    }
                }
                if s.opacity < 1. {
                    q.color = crate::theme::fade(q.color, s.opacity);
                }
            }
        }
    }
}

/// Draw a layout. Exactly one focus target is kept (the first marked).
pub(crate) fn draw_layout(c: &mut Canvas, s: &Scene, layout: &[Node]) {
    let start = c.draw.len();
    nodes(c, s, layout, 0., 0., None);
    let mut seen = false;
    for q in &mut c.draw[start..] {
        if q.focus_target {
            q.focus_target = !seen;
            seen = true;
        }
    }
}

/// Recolour Reborn's own drawing from `start` with the skin's palette: each
/// colour of Reborn's design keeps its role (and its opacity) in the skin.
/// Pictures (artwork, thumbnails, skin images) keep their colours.
pub(crate) fn remap(c: &mut Canvas, start: usize, p: &Palette, accent: u32) {
    let table: [(u32, u32); 16] = [
        (0x06070A, p.bg),
        (0x000000, p.scrim),
        (0xF6F6F8, p.text),
        (0xDEE1E8, p.text),
        (0xF2F3F5, p.text),
        (0xA0A4AE, p.muted),
        (0xAAAEBA, p.muted),
        (0x848894, p.dim),
        (0xFFFFFF, p.glass),
        (0x0E1014, p.sheet),
        (0x14161C, p.sheet),
        (0x1E2023, p.track),
        (0x1E2026, p.track),
        (0xFF7864, p.danger),
        (0x3CC878, p.success),
        (0xFFBE3C, p.warning),
    ];
    let default_accent = color::ACCENT >> 8;
    let map = |c: u32| -> u32 {
        let (rgb, a) = (c >> 8, c & 0xFF);
        // Reborn draws controls in its fixed accent; a skin keeps its own
        // (or, without one, the cover's) as before UI v4.
        let to = if rgb == default_accent {
            Some(accent)
        } else {
            table
                .iter()
                .find(|(from, _)| *from == rgb)
                .map(|(_, to)| *to)
        };
        match to {
            Some(to) => (to & 0xFFFF_FF00) | ((a * (to & 0xFF) + 127) / 255),
            None => c,
        }
    };
    for q in &mut c.draw[start..] {
        if matches!(
            q.tex,
            Tex::Art | Tex::CollectionArt | Tex::Thumb(_) | Tex::Skin
        ) {
            continue;
        }
        q.color = map(q.color);
        // A warped quad is drawn with its corner colours (Cover Flow's shelf
        // fades into the background, Y2Reborn #57; the progress ribbon).
        if let Some(w) = &mut q.warp {
            w.colors = w.colors.map(map);
        }
    }
}
