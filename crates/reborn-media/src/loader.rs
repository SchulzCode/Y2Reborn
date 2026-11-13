//! Functions of the loaded FFmpeg membrane (`libreborn_media.so`), by name.
use std::ffi::c_void;

/// The function `name` (NUL-terminated) of the membrane behind `handle`.
///
/// # Safety
/// `handle` is a live `dlopen` handle and `T` is the function pointer type of that symbol.
pub(crate) unsafe fn symbol<T: Copy>(
    handle: *mut c_void,
    name: &'static [u8],
) -> Result<T, String> {
    let ptr = libc::dlsym(handle, name.as_ptr().cast());
    if ptr.is_null() {
        return Err(format!(
            "FFmpeg media membrane symbol {} is missing",
            String::from_utf8_lossy(&name[..name.len().saturating_sub(1)])
        ));
    }
    // POSIX specifies dlsym results as usable function addresses. A
    // transmute_copy is required because Rust keeps data and function
    // pointers as distinct types even on this target.
    Ok(std::mem::transmute_copy(&ptr))
}
