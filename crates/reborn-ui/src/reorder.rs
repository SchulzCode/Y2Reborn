//! Moving an item to another place in a list: Home's places (Customize > Home Menu, Y2Reborn #53)
//! and the Playlists list (#33). Hold Select on an item (a playlist: its menu's Move), turn the
//! wheel to carry it, Select to place it, Back to leave it where it was. Settings modes that take
//! the wheel (the Brightness card) come in through the same entry.
use crate::*;
use reborn_core::home::{moved, ordered};

/// An item being moved: where it was and where it is now, as places in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Carry {
    pub from: usize,
    pub to: usize,
}

impl Carry {
    fn at(place: usize) -> Self {
        Self {
            from: place,
            to: place,
        }
    }
    /// One wheel step or arrow: the item moves one place, within `count` places.
    fn step(self, action: Action, count: usize) -> Option<Self> {
        let delta: i64 = match action {
            Action::WheelClockwise(_) | Action::NavigateDown => 1,
            Action::WheelCounterClockwise(_) | Action::NavigateUp => -1,
            _ => return None,
        };
        let last = count.saturating_sub(1) as i64;
        let to = (self.to as i64 + delta).clamp(0, last) as usize;
        Some(Self { to, ..self })
    }
}

/// What a key press does to a list while an item is carried.
enum CarryInput {
    Moved(Carry),
    Place,
    Cancel,
}

fn carry_input(carry: Carry, action: Action, count: usize) -> CarryInput {
    match action {
        Action::Select => CarryInput::Place,
        Action::Back | Action::Home => CarryInput::Cancel,
        _ => carry
            .step(action, count)
            .map_or(CarryInput::Moved(carry), CarryInput::Moved),
    }
}

impl Ui {
    /// The settings modes that take the wheel; `None` leaves the action to the screen.
    pub(crate) fn mode_input(&mut self, m: &mut AppModel, action: Action) -> Option<Effect> {
        if let Some(effect) = self.brightness_input(m, action) {
            return Some(effect);
        }
        if let Some(effect) = self.home_move_input(m, action) {
            return Some(effect);
        }
        self.playlist_order_input(m, action)
    }

    /// Home's places in the order the Home Menu shows them, with a carried place at its new spot.
    pub(crate) fn home_menu_places(&self, m: &AppModel) -> Vec<(&'static str, &'static str)> {
        let places = m.settings.home_places();
        match self.home_moving {
            Some(c) => moved(&places, c.from, c.to),
            None => places,
        }
    }

    /// Hold Select on the Home Menu: carry the focused place.
    pub(crate) fn start_home_move(&mut self, m: &AppModel) {
        let count = m.settings.home_places().len();
        if m.navigation.focus < count {
            self.home_moving = Some(Carry::at(m.navigation.focus));
        }
    }

    fn home_move_input(&mut self, m: &mut AppModel, action: Action) -> Option<Effect> {
        let carry = self.home_moving?;
        if m.screen != Screen::HomeMenu || m.navigation.modal.is_some() {
            self.home_moving = None;
            return None;
        }
        let places = m.settings.home_places();
        Some(match carry_input(carry, action, places.len()) {
            CarryInput::Moved(c) => {
                self.home_moving = Some(c);
                m.navigation.focus = c.to;
                Effect::None
            }
            CarryInput::Place => {
                self.home_moving = None;
                let order = moved(&places, carry.from, carry.to);
                Effect::SetHomeOrder(order.iter().map(|(key, _)| key.to_string()).collect())
            }
            CarryInput::Cancel => {
                self.home_moving = None;
                m.navigation.focus = carry.from;
                Effect::None
            }
        })
    }

    /// The playlists as the Playlists list shows them: the listener's order, a carried one at its
    /// new spot. Indices into `playlists.lists`.
    pub(crate) fn playlist_order(&self, m: &AppModel) -> Vec<usize> {
        let paths: Vec<(usize, String)> = (self.playlists.lists.iter().enumerate())
            .map(|(i, p)| (i, p.path.to_string_lossy().into_owned()))
            .collect();
        let order: Vec<usize> = ordered(&paths, &m.settings.playlist_order, |(_, p)| p)
            .into_iter()
            .map(|(i, _)| *i)
            .collect();
        match self.playlists.carried {
            Some(c) => moved(&order, c.from, c.to),
            None => order,
        }
    }

    /// Move in a playlist's menu: carry it (`index` in `playlists.lists`).
    pub(crate) fn start_playlist_move(&mut self, m: &mut AppModel, index: usize) {
        if let Some(place) = self.playlist_order(m).iter().position(|i| *i == index) {
            self.playlists.carried = Some(Carry::at(place));
        }
    }

    fn playlist_order_input(&mut self, m: &mut AppModel, action: Action) -> Option<Effect> {
        let carry = self.playlists.carried?;
        if m.screen != Screen::Playlists || m.navigation.modal.is_some() {
            self.playlists.carried = None;
            return None;
        }
        // New Playlist stands first, above the playlists.
        const LEAD: usize = 1;
        let count = self.playlists.lists.len();
        Some(match carry_input(carry, action, count) {
            CarryInput::Moved(c) => {
                self.playlists.carried = Some(c);
                m.navigation.focus = LEAD + c.to;
                Effect::None
            }
            CarryInput::Place => {
                let order = self.playlist_order(m);
                self.playlists.carried = None;
                let paths = order
                    .iter()
                    .map(|i| self.playlists.lists[*i].path.to_string_lossy().into_owned());
                Effect::SetPlaylistOrder(paths.collect())
            }
            CarryInput::Cancel => {
                self.playlists.carried = None;
                m.navigation.focus = LEAD + carry.from;
                Effect::None
            }
        })
    }
}
