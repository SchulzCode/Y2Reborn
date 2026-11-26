//! Settings → Library → Storage: a card per volume with its bar split by what fills it (music,
//! audiobooks, films, other), a legend with size, count and play time, and the actions below.
use crate::{
    canvas::Canvas,
    components,
    pages::Page,
    storage_use::{self, Kinds, Share},
    text::{self, Face},
    theme::{color, HEIGHT, WIDTH},
    views_settings::internal_note,
    RowKind, Ui,
};
use reborn_core::{
    platform::{human_bytes, SdCard, VolumeSpace},
    AppModel, Track,
};

const MUSIC: u32 = 0x6EA0FFFF;
const BOOKS: u32 = 0xE8A23CFF;
const VIDEOS: u32 = 0xB48CFFFF;
const OTHER: u32 = 0x8A90A0FF;

/// Card text starts here; the legend has two columns of `COLUMN` each.
const SIDE: f32 = 28.;
const COLUMN: f32 = 212.;
/// A volume without a bar (no card, not set up): the card the page always had.
const NOTE_HEIGHT: f32 = 76.;
/// The first card starts here, under the status bar.
const TOP: f32 = 40.;
/// An action row, and what the last drawn thing keeps clear of the screen's edge.
const ACTION: f32 = 44.;
const MARGIN: f32 = 4.;

/// The page's vertical rhythm. With room to spare it opens up from tight toward loose, so the lines
/// stay easy to tell apart; when even tight does not fit (both cards with two legend rows and two
/// actions) it squeezes and the footer lines go.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Spacing {
    /// A card's top to its legend (name and bar).
    head: f32,
    /// One legend row: the size line, the count line under it and the air before the next row.
    pitch: f32,
    /// The footer line under the legend.
    foot: f32,
    /// Between cards.
    gap: f32,
    /// A legend entry's size line to its count line.
    line: f32,
}

impl Spacing {
    const TIGHT: Spacing = Spacing {
        head: 50.,
        pitch: 24.,
        foot: 20.,
        gap: 8.,
        line: 12.,
    };
    const LOOSE: Spacing = Spacing {
        head: 58.,
        pitch: 38.,
        foot: 28.,
        gap: 12.,
        line: 16.,
    };
    /// No footer line (a card's used and total show in its header and bar).
    const SQUEEZED: Spacing = Spacing {
        head: 48.,
        pitch: 26.,
        foot: 4.,
        gap: 6.,
        line: 12.,
    };

    fn between(k: f32) -> Spacing {
        let mix = |a: f32, b: f32| a + (b - a) * k;
        let (t, l) = (Self::TIGHT, Self::LOOSE);
        Spacing {
            head: mix(t.head, l.head),
            pitch: mix(t.pitch, l.pitch),
            foot: mix(t.foot, l.foot),
            gap: mix(t.gap, l.gap),
            line: mix(t.line, l.line),
        }
    }

    /// Where the bar sits in a card: between the name and the legend, a little lower with more room.
    fn bar(&self) -> f32 {
        34. + (self.head - Self::TIGHT.head) / 2.
    }

    /// A card with a bar and `rows` legend rows.
    fn card(&self, rows: usize) -> f32 {
        self.head + rows as f32 * self.pitch + self.foot
    }

    /// Whether the footer line (used of total) is drawn.
    fn footer(&self) -> bool {
        self.foot >= Self::TIGHT.foot
    }

    /// Where the page ends: the cards, a gap before each, and the actions after a gap. `cards` holds
    /// each card's legend rows, None for a card that only carries a note.
    fn end(&self, cards: &[Option<usize>], actions: usize) -> f32 {
        let cards: f32 = cards
            .iter()
            .map(|rows| rows.map_or(NOTE_HEIGHT, |rows| self.card(rows)) + self.gap)
            .sum();
        let actions = if actions > 0 {
            self.gap + 2. + actions as f32 * ACTION
        } else {
            0.
        };
        TOP + cards + actions
    }

    /// The loosest spacing that still leaves every card and `actions` rows on the screen.
    fn fit(cards: &[Option<usize>], actions: usize) -> Spacing {
        let limit = HEIGHT - MARGIN;
        let (tight, loose) = (
            Self::TIGHT.end(cards, actions),
            Self::LOOSE.end(cards, actions),
        );
        if tight > limit {
            return Self::SQUEEZED;
        }
        if loose <= tight {
            return Self::TIGHT;
        }
        Self::between(((limit - tight) / (loose - tight)).clamp(0., 1.))
    }
}

/// One legend entry: a kind of file and what it takes.
struct Entry {
    label: &'static str,
    tint: u32,
    bytes: u64,
    detail: String,
}

impl Ui {
    /// The internal storage's and the SD card's files by kind: summed once per library, not per frame.
    fn library_kinds(&self, m: &AppModel, tracks: &[Track]) -> (Kinds, Kinds) {
        let stamp = crate::library_stamp(tracks);
        let mut cache = self.storage_use.borrow_mut();
        let map = match cache.take() {
            Some((seen, map)) if seen == stamp => map,
            _ => storage_use::by_source(tracks, &self.books(tracks)),
        };
        let kinds = (
            storage_use::volume(&map, &m.sources, true),
            storage_use::volume(&map, &m.sources, false),
        );
        *cache = Some((stamp, map));
        kinds
    }
}

pub fn screen(
    c: &mut Canvas,
    ui: &Ui,
    m: &AppModel,
    tracks: &[Track],
    page: &Page,
    focus: usize,
    interactive: bool,
) {
    let snapshot = &m.platform.snapshot;
    let (internal, sd) = ui.library_kinds(m, tracks);
    let volumes = [
        (
            "Internal Storage",
            // Not set up yet (STORAGE-SETUP): no bar for the small Y2DATA.
            internal_note(snapshot.internal_setup)
                .is_none()
                .then_some(&snapshot.storage.internal),
            internal,
        ),
        (
            "SD Card",
            match &snapshot.storage.sd {
                SdCard::Ready(space) | SdCard::ReadOnly(space) | SdCard::Unchecked(space) => {
                    Some(space)
                }
                _ => None,
            },
            sd,
        ),
    ];
    // Each card's legend first: how many rows it needs decides how much air the page can give.
    let cards: Vec<_> = volumes
        .into_iter()
        .map(|(name, space, kinds)| {
            let legend = space.map(|space| {
                entries(
                    &kinds,
                    storage_use::other_bytes(space, kinds.bytes()).unwrap_or(0),
                )
            });
            (name, space, legend)
        })
        .collect();
    let rows: Vec<_> = cards
        .iter()
        .map(|(_, _, legend)| legend.as_ref().map(|l| l.len().div_ceil(2)))
        .collect();
    let spacing = Spacing::fit(&rows, page.rows.len());
    let mut y = TOP;
    for (i, (name, space, legend)) in cards.iter().enumerate() {
        y += spacing.gap;
        y += match (space, legend) {
            (Some(space), Some(legend)) => volume(c, y, name, space, legend, &spacing),
            _ => {
                components::card(c, 10., y, WIDTH - 20., NOTE_HEIGHT);
                c.text_mid(SIDE, y + 22., 250., name, Face::S15, color::TEXT);
                let note = missing_note(m, i == 0);
                c.text_mid(SIDE, y + 50., 300., note, Face::R13, color::MUTED);
                NOTE_HEIGHT
            }
        };
    }
    actions(c, y + spacing.gap, page, focus, interactive);
}

/// Why a volume shows no bar.
fn missing_note(m: &AppModel, internal: bool) -> &'static str {
    let snapshot = &m.platform.snapshot;
    if internal {
        return internal_note(snapshot.internal_setup).unwrap_or("Checking…");
    }
    match snapshot.storage.sd {
        SdCard::Error => "Card can't be read",
        SdCard::NeedsRepair => "Needs repair · Check it on a computer",
        SdCard::CheckTimedOut => "Check took too long · Insert it again",
        // Lent to the computer (SD Card as a USB Stick): there, not missing.
        SdCard::OnComputer => "On the computer · Eject it there",
        _ => "No SD card",
    }
}

/// The legend: a kind of file with a size, when there is any (Other is what the volume holds beyond
/// the library; with no library read yet it is all "Used").
fn entries(kinds: &Kinds, other: u64) -> Vec<Entry> {
    let detail = |count: String, share: &Share, timed: bool| {
        if timed && share.ms > 0 {
            format!("{count} · {}", storage_use::span(share.ms))
        } else {
            count
        }
    };
    let kind = |label, tint, share: &Share, count: String, timed| Entry {
        label,
        tint,
        bytes: share.bytes,
        detail: detail(count, share, timed),
    };
    let (music, books, videos) = (&kinds.music, &kinds.books, &kinds.videos);
    let mut list = vec![
        kind(
            "Music",
            MUSIC,
            music,
            components::count(music.count, "song", "songs"),
            true,
        ),
        kind(
            "Audiobooks",
            BOOKS,
            books,
            components::count(books.count, "book", "books"),
            true,
        ),
        kind(
            "Videos",
            VIDEOS,
            videos,
            components::count(videos.count, "video", "videos"),
            false,
        ),
    ];
    list.retain(|e| e.bytes > 0);
    if other > 0 {
        let library = !list.is_empty();
        list.push(Entry {
            label: if library { "Other" } else { "Used" },
            tint: OTHER,
            bytes: other,
            detail: if library {
                "Playlists, art, other files".into()
            } else {
                String::new()
            },
        });
    }
    list
}

/// A volume's card at `y`; its height.
fn volume(
    c: &mut Canvas,
    y: f32,
    name: &str,
    space: &VolumeSpace,
    list: &[Entry],
    spacing: &Spacing,
) -> f32 {
    let legend = y + spacing.head;
    let height = spacing.card(list.len().div_ceil(2));
    components::card(c, 10., y, WIDTH - 20., height);
    c.text_mid(SIDE, y + 22., 250., name, Face::S15, color::TEXT);
    // Without a footer line the header carries the total.
    let free = match (space.free_bytes, space.total_bytes) {
        (Some(f), Some(t)) if !spacing.footer() => {
            format!("{} free of {}", human_bytes(f), human_bytes(t))
        }
        (Some(f), _) => format!("{} free", human_bytes(f)),
        _ => String::new(),
    };
    c.text_mid_right(WIDTH - SIDE, y + 22., &free, Face::R11, color::MUTED);
    bar(c, y + spacing.bar(), space.total_bytes, list);
    for (i, entry) in list.iter().enumerate() {
        let x = SIDE + (i % 2) as f32 * COLUMN;
        cell(
            c,
            x,
            legend + (i / 2) as f32 * spacing.pitch,
            spacing.line,
            entry,
        );
    }
    if spacing.footer() {
        let footer = match (space.total_bytes, space.free_bytes) {
            (Some(total), Some(free)) => format!(
                "{} used of {}",
                human_bytes(total.saturating_sub(free)),
                human_bytes(total)
            ),
            (Some(total), None) => format!("{} total", human_bytes(total)),
            _ => String::new(),
        };
        c.text_mid(
            SIDE,
            y + height - 14. - (spacing.foot - Spacing::TIGHT.foot) / 2.,
            300.,
            &footer,
            Face::R11,
            color::MUTED,
        );
    }
    height
}

/// The bar: each kind in its colour from the left, the rest of the track free.
fn bar(c: &mut Canvas, y: f32, total: Option<u64>, list: &[Entry]) {
    let width = WIDTH - 2. * SIDE;
    c.rrect(SIDE, y, width, 8., 4., color::GLASS_STRONG);
    let sum: u64 = list.iter().map(|e| e.bytes).sum();
    let scale = total.unwrap_or(0).max(sum);
    if scale == 0 {
        return;
    }
    let end = SIDE + width;
    let mut x = SIDE;
    for (i, entry) in list.iter().enumerate() {
        let part = (width * entry.bytes as f32 / scale as f32)
            .max(2.)
            .min(end - x);
        if i == 0 {
            // The first segment is rounded on its left; the next one covers what it runs under.
            let under = if list.len() > 1 { 4. } else { 0. };
            c.rrect(x, y, (part + under).min(end - x), 8., 4., entry.tint);
        } else {
            c.rect(x, y, part, 8., entry.tint);
        }
        x += part;
    }
}

/// One legend entry at `x`, `y`: its dot, name and size, with the count and play time `line` under.
fn cell(c: &mut Canvas, x: f32, y: f32, line: f32, entry: &Entry) {
    c.disc(x + 4., y + 6., 4., entry.tint);
    c.text_mid(x + 14., y + 6., 110., entry.label, Face::R13, color::TEXT);
    c.text_mid_right(
        x + COLUMN - 14.,
        y + 6.,
        &human_bytes(entry.bytes),
        Face::R13,
        color::TEXT,
    );
    let detail = text::fit(Face::R11, &entry.detail, COLUMN - 28.);
    c.text_mid(
        x + 14.,
        y + 6. + line,
        COLUMN - 20.,
        &detail,
        Face::R11,
        color::MUTED,
    );
}

/// The actions under the two volumes, straight on the screen as every list (#74).
fn actions(c: &mut Canvas, top: f32, page: &Page, focus: usize, interactive: bool) {
    for (i, row) in page.rows.iter().enumerate() {
        let y = top + 2. + i as f32 * 44.;
        if interactive && i == focus && row.enabled {
            // As wide as the cards above it: nothing scrolls here, so no room is kept for a bar.
            components::focus_row(c, 10., y + 1., WIDTH - 20., 42.);
        }
        let face = if interactive && i == focus {
            Face::S15
        } else {
            Face::R15
        };
        c.text_mid(
            SIDE,
            y + 22.,
            280.,
            &row.label,
            face,
            if row.enabled { color::TEXT } else { color::DIM },
        );
        // A switch is drawn as one (SD Card as a USB Stick), every other row leads on.
        if let RowKind::Toggle(on) = row.kind {
            components::toggle(c, WIDTH - 22., y + 22., on);
            continue;
        }
        let value = text::fit(Face::R11, &row.secondary, 190.);
        c.text_mid_right(WIDTH - 40., y + 22., &value, Face::R11, color::MUTED);
        components::chevron(c, WIDTH - 22., y + 22.);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: f32 = HEIGHT - MARGIN;

    #[test]
    fn a_page_that_cannot_fit_squeezes_and_still_ends_on_the_screen() {
        // Both cards with two legend rows and two action rows do not fit even tight.
        let cards = [Some(2), Some(2)];
        assert!(Spacing::TIGHT.end(&cards, 2) > LIMIT);
        let s = Spacing::fit(&cards, 2);
        assert_eq!(s, Spacing::SQUEEZED);
        assert!(!s.footer() && s.end(&cards, 2) <= LIMIT);
    }

    #[test]
    fn spare_room_opens_the_page_up_and_never_past_loose() {
        let roomy = Spacing::fit(&[Some(1), Some(1)], 0);
        assert!(roomy.pitch > Spacing::TIGHT.pitch && roomy.line > Spacing::TIGHT.line);
        assert_eq!(Spacing::fit(&[None, None], 0), Spacing::LOOSE);
        // More to show leaves less air.
        let more = Spacing::fit(&[Some(1), Some(2)], 0);
        assert!(roomy.pitch >= more.pitch && more.footer());
    }

    #[test]
    fn whatever_is_shown_the_page_ends_on_the_screen() {
        let shapes = [None, Some(1), Some(2)];
        for a in shapes {
            for b in shapes {
                for actions in 0..=2 {
                    let cards = [a, b];
                    let s = Spacing::fit(&cards, actions);
                    let end = s.end(&cards, actions);
                    assert!(end <= LIMIT + 0.01, "{cards:?} {actions} actions: {end}");
                }
            }
        }
    }

    #[test]
    fn the_action_rows_keep_a_gap_under_the_last_card() {
        let s = Spacing::fit(&[Some(1), Some(1)], 1);
        let with = s.end(&[Some(1), Some(1)], 1);
        let without = s.end(&[Some(1), Some(1)], 0);
        assert!(with - without >= ACTION + s.gap);
    }
}
