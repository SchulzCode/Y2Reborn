//! The playlist worker: every playlist file is found, read and written on
//! this thread, never on the UI thread (an SD card can stall for seconds).
//! Each edit reads the file fresh and checks the entry it changes is still
//! there, so a change made on a computer meanwhile is never overwritten
//! blindly.

use reborn_core::playlist::{Playlist, PlaylistEdit, PlaylistEntry};
use reborn_observability::isolated;
use reborn_platform::playlists as files;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

pub enum Job {
    Refresh(Vec<PathBuf>),
    /// The playlist files a library scan met, per storage it walked whole:
    /// a refresh reads them instead of walking the storage again (a card of
    /// 120 000 songs takes a minute and a half to walk; Beta 8.39).
    Known(Vec<(PathBuf, Vec<PathBuf>)>),
    Create {
        dir: PathBuf,
        name: String,
        entries: Vec<PlaylistEntry>,
    },
    Edit {
        path: PathBuf,
        edit: PlaylistEdit,
        create: bool,
    },
    Rename {
        path: PathBuf,
        name: String,
    },
    Duplicate(PathBuf),
    Delete(PathBuf),
}

pub enum Reply {
    Listed(Vec<Playlist>),
    /// A playlist was written; `created` when it is new (the UI opens it).
    Saved {
        playlist: Playlist,
        created: bool,
    },
    Renamed {
        old: PathBuf,
        playlist: Playlist,
    },
    Deleted(PathBuf),
    Failed(String),
}

pub struct Worker {
    jobs: Sender<Job>,
    pub replies: Receiver<Reply>,
}

impl Worker {
    pub fn spawn() -> Option<Self> {
        let (jobs, queue) = channel::<Job>();
        let (replies_tx, replies) = channel();
        std::thread::Builder::new()
            .name("playlists".into())
            .spawn(move || {
                let mut known = Known::default();
                while let Ok(job) = queue.recv() {
                    let job = match job {
                        Job::Known(found) => {
                            known.learn(found);
                            continue;
                        }
                        Job::Refresh(roots) => {
                            let paths = known.paths(&roots);
                            Job::Refresh(paths)
                        }
                        job => job,
                    };
                    // Y2Linux 8.27: the SD card is read-only between writes;
                    // a change to a playlist on it runs inside a write window.
                    let window = on_card(&job)
                        && reborn_platform::client::sd_write_window(true).is_ok();
                    // A bug on one odd file costs that job, not the player.
                    let reply = isolated(|| run(job)).unwrap_or_else(Reply::Failed);
                    if window {
                        let _ = reborn_platform::client::sd_write_window(false);
                    }
                    known.follow(&reply);
                    let reply = match reply {
                        Reply::Failed(error) if error.contains("os error 30") => Reply::Failed(
                            "The SD card can't be changed right now. If it has errors, repair it in Settings > Storage."
                                .into(),
                        ),
                        reply => reply,
                    };
                    if replies_tx.send(reply).is_err() {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Self { jobs, replies })
    }
    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

/// The job writes on the SD card (the platform's mount point).
fn on_card(job: &Job) -> bool {
    let path = match job {
        Job::Refresh(_) | Job::Known(_) => return false,
        Job::Create { dir, .. } => dir,
        Job::Edit { path, .. } | Job::Rename { path, .. } => path,
        Job::Duplicate(path) | Job::Delete(path) => path,
    };
    path.starts_with("/media/sd")
}

/// Playlist files known per storage root, from the last scan or walk.
#[derive(Default)]
struct Known(HashMap<PathBuf, BTreeSet<PathBuf>>);

impl Known {
    fn learn(&mut self, found: Vec<(PathBuf, Vec<PathBuf>)>) {
        for (root, paths) in found {
            self.0.insert(root, paths.into_iter().collect());
        }
    }
    /// The playlist files under `roots`: the known ones and each storage's
    /// own Playlists folder (made on the player); a storage not yet known is
    /// walked once.
    fn paths(&mut self, roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut out = BTreeSet::new();
        for root in roots {
            let known = self.0.entry(root.clone()).or_insert_with(|| {
                files::discover(std::slice::from_ref(root))
                    .into_iter()
                    .collect()
            });
            out.extend(known.iter().cloned());
            out.extend(files::discover(&[root.join(files::FOLDER)]));
        }
        out.into_iter().take(files::MAX_PLAYLISTS).collect()
    }
    /// A playlist made, renamed or deleted here stays known.
    fn follow(&mut self, reply: &Reply) {
        let (gone, new) = match reply {
            Reply::Saved {
                playlist,
                created: true,
            } => (None, Some(&playlist.path)),
            Reply::Renamed { old, playlist } => (Some(old), Some(&playlist.path)),
            Reply::Deleted(path) => (Some(path), None),
            _ => (None, None),
        };
        for (root, paths) in &mut self.0 {
            if let Some(gone) = gone {
                paths.remove(gone);
            }
            if let Some(new) = new.filter(|p| p.starts_with(root)) {
                paths.insert(new.clone());
            }
        }
    }
}

fn run(job: Job) -> Reply {
    match job {
        // The files to read, as `Known::paths` gave them.
        Job::Refresh(paths) => Reply::Listed(
            paths
                .iter()
                .filter_map(|path| isolated(|| files::read(path)).ok()?.ok())
                .collect(),
        ),
        Job::Known(_) => Reply::Failed("known playlists are kept by the worker".into()),
        Job::Create { dir, name, entries } => (|| {
            let mut playlist = files::create(&dir, &name)?;
            if !entries.is_empty() {
                playlist.apply(&PlaylistEdit::Append(entries))?;
                save(&mut playlist)?;
            }
            Ok(Reply::Saved {
                playlist,
                created: true,
            })
        })()
        .unwrap_or_else(Reply::Failed),
        Job::Edit { path, edit, create } => (|| {
            let (mut playlist, created) = match files::read(&path) {
                Ok(playlist) => (playlist, false),
                Err(_) if create && !path.exists() => {
                    let dir = path.parent().ok_or("no folder")?;
                    (files::create(dir, &files::name_of(&path))?, true)
                }
                Err(e) => return Err(e),
            };
            playlist.apply(&edit)?;
            save(&mut playlist)?;
            Ok(Reply::Saved { playlist, created })
        })()
        .unwrap_or_else(Reply::Failed),
        Job::Rename { path, name } => (|| {
            let target = files::rename(&path, &name)?;
            Ok(Reply::Renamed {
                old: path,
                playlist: files::read(&target)?,
            })
        })()
        .unwrap_or_else(Reply::Failed),
        Job::Duplicate(path) => (|| {
            let source = files::read(&path)?;
            let dir = path.parent().ok_or("no folder")?;
            let name = copy_name(dir, &source.name);
            let mut copy = files::create(dir, &name)?;
            copy.entries = source.entries;
            copy.trailer = source.trailer;
            save(&mut copy)?;
            Ok(Reply::Saved {
                playlist: copy,
                created: true,
            })
        })()
        .unwrap_or_else(Reply::Failed),
        Job::Delete(path) => match std::fs::remove_file(&path) {
            Ok(()) => Reply::Deleted(path),
            Err(e) => Reply::Failed(format!("Couldn't delete the playlist: {e}")),
        },
    }
}

fn save(playlist: &mut Playlist) -> Result<(), String> {
    let (stamp, encoding) = files::write(playlist)?;
    playlist.stamp = stamp;
    playlist.encoding = encoding;
    Ok(())
}

/// "Road Trip Copy", then "Road Trip Copy 2", … whichever is free.
fn copy_name(dir: &Path, name: &str) -> String {
    let free = |candidate: &str| {
        ["m3u8", "m3u"]
            .iter()
            .all(|e| !dir.join(format!("{candidate}.{e}")).exists())
    };
    let base = format!("{name} Copy");
    if free(&base) {
        return base;
    }
    (2..1000)
        .map(|n| format!("{base} {n}"))
        .find(|c| free(c))
        .unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_writes_on_the_sd_card_take_a_write_window() {
        let sd = PathBuf::from("/media/sd/Playlists/Road.m3u8");
        let internal = PathBuf::from("/media/internal/music/Playlists/Road.m3u8");
        assert!(on_card(&Job::Delete(sd.clone())));
        assert!(on_card(&Job::Rename {
            path: sd.clone(),
            name: "x".into()
        }));
        assert!(on_card(&Job::Duplicate(sd)));
        assert!(!on_card(&Job::Delete(internal.clone())));
        assert!(!on_card(&Job::Create {
            dir: internal,
            name: "x".into(),
            entries: vec![]
        }));
        assert!(!on_card(&Job::Refresh(vec![PathBuf::from("/media/sd")])));
    }

    #[test]
    fn edits_create_favorites_duplicate_rename_and_delete_through_the_worker() {
        let dir = std::env::temp_dir().join(format!("reborn-plw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lists = dir.join("Playlists");
        let entry = |l: &str| PlaylistEntry {
            location: l.into(),
            info: None,
            extra: vec![],
        };
        // The heart on a player without Favorites.m3u8 makes it.
        let favorites = lists.join("Favorites.m3u8");
        let Reply::Saved { playlist, created } = run(Job::Edit {
            path: favorites.clone(),
            edit: PlaylistEdit::Append(vec![entry("../a.mp3")]),
            create: true,
        }) else {
            panic!("favorites not saved")
        };
        assert!(created);
        assert_eq!(playlist.entries.len(), 1);
        // A change made on a computer meanwhile is read before the edit.
        std::fs::write(&favorites, "#EXTM3U\n../b.mp3\n../a.mp3\n").unwrap();
        let Reply::Saved { playlist, .. } = run(Job::Edit {
            path: favorites.clone(),
            edit: PlaylistEdit::Remove {
                index: 1,
                location: "../a.mp3".into(),
            },
            create: false,
        }) else {
            panic!("edit failed")
        };
        assert_eq!(playlist.entries, vec![entry("../b.mp3")]);
        assert!(matches!(
            run(Job::Edit {
                path: favorites.clone(),
                edit: PlaylistEdit::Remove {
                    index: 0,
                    location: "../a.mp3".into()
                },
                create: false,
            }),
            Reply::Failed(_)
        ));
        // Create with songs, duplicate, rename, delete.
        let Reply::Saved { playlist, created } = run(Job::Create {
            dir: lists.clone(),
            name: "Road Trip".into(),
            entries: vec![entry("../x.flac")],
        }) else {
            panic!("create failed")
        };
        assert!(created && playlist.path == lists.join("Road Trip.m3u8"));
        let Reply::Saved { playlist: copy, .. } = run(Job::Duplicate(playlist.path.clone())) else {
            panic!("duplicate failed")
        };
        assert_eq!(copy.name, "Road Trip Copy");
        assert_eq!(copy.entries, playlist.entries);
        let Reply::Renamed {
            old,
            playlist: renamed,
        } = run(Job::Rename {
            path: copy.path.clone(),
            name: "Summer".into(),
        })
        else {
            panic!("rename failed")
        };
        assert_eq!((old, renamed.name.as_str()), (copy.path, "Summer"));
        assert!(matches!(
            run(Job::Delete(renamed.path.clone())),
            Reply::Deleted(_)
        ));
        let mut known = Known::default();
        let Reply::Listed(found) = run(Job::Refresh(known.paths(std::slice::from_ref(&dir))))
        else {
            panic!("refresh failed")
        };
        let mut names: Vec<_> = found.iter().map(|p| p.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["Favorites", "Road Trip"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_scan_s_playlists_are_read_without_walking_the_storage_again() {
        let dir = std::env::temp_dir().join(format!("reborn-plk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Music/Mixes")).unwrap();
        std::fs::create_dir_all(dir.join(files::FOLDER)).unwrap();
        let mix = dir.join("Music/Mixes/Mix.m3u");
        std::fs::write(&mix, "a.mp3\n").unwrap();
        let mut known = Known::default();
        known.learn(vec![(dir.clone(), vec![mix.clone()])]);
        // Copied without a scan since: not seen until the next one…
        let later = dir.join("Music/Later.m3u");
        std::fs::write(&later, "b.mp3\n").unwrap();
        // …except in the Playlists folder, where the player makes them.
        let made = dir.join(files::FOLDER).join("Made.m3u8");
        std::fs::write(&made, "c.mp3\n").unwrap();
        assert_eq!(
            known.paths(std::slice::from_ref(&dir)),
            [mix.clone(), made.clone()]
        );
        // A rename here stays known; a scan replaces the list.
        let renamed = dir.join("Music/Mixes/Renamed.m3u");
        known.follow(&Reply::Renamed {
            old: mix.clone(),
            playlist: Playlist {
                path: renamed.clone(),
                ..files::read(&made).unwrap()
            },
        });
        assert_eq!(
            known.paths(std::slice::from_ref(&dir)),
            [renamed, made.clone()]
        );
        known.learn(vec![(dir.clone(), vec![later.clone()])]);
        assert_eq!(known.paths(std::slice::from_ref(&dir)), [later, made]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
