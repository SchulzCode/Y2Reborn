//! Customize > Buttons: the layouts, a drawing of the player per layer to change a button, the
//! command pickers, Save with its check and the Keep-or-Undo sheet; and what the router needs to
//! know about the screen (`Ui::button_context`). Plan: docs/ui/REBORN-BUTTON-MAPPER.md.
//!
//! Every page here answers as Reborn Default, whatever layout is in use, so the place that fixes a
//! layout always works the same way. Edits stay a draft until Save; Save checks the layout, walks
//! the menus with it, and keeps it only when Keep is chosen with the new buttons within
//! `TRIAL` (otherwise the old layout comes back by itself).
use crate::{
    pages::{row, Page},
    Item, RowKind, Ui,
};
use reborn_core::{
    buttons::{
        built_in, ButtonContext, ButtonSettings, Command, Control, Gesture, HoldTime, Layer,
        Layout, REBORN_DEFAULT,
    },
    AppModel, ConfirmAction, Effect, Modal, PlatformTask, Screen, SleepChoice, Track,
};
use std::time::{Duration, Instant};

/// The main page's navigation filter (also reached as the older "wheel").
pub(crate) const BUTTONS: &str = "buttons";
/// How long a new layout waits for Keep before the old one comes back.
pub const TRIAL: Duration = Duration::from_secs(15);

/// The button pages' state beside the model: the unsaved draft, the trial after a save, and why a
/// save was refused.
#[derive(Default)]
pub(crate) struct ButtonsUi {
    pub draft: Option<Layout>,
    pub trial: Option<(ButtonSettings, Instant)>,
    pub problem: Option<String>,
}

/// One of the button pages, read from the navigation filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ButtonPage {
    Main,
    Layouts,
    Hold,
    Face(Layer),
    Control(Layer, Control),
    Pick(Layer, Control, Gesture),
}

pub(crate) fn parse(filter: &str) -> Option<ButtonPage> {
    if filter == BUTTONS || filter == crate::customize::WHEEL {
        return Some(ButtonPage::Main);
    }
    let mut parts = filter.strip_prefix("buttons:")?.split(':');
    let first = parts.next()?;
    let (control, gesture) = (parts.next(), parts.next());
    if parts.next().is_some() {
        return None;
    }
    Some(match (first, control, gesture) {
        ("layouts", None, _) => ButtonPage::Layouts,
        ("hold", None, _) => ButtonPage::Hold,
        (layer, None, _) => ButtonPage::Face(Layer::from_key(layer)?),
        (layer, Some(control), None) => {
            ButtonPage::Control(Layer::from_key(layer)?, Control::from_key(control)?)
        }
        (layer, Some(control), Some(gesture)) => ButtonPage::Pick(
            Layer::from_key(layer)?,
            Control::from_key(control)?,
            Gesture::from_key(gesture)?,
        ),
    })
}

/// The drawing of the player: a layer's face page.
pub(crate) fn face(m: &AppModel) -> Option<Layer> {
    match (m.screen, parse(&m.navigation.filter)) {
        (Screen::Customize, Some(ButtonPage::Face(layer))) => Some(layer),
        _ => None,
    }
}

pub(crate) fn title(page: ButtonPage) -> String {
    match page {
        ButtonPage::Main => "Buttons".into(),
        ButtonPage::Layouts => "Layout".into(),
        ButtonPage::Hold => "Hold Time".into(),
        ButtonPage::Face(layer) => layer.label().into(),
        ButtonPage::Control(_, control) => control.label().into(),
        ButtonPage::Pick(_, control, gesture) => {
            format!("{} · {}", control.label(), gesture_name(control, gesture))
        }
    }
}

fn gesture_name(control: Control, gesture: Gesture) -> &'static str {
    match (control.is_wheel(), gesture) {
        (true, _) => "Turn",
        (false, Gesture::Press) => "Press",
        (false, Gesture::Hold) => "Hold",
    }
}

fn gestures(control: Control) -> &'static [Gesture] {
    if control.is_wheel() {
        &[Gesture::Press]
    } else {
        &[Gesture::Press, Gesture::Hold]
    }
}

fn hold_meaning(hold: HoldTime) -> &'static str {
    match hold {
        HoldTime::Short => "A hold starts after a little under half a second.",
        HoldTime::Normal => {
            "A hold starts after about two thirds of a second, as Reborn always did."
        }
        HoldTime::Long => "A hold starts after a full second: for slow, firm presses.",
    }
}

impl Ui {
    /// The layout being edited: the draft, or the layout in use.
    pub(crate) fn editing(&self, m: &AppModel) -> Layout {
        self.buttons
            .draft
            .clone()
            .unwrap_or_else(|| m.settings.buttons.layout())
    }

    /// What a button does on a face page, in a line: "Next · held: Fast Forward".
    pub(crate) fn button_summary(&self, m: &AppModel, layer: Layer, control: Control) -> String {
        let layout = self.editing(m);
        let press = layout.resolve(layer, control, Gesture::Press).label();
        if control.is_wheel() {
            return press.into();
        }
        match layout.resolve(layer, control, Gesture::Hold) {
            Command::Nothing => press.into(),
            hold => format!("{press} · held: {}", hold.label()),
        }
    }

    /// The rows of a button page and the line under them.
    pub(crate) fn button_page(&self, m: &AppModel, page: ButtonPage) -> Page {
        let (rows, status) = match page {
            ButtonPage::Main => self.main_page(m),
            ButtonPage::Layouts => layouts_page(m),
            ButtonPage::Hold => hold_page(m),
            ButtonPage::Face(layer) => {
                let rows = Control::ALL
                    .into_iter()
                    .map(|c| {
                        let key = format!("customize:buttons:{}:{}", layer.key(), c.key());
                        row(c.label(), &key, self.button_summary(m, layer, c))
                    })
                    .collect();
                let hint = "Turn to pick a button, or press it. Select changes it.";
                (rows, Some(hint.into()))
            }
            ButtonPage::Control(layer, control) => control_page(&self.editing(m), layer, control),
            ButtonPage::Pick(layer, control, gesture) => {
                pick_page(m, &self.editing(m), layer, control, gesture)
            }
        };
        let mut page = Page::rows(rows);
        page.status = status;
        page
    }

    /// Buttons itself: the layout, the three layers, Save or Delete, the hold time.
    fn main_page(&self, m: &AppModel) -> (Vec<Item>, Option<String>) {
        let settings = &m.settings.buttons;
        let layout = self.editing(m);
        let edited = if self.buttons.draft.is_some() {
            " · Edited"
        } else {
            ""
        };
        let mut rows = vec![row(
            "Layout",
            "customize:buttons:layouts",
            format!("{}{edited}", settings.active_name()),
        )
        .with_kind(RowKind::Chevron)];
        for layer in Layer::ALL {
            let summary = match layout.changed(layer).len() {
                0 => "As Reborn Default".to_owned(),
                1 => "1 button changed".to_owned(),
                n => format!("{n} buttons changed"),
            };
            let key = format!("customize:buttons:{}", layer.key());
            rows.push(row(layer.label(), &key, summary).with_kind(RowKind::Chevron));
        }
        if self.buttons.draft.is_some() {
            rows.push(row("Save Changes", "btn_save", ""));
            rows.push(row("Discard Changes", "btn_discard", ""));
        } else if settings.is_own() {
            rows.push(row("Delete This Layout", "btn_delete", ""));
        }
        rows.push(
            row("Hold Time", "customize:buttons:hold", settings.hold.label())
                .with_kind(RowKind::Chevron),
        );
        let focused = rows.get(m.navigation.focus).map(|r| r.key.clone());
        let hint = self
            .buttons
            .problem
            .clone()
            .or_else(|| main_hint(focused.as_deref()?).map(str::to_owned));
        (rows, hint)
    }

    /// Where a button page opens: a picker on its current choice.
    pub(crate) fn button_page_focus(&self, m: &AppModel, filter: &str) -> usize {
        let layout = self.editing(m);
        match parse(filter) {
            Some(ButtonPage::Layouts) => {
                let ids: Vec<String> = built_in()
                    .into_iter()
                    .map(|(id, ..)| id.to_owned())
                    .chain((0..m.settings.buttons.own.len()).map(|i| format!("own:{i}")))
                    .collect();
                ids.iter()
                    .position(|id| *id == m.settings.buttons.active)
                    .unwrap_or(0)
            }
            Some(ButtonPage::Hold) => HoldTime::ALL
                .iter()
                .position(|&h| h == m.settings.buttons.hold)
                .unwrap_or(1),
            Some(ButtonPage::Pick(layer, control, gesture)) => {
                let first = usize::from(layer == Layer::NowPlaying);
                match layout.get(layer, control, gesture) {
                    None if layer == Layer::NowPlaying => 0,
                    _ => {
                        let current = layout.resolve(layer, control, gesture);
                        offered(layer)
                            .iter()
                            .position(|&c| c == current)
                            .map_or(0, |i| i + first)
                    }
                }
            }
            _ => 0,
        }
    }

    /// Select on a row of the button pages ("btn_…").
    pub(crate) fn button_select(&mut self, m: &mut AppModel, key: &str) -> Option<Effect> {
        let mut settings = m.settings.buttons.clone();
        if let Some(id) = key.strip_prefix("btn_use:") {
            settings.preset(id)?;
            settings.active = id.into();
            self.buttons.draft = None;
            self.buttons.problem = None;
            Self::back(m);
            self.flash(format!("Buttons: {}", settings.active_name()));
            return Some(Effect::SetButtons(Box::new(settings)));
        }
        if let Some(index) = key.strip_prefix("btn_hold:") {
            settings.hold = *HoldTime::ALL.get(index.parse::<usize>().ok()?)?;
            Self::back(m);
            return Some(Effect::SetButtons(Box::new(settings)));
        }
        if let Some(rest) = key.strip_prefix("btn_set:") {
            let parts: Vec<&str> = rest.split(':').collect();
            let [layer, control, gesture, command] = parts.as_slice() else {
                return None;
            };
            let (layer, control, gesture) = (
                Layer::from_key(layer)?,
                Control::from_key(control)?,
                Gesture::from_key(gesture)?,
            );
            let command = match *command {
                "same" => None,
                c => Some(Command::from_key(c)?),
            };
            let mut draft = self.editing(m);
            draft.set(layer, control, gesture, command);
            self.set_draft(m, draft);
            Self::back(m);
            return Some(Effect::None);
        }
        if let Some(rest) = key.strip_prefix("btn_reset:") {
            let (layer, control) = rest.split_once(':')?;
            let (layer, control) = (Layer::from_key(layer)?, Control::from_key(control)?);
            let mut draft = self.editing(m);
            for &gesture in gestures(control) {
                draft.set(
                    layer,
                    control,
                    gesture,
                    Layout::default().get(layer, control, gesture),
                );
            }
            self.set_draft(m, draft);
            Self::back(m);
            return Some(Effect::None);
        }
        Some(match key {
            "btn_discard" => {
                self.buttons.draft = None;
                self.buttons.problem = None;
                Effect::None
            }
            "btn_delete" => {
                settings.delete_active();
                m.navigation.focus = 0;
                Effect::SetButtons(Box::new(settings))
            }
            "btn_save" => self.save_buttons(m),
            _ => return None,
        })
    }

    /// A draft equal to the layout in use is no draft.
    fn set_draft(&mut self, m: &AppModel, draft: Layout) {
        self.buttons.problem = None;
        self.buttons.draft = (draft != m.settings.buttons.layout()).then_some(draft);
    }

    /// Save: the check, the walk through the menus, then the new layout on trial.
    fn save_buttons(&mut self, m: &mut AppModel) -> Effect {
        let Some(draft) = self.buttons.draft.clone() else {
            return Effect::None;
        };
        if let Err(problem) = draft
            .check()
            .map_err(|p| p.message())
            .and_then(|()| walk(&draft))
        {
            self.flash("These buttons can't be saved");
            self.buttons.problem = Some(problem);
            return Effect::None;
        }
        let previous = m.settings.buttons.clone();
        let mut settings = previous.clone();
        settings.save(draft);
        self.buttons.draft = None;
        self.buttons.problem = None;
        self.buttons.trial = Some((previous, Instant::now() + TRIAL));
        Self::open_modal(m, Modal::Confirm(ConfirmAction::KeepButtons));
        Effect::SetButtons(Box::new(settings))
    }

    /// The Keep-or-Undo sheet's answer.
    pub(crate) fn keep_buttons(&mut self, m: &mut AppModel, keep: bool) -> Effect {
        Self::close_modal(m);
        let Some((previous, _)) = self.buttons.trial.take() else {
            return Effect::None;
        };
        if keep {
            self.flash("New buttons kept");
            Effect::None
        } else {
            self.flash("The old buttons are back");
            Effect::SetButtons(Box::new(previous))
        }
    }

    /// The trial ran out without Keep: the layout to go back to (the app stores it).
    pub fn buttons_trial_expired(
        &mut self,
        m: &mut AppModel,
        now: Instant,
    ) -> Option<ButtonSettings> {
        let (_, until) = self.buttons.trial.as_ref()?;
        if now < *until {
            return None;
        }
        let (previous, _) = self.buttons.trial.take()?;
        if m.navigation.modal == Some(Modal::Confirm(ConfirmAction::KeepButtons)) {
            Self::close_modal(m);
        }
        self.flash("The old buttons are back");
        Some(previous)
    }

    /// On a face page, Previous, Next, Play/Pause and the volume buttons pick themselves instead of
    /// acting (the page answers as Reborn Default, so their actions are the default ones).
    pub(crate) fn face_pick(&mut self, m: &mut AppModel, action: reborn_core::Action) -> bool {
        use reborn_core::Action as A;
        if face(m).is_none() || m.navigation.modal.is_some() || m.screen_off {
            return false;
        }
        let control = match action {
            A::PreviousTrack | A::SeekBackward => Control::Previous,
            A::NextTrack | A::SeekForward => Control::Next,
            A::PlayPause | A::ShowNowPlaying => Control::PlayPause,
            A::VolumeUp => Control::VolumeUp,
            A::VolumeDown => Control::VolumeDown,
            _ => return false,
        };
        m.navigation.focus = Control::ALL.iter().position(|&c| c == control).unwrap_or(0);
        true
    }

    /// What the router needs to know about the screen to turn a button into commands.
    pub fn button_context(&self, m: &AppModel) -> ButtonContext {
        let keyboard = self.keyboard.is_some();
        let trial = m.navigation.modal == Some(Modal::Confirm(ConfirmAction::KeepButtons));
        let button_page =
            m.screen == Screen::Customize && parse(&m.navigation.filter).is_some() && !trial;
        let now_playing = m.screen == Screen::NowPlaying
            && m.navigation.modal.is_none()
            && m.current().is_some()
            && !keyboard;
        ButtonContext {
            layout: m.settings.buttons.layout(),
            layer: if now_playing {
                Layer::NowPlaying
            } else {
                Layer::Menus
            },
            protected: button_page
                || keyboard
                || self.pairing.is_some()
                || self.alert.is_some()
                || self.update_run
                || m.navigation.modal == Some(Modal::QuickMenu),
            keyboard,
            keyboard_wheel: self
                .keyboard
                .as_ref()
                .is_some_and(crate::keyboard::Keyboard::live),
            station: m.current().is_some_and(Track::is_radio),
        }
    }

    /// A layout command without an action of its own.
    pub(crate) fn run_command(
        &mut self,
        m: &mut AppModel,
        tracks: &[Track],
        command: Command,
    ) -> Effect {
        let busy = matches!(
            m.platform.busy,
            Some(PlatformTask::UpdateApply | PlatformTask::UpdateRollback)
        );
        if self.update_run || busy || m.platform.shutting_down.is_some() {
            return Effect::None;
        }
        if m.screen_off && !command.in_the_dark() && command != Command::RebornDefault {
            return Effect::None;
        }
        let place = match command {
            Command::Queue => Some(Screen::Queue),
            Command::OpenMusic => Some(Screen::Music),
            Command::OpenAudiobooks => Some(Screen::Audiobooks),
            Command::OpenVideos => Some(Screen::Videos),
            Command::OpenPlaylists => Some(Screen::Playlists),
            Command::OpenRadio => Some(Screen::Radio),
            Command::OpenSettings => Some(Screen::Settings),
            _ => None,
        };
        if place.is_some() || command == Command::OpenSearch {
            if self.keyboard.is_some() || self.pairing.is_some() {
                return Effect::None;
            }
            if m.navigation.modal.is_some() {
                Self::close_modal(m);
            }
            match place {
                Some(screen) => Self::go(m, screen, ""),
                None => self.open_library_search(m, tracks),
            }
            return Effect::None;
        }
        match command {
            Command::RebornDefault => {
                self.buttons.draft = None;
                self.buttons.trial = None;
                self.flash("Buttons: Reborn Default");
                let mut settings = m.settings.buttons.clone();
                settings.active = REBORN_DEFAULT.into();
                Effect::SetButtons(Box::new(settings))
            }
            Command::Stop => Effect::StopPlayback,
            Command::Shuffle => {
                let next = match m.settings.shuffle_mode() {
                    reborn_core::ShuffleMode::Off => reborn_core::ShuffleMode::Songs,
                    _ => reborn_core::ShuffleMode::Off,
                };
                self.flash(format!("Shuffle: {}", next.label()));
                Effect::SetShuffle(next)
            }
            Command::Repeat => {
                let next = crate::next_repeat(m.settings.repeat);
                self.flash(format!("Repeat: {}", crate::repeat_label(next)));
                Effect::SetRepeat(next)
            }
            Command::BrightnessUp | Command::BrightnessDown => {
                let step = i16::from(reborn_core::BRIGHTNESS_STEP);
                let delta = if command == Command::BrightnessUp {
                    step
                } else {
                    -step
                };
                let level = (i16::from(m.settings.brightness) + delta)
                    .clamp(i16::from(reborn_core::BRIGHTNESS_MIN), 100);
                Effect::SetBrightness(level as u8)
            }
            Command::SleepTimer => {
                let steps = [
                    SleepChoice::Off,
                    SleepChoice::Minutes(15),
                    SleepChoice::Minutes(30),
                    SleepChoice::Minutes(45),
                    SleepChoice::Minutes(60),
                ];
                let now = m.sleep_timer.map_or(SleepChoice::Off, |t| t.choice);
                let at = steps.iter().position(|&s| s == now).map_or(0, |i| i + 1);
                let next = steps[at % steps.len()];
                self.flash(match next {
                    SleepChoice::Minutes(n) => format!("Sleep Timer: {n} min"),
                    _ => "Sleep Timer: Off".into(),
                });
                Effect::SetSleepTimer(next)
            }
            Command::Wifi => Effect::WifiPower,
            Command::Bluetooth => Effect::BluetoothPower,
            _ => Effect::None,
        }
    }
}

/// What the focused row of Buttons does, in a line.
fn main_hint(key: &str) -> Option<&'static str> {
    Some(match key {
        "customize:buttons:layouts" => "Reborn Default is always here to go back to.",
        "btn_save" => "Checks the buttons, then asks you to keep them.",
        "btn_discard" => "Forget the changes not saved.",
        "btn_delete" => "Remove this layout and use Reborn Default.",
        "customize:buttons:hold" => "How long a press must last to be a hold.",
        "customize:buttons:screen_off" => "With the screen off. Power always wakes the player.",
        k if k.starts_with("customize:buttons:") => {
            "Which button does what. Power always sleeps, wakes and opens the Quick Menu."
        }
        _ => return None,
    })
}

/// The layouts: Reborn's own, then the player's.
fn layouts_page(m: &AppModel) -> (Vec<Item>, Option<String>) {
    let settings = &m.settings.buttons;
    let mut rows: Vec<Item> = built_in()
        .into_iter()
        .map(|(id, name, ..)| {
            row(name, &format!("btn_use:{id}"), "").with_kind(RowKind::Check(settings.active == id))
        })
        .collect();
    rows.extend(settings.own.iter().enumerate().map(|(i, p)| {
        let id = format!("own:{i}");
        row(&p.name, &format!("btn_use:{id}"), "").with_kind(RowKind::Check(settings.active == id))
    }));
    let meaning = built_in()
        .into_iter()
        .nth(m.navigation.focus)
        .map_or("Your own layout.", |(_, _, meaning, _)| meaning);
    (rows, Some(meaning.into()))
}

fn hold_page(m: &AppModel) -> (Vec<Item>, Option<String>) {
    let rows = HoldTime::ALL
        .into_iter()
        .enumerate()
        .map(|(i, h)| {
            row(h.label(), &format!("btn_hold:{i}"), "")
                .with_kind(RowKind::Check(m.settings.buttons.hold == h))
        })
        .collect();
    let meaning = HoldTime::ALL
        .get(m.navigation.focus)
        .map(|&h| hold_meaning(h).into());
    (rows, meaning)
}

/// A button in a layer: its press (or turn) and hold, and the way back to Reborn Default.
fn control_page(layout: &Layout, layer: Layer, control: Control) -> (Vec<Item>, Option<String>) {
    let mut rows: Vec<Item> = gestures(control)
        .iter()
        .map(|&g| {
            let key = format!(
                "customize:buttons:{}:{}:{}",
                layer.key(),
                control.key(),
                g.key()
            );
            let now = match layout.get(layer, control, g) {
                None if layer == Layer::NowPlaying => format!(
                    "As in Menus ({})",
                    layout.resolve(Layer::Menus, control, g).label()
                ),
                _ => layout.resolve(layer, control, g).label().into(),
            };
            row(gesture_name(control, g), &key, now).with_kind(RowKind::Chevron)
        })
        .collect();
    if layout.changed(layer).contains(&control) {
        rows.push(row(
            "Back to Reborn Default",
            &format!("btn_reset:{}:{}", layer.key(), control.key()),
            "",
        ));
    }
    let hint = format!(
        "What {} does {}.",
        control.label(),
        layer.label().to_lowercase()
    );
    (rows, Some(hint))
}

/// The commands for a button's press or hold, the current one checked, its meaning below.
fn pick_page(
    m: &AppModel,
    layout: &Layout,
    layer: Layer,
    control: Control,
    gesture: Gesture,
) -> (Vec<Item>, Option<String>) {
    let set = |c: &str| {
        format!(
            "btn_set:{}:{}:{}:{c}",
            layer.key(),
            control.key(),
            gesture.key()
        )
    };
    let own = layout.get(layer, control, gesture);
    let mut rows = vec![];
    if layer == Layer::NowPlaying {
        rows.push(row("As in Menus", &set("same"), "").with_kind(RowKind::Check(own.is_none())));
    }
    let current = layout.resolve(layer, control, gesture);
    for command in offered(layer) {
        let chosen = command == current && (layer != Layer::NowPlaying || own.is_some());
        rows.push(row(command.label(), &set(command.key()), "").with_kind(RowKind::Check(chosen)));
    }
    let first = usize::from(layer == Layer::NowPlaying);
    let hint = match m.navigation.focus.checked_sub(first) {
        None => "Does what it does in the menus.",
        Some(i) => offered(layer).get(i).map_or("", |c| c.hint()),
    };
    (rows, Some(hint.into()))
}

/// The commands a picker offers in a layer.
fn offered(layer: Layer) -> Vec<Command> {
    Command::OFFERED
        .into_iter()
        .filter(|c| layer != Layer::ScreenOff || c.in_the_dark())
        .collect()
}

/// The proof behind Save: from Home, the layout's own commands reach this page (Settings,
/// Customize, Buttons) and Back returns to Home, on Reborn's real menus.
pub(crate) fn walk(layout: &Layout) -> Result<(), String> {
    let has = |command: Command| {
        Control::ALL.into_iter().any(|c| {
            gestures(c)
                .iter()
                .any(|&g| layout.resolve(Layer::Menus, c, g) == command)
        })
    };
    for command in [Command::ScrollForward, Command::Select, Command::Back] {
        if !has(command) {
            return Err(format!("The menus need {}.", command.label()));
        }
    }
    let mut ui = Ui::default();
    let mut m = AppModel::default();
    let act = |ui: &mut Ui, m: &mut AppModel, command: Command| {
        for action in command.actions(1) {
            let _ = ui.model_action(m, action);
        }
    };
    for key in ["settings", "customize", "customize:buttons"] {
        let mut found = false;
        for _ in 0..64 {
            if ui
                .rows(&m, &[])
                .get(m.navigation.focus)
                .is_some_and(|r| r.key == key)
            {
                found = true;
                break;
            }
            act(&mut ui, &mut m, Command::ScrollForward);
        }
        if !found {
            return Err("Scrolling can't reach every row of the menus.".into());
        }
        act(&mut ui, &mut m, Command::Select);
    }
    if parse(&m.navigation.filter) != Some(ButtonPage::Main) {
        return Err("Select doesn't open the menus' pages.".into());
    }
    for _ in 0..8 {
        if m.screen == Screen::Home {
            return Ok(());
        }
        act(&mut ui, &mut m, Command::Back);
    }
    Err("Back doesn't lead home from the menus.".into())
}
