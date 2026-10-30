//! Moving between screens: go, back, home and the modals.
use crate::*;

impl Ui {
    pub(crate) fn go(m: &mut AppModel, screen: Screen, filter: impl Into<String>) {
        let filter = filter.into();
        if m.screen != screen || m.navigation.filter != filter {
            if m.navigation.history.len() >= 64 {
                m.navigation.history.remove(0);
                m.navigation.stack.remove(0);
            }
            m.navigation.stack.push(m.screen);
            m.navigation.history.push(NavigationFrame {
                screen: m.screen,
                focus: m.navigation.focus,
                scroll: m.navigation.scroll,
                filter: m.navigation.filter.clone(),
            });
        }
        m.screen = screen;
        m.navigation.focus = 0;
        m.navigation.scroll = 0;
        m.navigation.filter = filter;
        Self::close_modal(m);
    }

    pub(crate) fn close_modal(m: &mut AppModel) {
        m.navigation.modal = None;
        m.navigation.modal_focus = 0;
        m.navigation.context_target = None;
        m.navigation.context_key = None;
    }

    pub(crate) fn open_modal(m: &mut AppModel, modal: Modal) {
        m.navigation.modal = Some(modal);
        m.navigation.modal_focus = 0;
    }

    pub(crate) fn back(m: &mut AppModel) {
        if m.navigation.modal.is_some() {
            Self::close_modal(m);
            return;
        }
        if m.screen == Screen::TextEntry {
            return;
        }
        if let Some(previous) = m.navigation.history.pop() {
            m.navigation.stack.pop();
            m.screen = previous.screen;
            m.navigation.focus = previous.focus;
            m.navigation.scroll = previous.scroll;
            m.navigation.filter = previous.filter;
            return;
        }
        if let Some(previous) = m.navigation.stack.pop() {
            m.screen = previous;
            m.navigation.focus = 0;
            m.navigation.scroll = 0;
            m.navigation.filter.clear();
        } else if m.screen != Screen::Home {
            Self::home(m);
        }
    }

    /// A Home Menu switch changes Home's rows while Home waits in the
    /// history: Back returns to the place that was focused, not its index.
    pub(crate) fn keep_home_focus(m: &mut AppModel, tracks: &[Track], effect: &Effect) {
        let mut after = m.settings.clone();
        match effect {
            Effect::SetHomePin(key, on) => after.pin_home(key, *on),
            Effect::SetHomeShown(key, on) => after.show_on_home(key, *on),
            Effect::SetAudiobooksOnHome(on) => after.audiobooks_on_home = *on,
            _ => return,
        }
        let Some(at) = m
            .navigation
            .history
            .iter()
            .rposition(|f| f.screen == Screen::Home)
        else {
            return;
        };
        let before = pages::home_rows(&m.settings, tracks);
        let after = pages::home_rows(&after, tracks);
        let frame = &mut m.navigation.history[at];
        let key = before.get(frame.focus).map(|r| r.key.as_str());
        frame.focus = key
            .and_then(|k| after.iter().position(|r| r.key == k))
            .unwrap_or(frame.focus.min(after.len() - 1));
    }

    pub(crate) fn home(m: &mut AppModel) {
        m.navigation.stack.clear();
        m.navigation.history.clear();
        m.screen = Screen::Home;
        m.navigation.focus = 0;
        m.navigation.scroll = 0;
        m.navigation.filter.clear();
        Self::close_modal(m);
    }
}
