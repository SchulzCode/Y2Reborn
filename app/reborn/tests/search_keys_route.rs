//! The library search's keys through the device's real input route (#51):
//! physical button events go through `ActionRouter` in keyboard mode, synced
//! per event from the UI as the runtime does, into `Ui::model_action`.

use reborn_core::{
    Action, AppModel, MediaSource, NormalizedInput, PhysicalControl, Screen, Source, Track,
};
use reborn_platform::input::{ActionRouter, InputEvent};
use reborn_ui::{keyboard::Key, Ui};

fn model() -> AppModel {
    let mut m = AppModel::default();
    m.library.tracks = ["Beat It", "Bad", "Thriller"]
        .iter()
        .enumerate()
        .map(|(i, title)| Track {
            id: i as i64,
            source_id: "internal".into(),
            title: (*title).into(),
            artist: "Michael Jackson".into(),
            album: "Hits".into(),
            album_artist: "Michael Jackson".into(),
            path: format!("/data/music/{i}.flac").into(),
            filename: format!("{i}.flac"),
            duration_ms: 200_000,
            online: true,
            ..Default::default()
        })
        .collect();
    m.sources = vec![Source {
        id: "internal".into(),
        kind: MediaSource::Internal,
        root: "/data/music".into(),
        online: true,
        mount: "internal".into(),
        mount_id: None,
    }];
    m
}

struct Device {
    ui: Ui,
    m: AppModel,
    router: ActionRouter,
}

impl Device {
    fn new() -> Self {
        Self {
            ui: Ui::default(),
            m: model(),
            router: ActionRouter::default(),
        }
    }

    /// One normalized event, routed as `main.rs` routes it.
    fn event(&mut self, input: NormalizedInput) -> Vec<Action> {
        self.router.set_keyboard(self.ui.keyboard.is_some());
        self.router.set_keyboard_wheel(
            self.ui
                .keyboard
                .as_ref()
                .is_some_and(reborn_ui::keyboard::Keyboard::live),
        );
        let event = InputEvent {
            device: "test".into(),
            input,
            kernel_mono_us: None,
        };
        let actions = self.router.route(&event, true);
        for &action in &actions {
            self.ui.model_action(&mut self.m, action);
        }
        actions
    }

    fn click(&mut self, control: PhysicalControl) {
        self.event(NormalizedInput::Press(control));
        self.event(NormalizedInput::Release(control));
    }

    fn hold(&mut self, control: PhysicalControl) {
        self.event(NormalizedInput::Press(control));
        self.event(NormalizedInput::LongPress(control));
        self.event(NormalizedInput::Release(control));
    }

    /// Select a row by key on the current screen.
    fn open(&mut self, key: &str) {
        let tracks = self.m.library.tracks.clone();
        let rows = self.ui.rows(&self.m, &tracks);
        self.m.navigation.focus = rows.iter().position(|r| r.key == key).unwrap();
        self.click(PhysicalControl::Select);
    }

    fn focused_row(&self) -> String {
        let rows = self.ui.rows(&self.m, &self.m.library.tracks);
        rows[self.m.navigation.focus].key.clone()
    }

    /// Move along the keys with Next (right) and Play/Pause (down) until
    /// the key has the focus, then type it.
    fn type_key(&mut self, key: Key) {
        for _ in 0..4 {
            for _ in 0..12 {
                if self.ui.keyboard.as_ref().unwrap().focused() == key {
                    self.click(PhysicalControl::Select);
                    return;
                }
                self.click(PhysicalControl::Next);
            }
            self.click(PhysicalControl::PlayPause);
        }
        panic!("{key:?} not on the keys");
    }

    fn up_to_the_top_row(&mut self) {
        while self.ui.keyboard.as_ref().unwrap().row != 0 {
            self.click(PhysicalControl::PlayPause);
        }
    }

    fn search_for_b(&mut self) {
        self.open("search");
        assert_eq!(self.m.screen, Screen::Search);
        assert!(self.ui.keyboard.is_some());
        self.type_key(Key::Char('b'));
        assert_eq!(self.ui.keyboard.as_ref().unwrap().text, "b");
        assert!(self.ui.keyboard.as_ref().unwrap().found > 0);
    }
}

#[test]
fn back_moves_between_the_keys_and_the_results() {
    let mut d = Device::new();
    d.search_for_b();
    // Back is the Up arrow: off the top row it goes into the results…
    d.up_to_the_top_row();
    d.click(PhysicalControl::Back);
    assert!(d.ui.keyboard.is_none());
    assert_eq!(d.m.screen, Screen::Search);
    // …and Back from the results returns to the search box, with the text.
    d.click(PhysicalControl::Back);
    assert_eq!(d.m.screen, Screen::Search);
    assert_eq!(d.ui.keyboard.as_ref().unwrap().text, "b");
}

#[test]
fn cancel_leaves_the_search_with_text_typed() {
    let mut d = Device::new();
    d.search_for_b();
    d.type_key(Key::Cancel);
    assert!(d.ui.keyboard.is_none());
    assert_eq!(d.m.screen, Screen::Home);
    assert_eq!(d.focused_row(), "search");
    // Also when the keys came back over the results.
    d.search_for_b();
    d.up_to_the_top_row();
    d.click(PhysicalControl::Back);
    d.click(PhysicalControl::Back);
    assert!(d.ui.keyboard.is_some());
    d.type_key(Key::Cancel);
    assert!(d.ui.keyboard.is_none());
    assert_eq!(d.m.screen, Screen::Home);
}

#[test]
fn a_held_back_leaves_the_search_while_typing() {
    // From a lower row the press moves up a row, the hold leaves.
    let mut d = Device::new();
    d.search_for_b();
    assert_ne!(d.ui.keyboard.as_ref().unwrap().row, 0);
    d.hold(PhysicalControl::Back);
    assert!(d.ui.keyboard.is_none());
    assert_eq!(d.m.screen, Screen::Home);
    assert_eq!(d.focused_row(), "search");
    // From the top row the press already folded the keys into the results;
    // the hold still leaves the search, not into the results.
    let mut d = Device::new();
    d.search_for_b();
    d.up_to_the_top_row();
    d.event(NormalizedInput::Press(PhysicalControl::Back));
    assert!(d.ui.keyboard.is_none());
    assert_eq!(d.m.screen, Screen::Search);
    d.event(NormalizedInput::LongPress(PhysicalControl::Back));
    d.event(NormalizedInput::Release(PhysicalControl::Back));
    assert!(d.ui.keyboard.is_none());
    assert_eq!(d.m.screen, Screen::Home);
    assert_eq!(d.focused_row(), "search");
    // Held Back on the results (keys folded away) is Home as everywhere.
    let mut d = Device::new();
    d.search_for_b();
    d.up_to_the_top_row();
    d.click(PhysicalControl::Back);
    d.hold(PhysicalControl::Back);
    assert_eq!(d.m.screen, Screen::Home);
}
