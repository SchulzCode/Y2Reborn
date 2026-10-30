//! Skins and themes: the roots, the scan and applying one.
use crate::*;

impl Runtime {
    /// Folders skins are installed in: the shipped ones first, then Skins (or
    /// Themes, its name before Beta 7.5) on Internal Storage and on the SD
    /// card (any letter case), as seen over MTP.
    pub(crate) fn skin_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![];
        if let Ok(dev) = std::env::var("REBORN_SKINS") {
            roots.push(PathBuf::from(dev));
        }
        roots.push(PathBuf::from("/usr/share/reborn/skins"));
        for dir in self.storage_roots() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            roots.extend(
                entries
                    .flatten()
                    .filter(|e| {
                        let name = e.file_name();
                        let name = name.to_string_lossy();
                        name.eq_ignore_ascii_case("skins") || name.eq_ignore_ascii_case("themes")
                    })
                    .map(|e| e.path()),
            );
        }
        roots
    }

    pub(crate) fn scan_themes(&mut self) {
        let mut themes = skins::scan(&self.skin_roots());
        // Keep the reason a skin was refused this session.
        for t in &mut themes {
            if let Some(old) = self
                .ui
                .themes
                .iter()
                .find(|o| o.id == t.id && o.problem.is_some())
            {
                t.problem = t.problem.take().or_else(|| old.problem.clone());
            }
        }
        self.ui.themes = themes;
        self.dirty.mark_render();
    }

    pub(crate) fn upload_skin(&mut self, skin: &reborn_ui::skin::Skin) -> bool {
        let Some(renderer) = self.graphics.as_mut() else {
            return true;
        };
        match renderer.skin_atlas(Some((&skin.atlas, skin.atlas_width, skin.atlas_height))) {
            Ok(()) => true,
            Err(e) => {
                self.log.emit(
                    Level::Warn,
                    "ui",
                    "skin_upload_failed",
                    &e,
                    None,
                    json!({"skin":skin.id}),
                );
                false
            }
        }
    }

    /// Switch to the skin `id` ("" = Reborn's own look). A skin that does not
    /// load leaves the current look in place and says why.
    pub(crate) fn apply_theme(&mut self, id: &str, announce: bool) -> Result<(), String> {
        if id.is_empty() {
            self.ui.skin = None;
            if let Some(renderer) = self.graphics.as_mut() {
                let _ = renderer.skin_atlas(None);
            }
            self.dirty.mark_render();
            return Ok(());
        }
        if !self.ui.themes.iter().any(|t| t.id == id) {
            self.scan_themes();
        }
        let info = self
            .ui
            .themes
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or_else(|| "That theme is no longer installed".to_owned())?;
        let started = Instant::now();
        let loaded = skins::load(&info.dir, &info.id, &|path, w, h| {
            reborn_media::decode_image(path, w, h)
        });
        let skin = match loaded {
            Ok(skin) => skin,
            Err(e) => {
                self.log.emit(
                    Level::Warn,
                    "ui",
                    "skin_refused",
                    &e,
                    None,
                    json!({"skin":info.id,"dir":info.dir}),
                );
                if let Some(t) = self.ui.themes.iter_mut().find(|t| t.id == id) {
                    t.problem = Some(e.clone());
                }
                return Err(format!("Couldn't use {}: {e}", info.name));
            }
        };
        let skin = std::rc::Rc::new(skin);
        if !self.upload_skin(&skin) {
            return Err(format!(
                "Couldn't use {}: not enough video memory",
                info.name
            ));
        }
        self.log.emit(
            Level::Info,
            "ui",
            "skin_loaded",
            &info.name,
            None,
            json!({"skin":info.id,"dir":info.dir,"load_ms":started.elapsed().as_secs_f64()*1000.,
                "atlas":[skin.atlas_width,skin.atlas_height],"layouts":skin.layout_names(),"warnings":skin.warnings}),
        );
        if announce {
            if let Some(w) = skin.warnings.first() {
                self.ui.flash(format!("{}: {w}", info.name));
            }
        }
        self.ui.skin = Some(skin);
        self.dirty.mark_render();
        Ok(())
    }
}
