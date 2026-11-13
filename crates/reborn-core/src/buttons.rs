//! Button layouts (Customize > Buttons): which command each button, each hold and each turn of the
//! wheel gives, in three layers: the menus, Now Playing and the dark screen. Presets keep whole
//! layouts; **Reborn Default** is how the buttons always worked and cannot be changed.
//!
//! Power is not part of a layout: it is the only button that wakes a sleeping player, and holding
//! it (the Quick Menu, Power Off) is the way back from any layout. A layout is checked before it is
//! used (`Layout::check`), and the router falls back to Reborn Default where a layout is unusable.
use crate::{Action, PhysicalControl};
use serde::{Deserialize, Serialize};

/// A button or a direction of the wheel that a layout can give a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    Back,
    Next,
    PlayPause,
    Previous,
    Select,
    WheelForward,
    WheelBack,
    VolumeUp,
    VolumeDown,
}

impl Control {
    /// Clockwise around the wheel from the top, the centre, the wheel's turns, the side buttons.
    pub const ALL: [Self; 9] = [
        Self::Back,
        Self::Next,
        Self::PlayPause,
        Self::Previous,
        Self::Select,
        Self::WheelForward,
        Self::WheelBack,
        Self::VolumeUp,
        Self::VolumeDown,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Back => "Back",
            Self::Next => "Next",
            Self::PlayPause => "Play/Pause",
            Self::Previous => "Previous",
            Self::Select => "Centre",
            Self::WheelForward => "Wheel Clockwise",
            Self::WheelBack => "Wheel Anticlockwise",
            Self::VolumeUp => "Volume +",
            Self::VolumeDown => "Volume −",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Back => "back",
            Self::Next => "next",
            Self::PlayPause => "play_pause",
            Self::Previous => "previous",
            Self::Select => "select",
            Self::WheelForward => "wheel_forward",
            Self::WheelBack => "wheel_back",
            Self::VolumeUp => "volume_up",
            Self::VolumeDown => "volume_down",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == key)
    }
    /// A turn of the wheel has no hold.
    pub fn is_wheel(self) -> bool {
        matches!(self, Self::WheelForward | Self::WheelBack)
    }
    /// The physical button behind this control (the wheel's turns have none). Power has no control.
    pub fn of_button(button: PhysicalControl) -> Option<Self> {
        Some(match button {
            PhysicalControl::Back => Self::Back,
            PhysicalControl::Next => Self::Next,
            PhysicalControl::PlayPause => Self::PlayPause,
            PhysicalControl::Previous => Self::Previous,
            PhysicalControl::Select => Self::Select,
            PhysicalControl::VolumeUp => Self::VolumeUp,
            PhysicalControl::VolumeDown => Self::VolumeDown,
            PhysicalControl::Power
            | PhysicalControl::WheelClockwise
            | PhysicalControl::WheelCounterClockwise => return None,
        })
    }
}

/// How a button is used: a short press (a turn, for the wheel) or a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gesture {
    Press,
    Hold,
}

impl Gesture {
    pub fn key(self) -> &'static str {
        match self {
            Self::Press => "press",
            Self::Hold => "hold",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        [Self::Press, Self::Hold]
            .into_iter()
            .find(|g| g.key() == key)
    }
}

/// Where a layout's commands apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Every list and page.
    #[default]
    Menus,
    /// Now Playing with something playing; what it leaves open comes from the menus.
    NowPlaying,
    /// The screen is off (the player awake: music plays, or it has not gone to sleep yet).
    ScreenOff,
}

impl Layer {
    pub const ALL: [Self; 3] = [Self::Menus, Self::NowPlaying, Self::ScreenOff];
    pub fn label(self) -> &'static str {
        match self {
            Self::Menus => "In Menus",
            Self::NowPlaying => "On Now Playing",
            Self::ScreenOff => "Screen Off",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Menus => "menus",
            Self::NowPlaying => "now_playing",
            Self::ScreenOff => "screen_off",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.key() == key)
    }
}

/// What a button can do. Commands with an `Action` of their own become that action, so a layout
/// changes where things are, never how they behave: Next still skips a song, a chapter or a
/// station.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Nothing,
    Select,
    Back,
    Home,
    Options,
    ScrollForward,
    ScrollBack,
    NowPlaying,
    Queue,
    PlayPause,
    Next,
    Previous,
    SeekForward,
    SeekBack,
    Stop,
    Shuffle,
    Repeat,
    VolumeUp,
    VolumeDown,
    BrightnessUp,
    BrightnessDown,
    SleepTimer,
    ScreenOff,
    Wifi,
    Bluetooth,
    OpenMusic,
    OpenAudiobooks,
    OpenVideos,
    OpenPlaylists,
    OpenRadio,
    OpenSearch,
    OpenSettings,
    /// Not offered: the start-up escape (Back held five seconds) asks for it.
    RebornDefault,
}

impl Command {
    /// What the pickers offer, in their order.
    pub const OFFERED: [Self; 32] = [
        Self::Select,
        Self::Back,
        Self::Home,
        Self::Options,
        Self::ScrollForward,
        Self::ScrollBack,
        Self::NowPlaying,
        Self::Queue,
        Self::PlayPause,
        Self::Next,
        Self::Previous,
        Self::SeekForward,
        Self::SeekBack,
        Self::Stop,
        Self::Shuffle,
        Self::Repeat,
        Self::VolumeUp,
        Self::VolumeDown,
        Self::BrightnessUp,
        Self::BrightnessDown,
        Self::SleepTimer,
        Self::ScreenOff,
        Self::Wifi,
        Self::Bluetooth,
        Self::OpenMusic,
        Self::OpenAudiobooks,
        Self::OpenVideos,
        Self::OpenPlaylists,
        Self::OpenRadio,
        Self::OpenSearch,
        Self::OpenSettings,
        Self::Nothing,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Nothing => "Nothing",
            Self::Select => "Select",
            Self::Back => "Back",
            Self::Home => "Home",
            Self::Options => "Options",
            Self::ScrollForward => "Scroll Down",
            Self::ScrollBack => "Scroll Up",
            Self::NowPlaying => "Now Playing",
            Self::Queue => "Up Next",
            Self::PlayPause => "Play/Pause",
            Self::Next => "Next",
            Self::Previous => "Previous",
            Self::SeekForward => "Fast Forward",
            Self::SeekBack => "Rewind",
            Self::Stop => "Stop",
            Self::Shuffle => "Shuffle On/Off",
            Self::Repeat => "Repeat",
            Self::VolumeUp => "Volume Up",
            Self::VolumeDown => "Volume Down",
            Self::BrightnessUp => "Brighter",
            Self::BrightnessDown => "Dimmer",
            Self::SleepTimer => "Sleep Timer",
            Self::ScreenOff => "Screen Off",
            Self::Wifi => "Wi-Fi On/Off",
            Self::Bluetooth => "Bluetooth On/Off",
            Self::OpenMusic => "Open Music",
            Self::OpenAudiobooks => "Open Audiobooks",
            Self::OpenVideos => "Open Videos",
            Self::OpenPlaylists => "Open Playlists",
            Self::OpenRadio => "Open Radio",
            Self::OpenSearch => "Open Search",
            Self::OpenSettings => "Open Settings",
            Self::RebornDefault => "Reborn Default",
        }
    }

    /// Its name in a layout file and a row key (the serde name).
    pub fn key(self) -> &'static str {
        match self {
            Self::Nothing => "nothing",
            Self::Select => "select",
            Self::Back => "back",
            Self::Home => "home",
            Self::Options => "options",
            Self::ScrollForward => "scroll_forward",
            Self::ScrollBack => "scroll_back",
            Self::NowPlaying => "now_playing",
            Self::Queue => "queue",
            Self::PlayPause => "play_pause",
            Self::Next => "next",
            Self::Previous => "previous",
            Self::SeekForward => "seek_forward",
            Self::SeekBack => "seek_back",
            Self::Stop => "stop",
            Self::Shuffle => "shuffle",
            Self::Repeat => "repeat",
            Self::VolumeUp => "volume_up",
            Self::VolumeDown => "volume_down",
            Self::BrightnessUp => "brightness_up",
            Self::BrightnessDown => "brightness_down",
            Self::SleepTimer => "sleep_timer",
            Self::ScreenOff => "screen_off",
            Self::Wifi => "wifi",
            Self::Bluetooth => "bluetooth",
            Self::OpenMusic => "open_music",
            Self::OpenAudiobooks => "open_audiobooks",
            Self::OpenVideos => "open_videos",
            Self::OpenPlaylists => "open_playlists",
            Self::OpenRadio => "open_radio",
            Self::OpenSearch => "open_search",
            Self::OpenSettings => "open_settings",
            Self::RebornDefault => "reborn_default",
        }
    }

    /// A command a picker offers, by its key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::OFFERED.into_iter().find(|c| c.key() == key)
    }

    /// One line on what it does, under the picker.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Nothing => "The button rests: safe in a pocket.",
            Self::Select => "Picks the row in focus.",
            Self::Back => "One page back.",
            Self::Home => "Straight to Home.",
            Self::Options => "The options of the row or the song.",
            Self::ScrollForward | Self::ScrollBack => {
                concat!(
                    "One row at a time; held, it keeps going. ",
                    "On Now Playing it does what the wheel does."
                )
            }
            Self::NowPlaying => "Opens Now Playing.",
            Self::Queue => "Opens Up Next.",
            Self::PlayPause => "Plays or pauses.",
            Self::Next | Self::Previous => "The next or previous song, chapter, station or film.",
            Self::SeekForward | Self::SeekBack => "Moves through the song; held, it keeps going.",
            Self::Stop => "Stops the music.",
            Self::Shuffle => "Shuffle on or off.",
            Self::Repeat => "Off, all, this song: one step each time.",
            Self::VolumeUp | Self::VolumeDown => "Louder or softer; held, it keeps going.",
            Self::BrightnessUp | Self::BrightnessDown => {
                "The screen's brightness, a step at a time."
            }
            Self::SleepTimer => "Off, 15, 30, 45, 60 minutes: one step each time.",
            Self::ScreenOff => "Turns the screen off; the music plays on.",
            Self::Wifi | Self::Bluetooth => "Turns the radio on or off.",
            Self::OpenMusic
            | Self::OpenAudiobooks
            | Self::OpenVideos
            | Self::OpenPlaylists
            | Self::OpenRadio
            | Self::OpenSearch
            | Self::OpenSettings => "Opens that place from anywhere.",
            Self::RebornDefault => "The buttons as Reborn made them.",
        }
    }

    /// Held, it is given again and again (as the volume buttons are).
    pub fn repeats(self) -> bool {
        matches!(
            self,
            Self::ScrollForward
                | Self::ScrollBack
                | Self::SeekForward
                | Self::SeekBack
                | Self::VolumeUp
                | Self::VolumeDown
                | Self::BrightnessUp
                | Self::BrightnessDown
        )
    }

    /// Makes sense, and is safe in a pocket, with the screen off.
    pub fn in_the_dark(self) -> bool {
        matches!(
            self,
            Self::Nothing
                | Self::PlayPause
                | Self::Next
                | Self::Previous
                | Self::SeekForward
                | Self::SeekBack
                | Self::Shuffle
                | Self::Repeat
                | Self::VolumeUp
                | Self::VolumeDown
                | Self::SleepTimer
        )
    }

    /// The actions this command gives; `steps` is how far the wheel turned (1 for a button).
    pub fn actions(self, steps: u8) -> Vec<Action> {
        vec![match self {
            Self::Nothing => return vec![],
            Self::Select => Action::Select,
            Self::Back => Action::Back,
            Self::Home => Action::Home,
            Self::Options => Action::ContextMenu,
            Self::ScrollForward => Action::WheelClockwise(steps.max(1)),
            Self::ScrollBack => Action::WheelCounterClockwise(steps.max(1)),
            Self::NowPlaying => Action::ShowNowPlaying,
            Self::PlayPause => Action::PlayPause,
            Self::Next => Action::NextTrack,
            Self::Previous => Action::PreviousTrack,
            Self::SeekForward => Action::SeekForward,
            Self::SeekBack => Action::SeekBackward,
            Self::VolumeUp => Action::VolumeUp,
            Self::VolumeDown => Action::VolumeDown,
            Self::ScreenOff => Action::ScreenSleep,
            other => Action::Command(other),
        }]
    }
}

/// One command of a layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub layer: Layer,
    pub control: Control,
    pub gesture: Gesture,
    pub command: Command,
}

/// Every command of a layout. Now Playing names only what differs from the menus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Layout {
    pub bindings: Vec<Binding>,
}

/// A binding a newer Reborn wrote (an unknown command or button) is skipped, not the whole layout:
/// that button then does what Reborn Default gives it.
impl<'de> Deserialize<'de> for Layout {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            bindings: Vec<RawBinding>,
        }
        #[derive(Deserialize)]
        struct RawBinding {
            #[serde(default)]
            layer: String,
            #[serde(default)]
            control: String,
            #[serde(default)]
            gesture: String,
            #[serde(default)]
            command: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        let mut layout = Layout::default();
        for b in raw.bindings {
            let known = Layer::from_key(&b.layer)
                .zip(Control::from_key(&b.control))
                .zip(Gesture::from_key(&b.gesture))
                .zip(Command::from_key(&b.command));
            if let Some((((layer, control), gesture), command)) = known {
                layout.set(layer, control, gesture, Some(command));
            }
        }
        Ok(layout)
    }
}

impl Default for Layout {
    /// Reborn Default.
    fn default() -> Self {
        use Command as C;
        use Control::*;
        use Gesture::*;
        use Layer::*;
        let rows: [(Layer, Control, Gesture, Command); 30] = [
            (Menus, Select, Press, C::Select),
            (Menus, Select, Hold, C::Options),
            (Menus, Back, Press, C::Back),
            (Menus, Back, Hold, C::Home),
            (Menus, Previous, Press, C::Previous),
            (Menus, Previous, Hold, C::SeekBack),
            (Menus, Next, Press, C::Next),
            (Menus, Next, Hold, C::SeekForward),
            (Menus, PlayPause, Press, C::PlayPause),
            (Menus, PlayPause, Hold, C::NowPlaying),
            (Menus, WheelForward, Press, C::ScrollForward),
            (Menus, WheelBack, Press, C::ScrollBack),
            (Menus, VolumeUp, Press, C::VolumeUp),
            (Menus, VolumeDown, Press, C::VolumeDown),
            (NowPlaying, WheelForward, Press, C::VolumeUp),
            (NowPlaying, WheelBack, Press, C::VolumeDown),
            (ScreenOff, Select, Press, C::Nothing),
            (ScreenOff, Select, Hold, C::Nothing),
            (ScreenOff, Back, Press, C::Nothing),
            (ScreenOff, Back, Hold, C::Nothing),
            (ScreenOff, Previous, Press, C::Previous),
            (ScreenOff, Previous, Hold, C::SeekBack),
            (ScreenOff, Next, Press, C::Next),
            (ScreenOff, Next, Hold, C::SeekForward),
            (ScreenOff, PlayPause, Press, C::PlayPause),
            (ScreenOff, PlayPause, Hold, C::Nothing),
            (ScreenOff, WheelForward, Press, C::Nothing),
            (ScreenOff, WheelBack, Press, C::Nothing),
            (ScreenOff, VolumeUp, Press, C::VolumeUp),
            (ScreenOff, VolumeDown, Press, C::VolumeDown),
        ];
        Self {
            bindings: rows
                .into_iter()
                .map(|(layer, control, gesture, command)| Binding {
                    layer,
                    control,
                    gesture,
                    command,
                })
                .collect(),
        }
    }
}

/// Why a layout cannot be used, in words for the person who made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The menus need this command on some button or the wheel.
    Missing(Command),
    /// Nothing leaves Now Playing (Back or Home).
    NoWayOut,
    /// A command that needs the screen, given to a button for the dark.
    NotInTheDark(Control),
}

impl Problem {
    pub fn message(self) -> String {
        const NO_SELECT: &str = "Nothing selects in the menus. Give Select to a button.";
        const NO_BACK: &str = "Nothing goes back in the menus. Give Back to a button.";
        const NO_SCROLL: &str = concat!(
            "The menus can't scroll both ways. ",
            "Give Scroll Up and Scroll Down to the wheel or buttons."
        );
        const NO_WAY_OUT: &str = "Nothing leaves Now Playing. Give Back or Home to a button there.";
        match self {
            Self::Missing(Command::Select) => NO_SELECT.into(),
            Self::Missing(Command::Back) => NO_BACK.into(),
            Self::Missing(Command::ScrollForward | Command::ScrollBack) => NO_SCROLL.into(),
            Self::Missing(command) => format!("The menus need {}.", command.label()),
            Self::NoWayOut => NO_WAY_OUT.into(),
            Self::NotInTheDark(control) => format!(
                "{} has a command that needs the screen. Pick another for Screen Off.",
                control.label()
            ),
        }
    }
}

impl Layout {
    /// The command this layer names for the control and gesture (without falling back).
    pub fn get(&self, layer: Layer, control: Control, gesture: Gesture) -> Option<Command> {
        self.bindings
            .iter()
            .find(|b| b.layer == layer && b.control == control && b.gesture == gesture)
            .map(|b| b.command)
    }

    /// The command that applies: Now Playing falls back to the menus; anything unset does nothing.
    pub fn resolve(&self, layer: Layer, control: Control, gesture: Gesture) -> Command {
        self.get(layer, control, gesture)
            .or_else(|| {
                (layer == Layer::NowPlaying)
                    .then(|| self.get(Layer::Menus, control, gesture))
                    .flatten()
            })
            .unwrap_or(Command::Nothing)
    }

    /// Give the control and gesture `command`, or (`None`) leave it to the menus.
    pub fn set(
        &mut self,
        layer: Layer,
        control: Control,
        gesture: Gesture,
        command: Option<Command>,
    ) {
        self.bindings
            .retain(|b| !(b.layer == layer && b.control == control && b.gesture == gesture));
        if let Some(command) = command {
            self.bindings.push(Binding {
                layer,
                control,
                gesture,
                command,
            });
        }
    }

    /// The controls whose commands in `layer` differ from Reborn Default.
    pub fn changed(&self, layer: Layer) -> Vec<Control> {
        let default = Layout::default();
        Control::ALL
            .into_iter()
            .filter(|&c| {
                [Gesture::Press, Gesture::Hold]
                    .into_iter()
                    .any(|g| self.resolve(layer, c, g) != default.resolve(layer, c, g))
            })
            .collect()
    }

    /// Whether the layout can be used: the menus select, go back and scroll both ways, Now
    /// Playing can be left, and the dark has only commands for the dark.
    pub fn check(&self) -> Result<(), Problem> {
        let has = |layer: Layer, command: Command| {
            Control::ALL.into_iter().any(|c| {
                [Gesture::Press, Gesture::Hold]
                    .into_iter()
                    .filter(|&g| g == Gesture::Press || !c.is_wheel())
                    .any(|g| self.resolve(layer, c, g) == command)
            })
        };
        for command in [
            Command::Select,
            Command::Back,
            Command::ScrollForward,
            Command::ScrollBack,
        ] {
            if !has(Layer::Menus, command) {
                return Err(Problem::Missing(command));
            }
        }
        if !has(Layer::NowPlaying, Command::Back) && !has(Layer::NowPlaying, Command::Home) {
            return Err(Problem::NoWayOut);
        }
        for b in self.bindings.iter().filter(|b| b.layer == Layer::ScreenOff) {
            if !b.command.in_the_dark() {
                return Err(Problem::NotInTheDark(b.control));
            }
        }
        Ok(())
    }
}

/// How long a press must last to be a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HoldTime {
    Short,
    #[default]
    Normal,
    Long,
}

impl HoldTime {
    pub const ALL: [Self; 3] = [Self::Short, Self::Normal, Self::Long];
    pub fn millis(self) -> u64 {
        match self {
            Self::Short => 450,
            Self::Normal => 650,
            Self::Long => 1000,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Short => "Short",
            Self::Normal => "Normal",
            Self::Long => "Long",
        }
    }
}

/// A layout someone made, under its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub layout: Layout,
}

/// The id of Reborn Default; own presets are "own:<index>".
pub const REBORN_DEFAULT: &str = "reborn_default";
/// How many own presets a player keeps.
pub const MAX_OWN: usize = 4;

/// The presets that ship with Reborn: id, name, what it is for, the layout.
pub fn built_in() -> Vec<(&'static str, &'static str, &'static str, Layout)> {
    use Command as C;
    use Control::*;
    use Gesture::*;
    use Layer::*;
    let with = |changes: &[(Layer, Control, Gesture, Command)]| {
        let mut layout = Layout::default();
        for &(layer, control, gesture, command) in changes {
            layout.set(layer, control, gesture, Some(command));
        }
        layout
    };
    vec![
        (
            REBORN_DEFAULT,
            "Reborn Default",
            "The buttons as Reborn made them.",
            Layout::default(),
        ),
        (
            "pocket",
            "Pocket",
            "With the screen off only Play/Pause and the volume buttons work.",
            with(&[
                (ScreenOff, Previous, Press, C::Nothing),
                (ScreenOff, Previous, Hold, C::Nothing),
                (ScreenOff, Next, Press, C::Nothing),
                (ScreenOff, Next, Hold, C::Nothing),
            ]),
        ),
        (
            "wheel_seeks",
            "Wheel Seeks",
            "On Now Playing the wheel moves through the song; the side buttons set the volume.",
            with(&[
                (NowPlaying, WheelForward, Press, C::SeekForward),
                (NowPlaying, WheelBack, Press, C::SeekBack),
            ]),
        ),
        (
            "one_hand",
            "One Hand",
            concat!(
                "Screen off: the wheel sets the volume, the centre plays and pauses, ",
                "the side buttons skip."
            ),
            with(&[
                (ScreenOff, WheelForward, Press, C::VolumeUp),
                (ScreenOff, WheelBack, Press, C::VolumeDown),
                (ScreenOff, Select, Press, C::PlayPause),
                (ScreenOff, VolumeUp, Press, C::Next),
                (ScreenOff, VolumeDown, Press, C::Previous),
            ]),
        ),
    ]
}

/// The layouts on this player, the one in use and the hold time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ButtonSettings {
    /// A built-in id or "own:<index>".
    pub active: String,
    pub own: Vec<Preset>,
    pub hold: HoldTime,
    /// The two older settings (Wheel on Now Playing, Buttons While Screen Off) were taken over.
    pub migrated: bool,
}

impl Default for ButtonSettings {
    fn default() -> Self {
        Self {
            active: REBORN_DEFAULT.into(),
            own: vec![],
            hold: HoldTime::Normal,
            migrated: false,
        }
    }
}

impl ButtonSettings {
    /// The name and layout of a preset id, if it exists.
    pub fn preset(&self, id: &str) -> Option<(String, Layout)> {
        if let Some(index) = id
            .strip_prefix("own:")
            .and_then(|i| i.parse::<usize>().ok())
        {
            return self
                .own
                .get(index)
                .map(|p| (p.name.clone(), p.layout.clone()));
        }
        built_in()
            .into_iter()
            .find(|(i, ..)| *i == id)
            .map(|(_, name, _, layout)| (name.into(), layout))
    }

    /// The layout in use: Reborn Default when the active one is gone or no longer passes the check.
    pub fn layout(&self) -> Layout {
        self.preset(&self.active)
            .map(|(_, layout)| layout)
            .filter(|layout| layout.check().is_ok())
            .unwrap_or_default()
    }

    pub fn active_name(&self) -> String {
        self.preset(&self.active)
            .map_or_else(|| "Reborn Default".into(), |(name, _)| name)
    }

    pub fn is_own(&self) -> bool {
        self.active.starts_with("own:")
    }

    /// Keep `layout` under the active own preset, or as a new one when a built-in is in use (the
    /// last own preset is replaced when there are `MAX_OWN`). It becomes the active layout.
    pub fn save(&mut self, layout: Layout) {
        if let Some(index) = self
            .active
            .strip_prefix("own:")
            .and_then(|i| i.parse::<usize>().ok())
            .filter(|&i| i < self.own.len())
        {
            self.own[index].layout = layout;
            return;
        }
        let name = (1..)
            .map(|n| {
                if n == 1 {
                    "My Buttons".to_owned()
                } else {
                    format!("My Buttons {n}")
                }
            })
            .find(|name| self.own.iter().all(|p| p.name != *name))
            .unwrap_or_default();
        if self.own.len() >= MAX_OWN {
            self.own.pop();
        }
        self.own.push(Preset { name, layout });
        self.active = format!("own:{}", self.own.len() - 1);
    }

    /// Remove the active own preset; Reborn Default is used again.
    pub fn delete_active(&mut self) {
        if let Some(index) = self
            .active
            .strip_prefix("own:")
            .and_then(|i| i.parse::<usize>().ok())
            .filter(|&i| i < self.own.len())
        {
            self.own.remove(index);
        }
        self.active = REBORN_DEFAULT.into();
    }

    /// Take over Wheel on Now Playing and Buttons While Screen Off (once): a built-in preset when
    /// one matches, else a new own preset, so nothing changes under anyone's fingers.
    pub fn migrate(&mut self, wheel: NowPlayingWheel, dark: ScreenOffButtons) {
        if self.migrated {
            return;
        }
        self.migrated = true;
        let layout = Layout::from_settings(wheel, dark);
        if layout == Layout::default() {
            return;
        }
        if let Some((id, ..)) = built_in().into_iter().find(|(.., l)| *l == layout) {
            self.active = id.into();
            return;
        }
        self.active = REBORN_DEFAULT.into();
        self.save(layout);
    }
}

impl Layout {
    /// The layout the two older settings described.
    pub fn from_settings(wheel: NowPlayingWheel, dark: ScreenOffButtons) -> Self {
        use Command as C;
        use Control::*;
        use Gesture::*;
        use Layer::*;
        let mut layout = Layout::default();
        if wheel == NowPlayingWheel::Seek {
            layout.set(NowPlaying, WheelForward, Press, Some(C::SeekForward));
            layout.set(NowPlaying, WheelBack, Press, Some(C::SeekBack));
        }
        match dark {
            ScreenOffButtons::Buttons => {}
            ScreenOffButtons::Locked => {
                for control in [Previous, Next, PlayPause] {
                    layout.set(ScreenOff, control, Press, Some(C::Nothing));
                    layout.set(ScreenOff, control, Hold, Some(C::Nothing));
                }
            }
            ScreenOffButtons::WheelAndButtons => {
                layout.set(ScreenOff, Select, Press, Some(C::PlayPause));
                layout.set(ScreenOff, WheelForward, Press, Some(C::VolumeUp));
                layout.set(ScreenOff, WheelBack, Press, Some(C::VolumeDown));
            }
        }
        layout
    }
}

/// What the input router needs from the screen to turn a button into commands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ButtonContext {
    pub layout: Layout,
    /// Menus or Now Playing (the dark is the router's own knowledge).
    pub layer: Layer,
    /// A page that always answers as Reborn Default: the button pages themselves, the Quick
    /// Menu, the on-screen keyboard, a pairing code.
    pub protected: bool,
    /// The on-screen keyboard is open, and whether its page has a list for the wheel.
    pub keyboard: bool,
    pub keyboard_wheel: bool,
    /// An internet or FM station plays: a seeking wheel sets the volume instead.
    pub station: bool,
}

/// What the wheel does on Now Playing: Customize > Now Playing Wheel. The
/// volume buttons always change the volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NowPlayingWheel {
    #[default]
    Volume,
    /// It moves through the song as holding Previous/Next does.
    Seek,
}
impl NowPlayingWheel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Volume => "Volume",
            Self::Seek => "Seek",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Volume => Self::Seek,
            Self::Seek => Self::Volume,
        }
    }
}
/// What the wheel, its centre and its four buttons do while the screen is
/// off. Power, the volume keys and headphone controls always work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScreenOffButtons {
    /// Nothing: safe in a pocket.
    Locked,
    /// Play/Pause, Previous and Next (held: seek); the wheel and centre rest.
    #[default]
    Buttons,
    /// Also the wheel (volume) and the centre (play/pause), like Now Playing.
    WheelAndButtons,
}
impl ScreenOffButtons {
    pub fn label(self) -> &'static str {
        match self {
            Self::Locked => "Locked",
            Self::Buttons => "Buttons",
            Self::WheelAndButtons => "Wheel + Buttons",
        }
    }
    /// The next choice, as one press on its row steps through them.
    pub fn next(self) -> Self {
        match self {
            Self::Locked => Self::Buttons,
            Self::Buttons => Self::WheelAndButtons,
            Self::WheelAndButtons => Self::Locked,
        }
    }
}

#[cfg(test)]
#[path = "buttons_tests.rs"]
mod tests;
