pub mod allocation_counts;
pub mod audiobookshelf;
pub mod avrcp;
pub mod bluetooth;
pub mod client;
pub mod codecs;
pub mod contract;
pub mod fm;
pub mod geo;
pub mod haptics;
pub mod input;
pub mod jack;
pub mod kernel_events;
pub mod lastfm;
mod native;
pub mod playlists;
pub mod power;
pub mod radio;
mod radiod;
pub mod scrobble;
pub mod share;
pub mod storage;
pub mod wifi;
pub mod workload;

/// How a thread is scheduled against the rest of the player.
enum ThreadClass {
    /// Yields to playback and the UI: nice 10.
    Background,
    /// Runs before everything that is not real-time: SCHED_FIFO at the lowest priority.
    Audio,
}

/// Schedule the calling thread as `class`; whether the kernel took it.
fn schedule_thread(class: ThreadClass) -> bool {
    // SAFETY: both calls act on the calling thread only, with plain values; nothing is shared.
    unsafe {
        match class {
            ThreadClass::Background => {
                libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10) == 0
            }
            ThreadClass::Audio => {
                let priority = libc::sched_param { sched_priority: 1 };
                libc::sched_setscheduler(0, libc::SCHED_FIFO, &priority) == 0
            }
        }
    }
}

/// A background helper thread (library scan workers) yields to playback and
/// the UI: nice 10 for this thread only.
pub fn lower_thread_priority() {
    schedule_thread(ThreadClass::Background);
}

/// The thread that writes audio to the output runs before the player's other work (a library
/// scan, covers, the UI), as a phone's mixer thread does: it writes a few milliseconds of audio
/// and sleeps, and when it is late the output runs dry. Threads it starts inherit this, which
/// is wanted: the Bluetooth output's transfer thread is started by its open. False where the
/// kernel refuses (not root: tests, a development host); the thread then runs as before.
pub fn audio_thread_priority() -> bool {
    schedule_thread(ThreadClass::Audio)
}

/// 64 random bits from the kernel's pool (getrandom), the clock if that fails.
pub fn random_u64() -> u64 {
    let mut value = [0u8; 8];
    // SAFETY: getrandom writes at most the buffer's length into it.
    let n = unsafe { libc::getrandom(value.as_mut_ptr().cast(), value.len(), 0) };
    if n == 8 {
        u64::from_ne_bytes(value)
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64)
    }
}
pub use native::{
    evdev_monotonic_clock, filesystem_uuid, free_bytes, install_signals, monotonic_seconds, space,
    stop_requested,
};
