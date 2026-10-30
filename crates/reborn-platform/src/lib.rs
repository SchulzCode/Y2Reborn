pub mod allocation_counts;
pub mod avrcp;
pub mod bluetooth;
pub mod client;
pub mod codecs;
pub mod contract;
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
pub mod scrobble;
pub mod spotify;
pub mod storage;
pub mod wifi;
pub mod workload;

/// A background helper thread (library scan workers) yields to playback and
/// the UI: nice 10 for this thread only.
pub fn lower_thread_priority() {
    // SAFETY: setpriority on this thread's own id; nothing is shared.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10);
    }
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
