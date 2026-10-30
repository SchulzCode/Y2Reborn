//! Drawing a frame: the screen slide and the visible rows.
use crate::*;

impl Ui {
    pub fn draw(&self, m: &AppModel, tracks: &[Track], has_art: bool) -> Vec<Quad> {
        self.motion.begin_frame();
        glyph_cache::begin_frame();
        let frame = screens::draw(self, m, tracks, has_art);
        let frame = if self.ui_motion {
            self.slide_screens(m, frame)
        } else {
            frame
        };
        glyph_cache::end_frame(&frame);
        frame
    }

    /// A new screen slides in from the right over the last one (Back: the
    /// last one slides out to the right), 200 ms, eased out.
    pub(crate) fn slide_screens(&self, m: &AppModel, frame: Vec<Quad>) -> Vec<Quad> {
        let now = Instant::now();
        let here = (
            m.screen,
            m.navigation.filter.clone(),
            m.navigation.history.len(),
        );
        {
            let mut seen = self.seen.borrow_mut();
            if let Some((screen, filter, depth, old)) = seen.take() {
                // Typing changes the search page's text, not the page; a
                // section shown alone (See All) is a page of its own.
                let section = |f: &str| catalog::search_parts(f).1;
                let moved = screen != here.0
                    || (filter != here.1 && screen != Screen::Search)
                    || (screen == Screen::Search && section(&filter) != section(&here.1));
                // Cover Flow's cover flight is its own transition.
                if moved && !m.screen_off && self.transition.get().is_none() && !old.is_empty() {
                    *self.slide.borrow_mut() = Some(Slide {
                        started: now,
                        push: here.2 >= depth,
                        old,
                    });
                }
            }
            *seen = Some((here.0, here.1, here.2, frame.clone()));
        }
        let mut slide = self.slide.borrow_mut();
        let Some(s) = slide.as_ref() else {
            return frame;
        };
        let p = motion::ease_out(now.saturating_duration_since(s.started).as_secs_f32() / SLIDE_S);
        if p >= 1. || m.screen_off {
            *slide = None;
            return frame;
        }
        let mut old = s.old.clone();
        for q in &mut old {
            q.focus_target = false;
        }
        let mut new = frame;
        let w = theme::WIDTH;
        let mut out = Vec::with_capacity(old.len() + new.len());
        if s.push {
            motion::shift(&mut old, -w * 0.3 * p);
            motion::shift(&mut new, w * (1. - p));
            out.extend(old);
            out.extend(new);
        } else {
            motion::shift(&mut new, -w * 0.3 * (1. - p));
            motion::shift(&mut old, w * p);
            out.extend(new);
            out.extend(old);
        }
        out
    }

    /// List rows one screen shows: the skin's list, else Reborn's own.
    pub(crate) fn visible_count(&self, m: &AppModel, tracks: &[Track]) -> usize {
        self.skin
            .as_deref()
            .and_then(|skin| {
                let (key, _, _) = screens::skin_layout(skin, self, m, tracks)?;
                skin.list_rows(key)
            })
            .unwrap_or_else(|| screens::visible_rows_for(m.screen))
    }
}
