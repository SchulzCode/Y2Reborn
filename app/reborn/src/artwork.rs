//! Bounded collection art loader. Native decoding never runs on the UI thread.
use reborn_core::Track;
use reborn_media::{external_artwork, Cancel, Decoder};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::mpsc::{sync_channel, Receiver, SyncSender},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub source: String,
    pub id: i64,
    path: PathBuf,
    size: u64,
    mtime: i64,
    mount: Option<u64>,
}
impl Key {
    pub fn new(track: &Track, mount: Option<u64>) -> Self {
        Self {
            source: track.source_id.clone(),
            id: track.id,
            path: track.path.clone(),
            size: track.size,
            mtime: track.mtime,
            mount,
        }
    }
}
pub struct Reply {
    pub key: Key,
    pub pixels: Option<Vec<u8>>,
}
pub struct Worker {
    requests: SyncSender<(Key, Cancel)>,
    pub replies: Receiver<Reply>,
    pub selected: Option<Key>,
    cancel: Option<Cancel>,
}
impl Worker {
    pub fn spawn() -> Result<Self, std::io::Error> {
        let (requests, rx) = sync_channel::<(Key, Cancel)>(1);
        let (tx, replies) = sync_channel(1);
        std::thread::Builder::new()
            .name("collection-art".into())
            .spawn(move || {
                let mut cache: VecDeque<(Key, Option<Vec<u8>>)> = VecDeque::new();
                while let Ok((key, cancel)) = rx.recv() {
                    let pixels = if let Some(i) = cache.iter().position(|(k, _)| k == &key) {
                        let entry = cache.remove(i).expect("known cache position");
                        let pixels = entry.1.clone();
                        cache.push_back(entry);
                        pixels
                    } else {
                        let pixels = load(&key, cancel).filter(|v| v.len() == 160 * 160 * 4);
                        if cache.len() == 4 {
                            cache.pop_front();
                        }
                        if pixels.is_some() {
                            cache.push_back((key.clone(), pixels.clone()));
                        }
                        pixels
                    };
                    // One latest route result; a slow UI cannot grow a result queue.
                    if tx.send(Reply { key, pixels }).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            requests,
            replies,
            selected: None,
            cancel: None,
        })
    }
    pub fn select(&mut self, key: Option<Key>) -> bool {
        if key == self.selected {
            return false;
        }
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        if let Some(key) = key {
            let Ok(cancel) = Cancel::new() else {
                return false;
            };
            if self
                .requests
                .try_send((key.clone(), cancel.clone()))
                .is_err()
            {
                return false;
            }
            self.cancel = Some(cancel);
            self.selected = Some(key);
        } else {
            self.selected = None;
        }
        true
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}
fn load(key: &Key, cancel: Cancel) -> Option<Vec<u8>> {
    let _cpu_hint = reborn_platform::workload::CpuWorkloadLease::acquire_workload_hint(
        reborn_platform::workload::Class::ArtworkDecode,
    );
    if let Ok(mut decoder) = Decoder::open(&key.path, 44100, cancel) {
        if decoder.metadata.artwork {
            if let Ok(pixels) = decoder.artwork() {
                return Some(pixels);
            }
        }
    }
    let parent = key.path.parent()?;
    [
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "cover.webp",
        "folder.jpg",
        "folder.jpeg",
        "folder.png",
        "folder.webp",
        "album.jpg",
        "album.png",
    ]
    .iter()
    .find_map(|name| external_artwork(&parent.join(name)).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removable_mount_and_file_revision_are_part_of_art_identity() {
        let mut t = Track {
            id: 1,
            source_id: "sd".into(),
            path: "/media/sd/a.flac".into(),
            ..Default::default()
        };
        let key = Key::new(&t, Some(10));
        assert_ne!(key, Key::new(&t, Some(11)));
        t.mtime = 4;
        assert_ne!(key, Key::new(&t, Some(10)));
    }
}
