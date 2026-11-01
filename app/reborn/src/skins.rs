//! Skin packages on disk: finding them and reading their files for the pure
//! compiler in `reborn_ui::skin`. Every file a skin names must resolve (after
//! symbolic links) to a regular file inside its own folder.
use reborn_ui::skin::{self, Files, Skin, SkinInfo, MANIFEST, MANIFEST_MAX, MAX_SKINS};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// Decodes a picture file: `(path, max_w, max_h) -> (width, height, rgba)`.
pub type Decode<'a> = &'a dyn Fn(&Path, u32, u32) -> Result<(u32, u32, Vec<u8>), String>;

fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|_| "is missing".to_owned())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("is not a file".into());
    }
    if meta.len() > limit {
        return Err(format!("is larger than {} KB", limit / 1024));
    }
    let mut out = Vec::with_capacity(meta.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|e| e.to_string())?;
    if out.len() as u64 > limit {
        return Err(format!("is larger than {} KB", limit / 1024));
    }
    Ok(out)
}

fn manifest(dir: &Path) -> Result<String, String> {
    let bytes =
        bounded(&dir.join(MANIFEST), MANIFEST_MAX).map_err(|e| format!("{MANIFEST} {e}"))?;
    String::from_utf8(bytes).map_err(|_| format!("{MANIFEST} is not UTF-8 text"))
}

/// Skins found in `roots`, in name order. A folder name seen in an earlier
/// root hides the same name later (the shipped skins come first).
pub fn scan(roots: &[PathBuf]) -> Vec<SkinInfo> {
    let mut out: Vec<SkinInfo> = vec![];
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        dirs.sort();
        for dir in dirs {
            let Some(id) = dir.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
                continue;
            };
            if !skin::valid_id(&id) || out.iter().any(|s| s.id == id) || out.len() >= MAX_SKINS {
                continue;
            }
            if !dir.join(MANIFEST).is_file() {
                continue;
            }
            let text = manifest(&dir);
            out.push(skin::describe(
                &id,
                dir,
                text.as_deref().map_err(Clone::clone),
            ));
        }
    }
    out.sort_by_key(|s| s.name.to_lowercase());
    out
}

/// A skin folder; `decode` turns a picture file into RGBA.
pub struct Folder<'a> {
    root: PathBuf,
    decode: Decode<'a>,
}

impl Folder<'_> {
    fn resolve(&self, name: &str) -> Result<PathBuf, String> {
        if !skin::relative_name(name) {
            return Err("must be a path inside the skin folder".into());
        }
        let real = self
            .root
            .join(name)
            .canonicalize()
            .map_err(|_| "is missing".to_owned())?;
        if !real.starts_with(&self.root) || !real.is_file() {
            return Err("must be a file inside the skin folder".into());
        }
        Ok(real)
    }
}

impl Files for Folder<'_> {
    fn read(&self, name: &str, limit: u64) -> Result<Vec<u8>, String> {
        bounded(&self.resolve(name)?, limit)
    }
    fn picture(&self, name: &str, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let path = self.resolve(name)?;
        bounded(&path, 16 * 1024 * 1024)?;
        (self.decode)(&path, max_w, max_h)
    }
}

/// Read and compile the skin in `dir` (folder name `id`).
pub fn load(dir: &Path, id: &str, decode: Decode) -> Result<Skin, String> {
    let root = dir
        .canonicalize()
        .map_err(|e| format!("the skin folder cannot be read ({e})"))?;
    let text = manifest(&root)?;
    skin::compile(id, &text, &Folder { root, decode })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn temp(name: &str) -> Temp {
        let dir = std::env::temp_dir().join(format!("reborn-skins-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }
    /// Test pictures are text files holding "WxH".
    fn decode(path: &Path, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let (w, h) = text.trim().split_once('x').ok_or("not a test picture")?;
        let (w, h): (u32, u32) = (w.parse().unwrap(), h.parse().unwrap());
        if w > max_w || h > max_h {
            return Err("too large".into());
        }
        Ok((w, h, [255, 0, 0, 255].repeat((w * h) as usize)))
    }
    fn write(dir: &Path, name: &str, format: u32, image: &str) {
        fs::create_dir_all(dir.join("images")).unwrap();
        fs::write(dir.join("images/bar.png"), "8x30").unwrap();
        fs::write(
            dir.join(MANIFEST),
            serde_json::json!({"format": format, "name": name, "author": "Tester",
                "palette": {"bg": "#FFFFFF", "text": "#000000"},
                "images": {"bar": image},
                "screens": {"toast": [{"type": "image", "w": 10, "h": 10, "image": "bar"}]}})
            .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn scan_lists_skins_by_name_with_problems_and_the_first_root_wins() {
        let (a, b) = (temp("scan-a"), temp("scan-b"));
        write(&a.0.join("classic"), "Test", 1, "images/bar.png");
        write(&b.0.join("classic"), "Other", 1, "images/bar.png");
        write(&b.0.join("Neon"), "Neon Nights", 9, "images/bar.png");
        fs::create_dir_all(b.0.join("not-a-skin")).unwrap();
        fs::create_dir_all(b.0.join(".hidden")).unwrap();
        fs::write(b.0.join(".hidden").join(MANIFEST), "{}").unwrap();
        let found = scan(&[a.0.clone(), b.0.clone(), PathBuf::from("/nonexistent")]);
        let ids: Vec<_> = found.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["Neon", "classic"]);
        assert!(found[1].dir.starts_with(&a.0));
        assert_eq!(found[1].author, "Tester");
        assert!(found[0].problem.as_deref().unwrap().contains("format 9"));
    }

    #[test]
    fn every_shipped_skin_loads_cleanly_with_clock_keyboard_and_live_radio() {
        // Pictures by their PNG header size; the layouts are what is checked.
        let png = |path: &Path, _: u32, _: u32| -> Result<(u32, u32, Vec<u8>), String> {
            let b = fs::read(path).map_err(|e| e.to_string())?;
            let dim = |at: usize| u32::from_be_bytes(b[at..at + 4].try_into().unwrap());
            let (w, h) = (dim(16), dim(20));
            Ok((w, h, [0, 0, 0, 255].repeat((w * h) as usize)))
        };
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skins");
        let found = scan(&[root]);
        assert_eq!(found.len(), 17);
        for info in found {
            let skin = load(&info.dir, &info.id, &png).unwrap();
            assert!(skin.warnings.is_empty(), "{}: {:?}", info.id, skin.warnings);
            let text = manifest(&info.dir).unwrap();
            assert!(text.contains("{clock}"), "{} has no clock", info.id);
            // Today's screens: its own keyboard header, and a station is live.
            assert!(
                skin.layout_names().contains(&"keyboard"),
                "{} keyboard",
                info.id
            );
            assert!(
                text.contains("live") || !text.contains("{queue_index}"),
                "{} shows a queue position for a station",
                info.id
            );
        }
    }

    #[test]
    fn files_stay_inside_the_skin_folder() {
        let t = temp("paths");
        let dir = t.0.join("s");
        write(&dir, "S", 1, "images/bar.png");
        assert!(load(&dir, "s", &decode).is_ok());
        for (file, needle) in [
            ("../outside.png", "must be a path inside"),
            ("/etc/hostname", "must be a path inside"),
            ("images/missing.png", "is missing"),
        ] {
            write(&dir, "S", 1, file);
            let error = load(&dir, "s", &decode).unwrap_err();
            assert!(error.contains(needle), "{file}: {error}");
        }
        fs::write(t.0.join("outside.png"), "8x8").unwrap();
        write(&dir, "S", 1, "images/link.png");
        std::os::unix::fs::symlink(t.0.join("outside.png"), dir.join("images/link.png")).unwrap();
        let error = load(&dir, "s", &decode).unwrap_err();
        assert!(error.contains("inside the skin folder"), "{error}");
        // An oversized manifest is refused before it is parsed.
        fs::write(dir.join(MANIFEST), vec![b' '; MANIFEST_MAX as usize + 1]).unwrap();
        assert!(load(&dir, "s", &decode).unwrap_err().contains("larger"));
    }
}
