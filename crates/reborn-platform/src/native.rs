use std::{
    ffi::{c_char, c_void, CStr, CString},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn signal_handler(_: i32) {
    STOP.store(true, Ordering::Relaxed)
}
pub fn install_signals() {
    // SAFETY: signal handler performs only a lock-free atomic store and has static lifetime.
    unsafe {
        libc::signal(libc::SIGTERM, signal_handler as libc::sighandler_t);
        libc::signal(libc::SIGINT, signal_handler as libc::sighandler_t);
    }
}
pub fn stop_requested() -> bool {
    STOP.load(Ordering::Relaxed)
}
pub fn monotonic_seconds() -> Option<f64> {
    let mut value = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: clock_gettime writes a valid timespec on success; only then read.
    unsafe {
        if libc::clock_gettime(libc::CLOCK_MONOTONIC, value.as_mut_ptr()) != 0 {
            return None;
        }
        let value = value.assume_init();
        Some(value.tv_sec as f64 + value.tv_nsec as f64 / 1e9)
    }
}
pub fn free_bytes(path: &Path) -> Result<u64, String> {
    use std::os::unix::ffi::OsStrExt;
    let s = CString::new(path.as_os_str().as_bytes()).map_err(|_| "NUL path")?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: valid NUL-terminated path and writable statvfs; result is read only on success.
    unsafe {
        if libc::statvfs(s.as_ptr(), stat.as_mut_ptr()) != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let v = stat.assume_init();
        #[allow(clippy::unnecessary_cast)] // statvfs field widths differ on ARM32 and host.
        let bytes = (v.f_bavail as u64) * (v.f_frsize as u64);
        Ok(bytes)
    }
}
#[link(name = "blkid")]
unsafe extern "C" {
    fn blkid_new_probe_from_filename(p: *const c_char) -> *mut c_void;
    fn blkid_do_safeprobe(p: *mut c_void) -> i32;
    fn blkid_probe_lookup_value(
        p: *mut c_void,
        name: *const c_char,
        data: *mut *const c_char,
        len: *mut usize,
    ) -> i32;
    fn blkid_free_probe(p: *mut c_void);
}
pub fn filesystem_uuid(device: &str) -> Option<String> {
    if !device.starts_with("/dev/") {
        return None;
    }
    let name = CString::new(device).ok()?;
    // SAFETY: probe has one local owner; lookup bytes are copied before probe destruction; no device data is modified.
    unsafe {
        let p = blkid_new_probe_from_filename(name.as_ptr());
        if p.is_null() {
            return None;
        }
        let mut value = std::ptr::null();
        let mut len = 0;
        let ok = blkid_do_safeprobe(p) == 0
            && blkid_probe_lookup_value(p, c"UUID".as_ptr(), &mut value, &mut len) == 0;
        let result = if ok && !value.is_null() {
            Some(CStr::from_ptr(value).to_string_lossy().into_owned())
        } else {
            None
        };
        blkid_free_probe(p);
        result
    }
}
