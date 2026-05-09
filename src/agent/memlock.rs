//! Memory locking helpers for agent secrets.
//!
//! Wraps `libc::mlock` / `libc::munlock` to prevent secret key
//! material from being paged to swap. On Linux, also marks locked
//! regions with `MADV_DONTDUMP` so they are excluded from core dumps.
//!
//! All functions degrade gracefully: on failure they log a warning and
//! return `false`. The agent continues to function without mlock; the
//! caller simply loses the swap-protection guarantee.

use std::ffi::c_void;

/// Lock a memory region so the OS will not page it to swap.
///
/// Returns `true` on success. On failure, logs a warning and returns
/// `false`.
pub(crate) fn mlock_slice(ptr: *const u8, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    // SAFETY: ptr and len describe a valid, allocated region owned by
    // the caller. mlock is a read-only advisory syscall that pins
    // pages in RAM; it does not modify the memory contents.
    let ret = unsafe { libc::mlock(ptr as *const c_void, len) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        warn!("mlock failed ({} bytes): {}", len, err);
        return false;
    }
    true
}

/// Unlock a previously mlocked memory region, allowing the OS to page
/// it normally again.
///
/// Returns `true` on success. On failure, logs a warning and returns
/// `false`.
pub(crate) fn munlock_slice(ptr: *const u8, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    // SAFETY: ptr and len describe the same region previously passed
    // to mlock_slice. munlock is advisory and does not modify memory
    // contents; it only allows the OS to page the region again.
    let ret = unsafe { libc::munlock(ptr as *const c_void, len) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        warn!("munlock failed ({} bytes): {}", len, err);
        return false;
    }
    true
}

/// On Linux, advise the kernel to exclude this region from core dumps.
/// No-op on other platforms (macOS has no `MADV_DONTDUMP` equivalent).
#[allow(unused_variables)]
pub(crate) fn mark_dontdump(ptr: *const u8, len: usize) {
    #[cfg(target_os = "linux")]
    {
        if len == 0 {
            return;
        }
        // SAFETY: ptr and len describe a valid, allocated region owned
        // by the caller. MADV_DONTDUMP is advisory; it excludes the
        // region from core dumps but does not modify memory contents.
        let ret = unsafe { libc::madvise(ptr as *mut c_void, len, libc::MADV_DONTDUMP) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            warn!("madvise(MADV_DONTDUMP) failed ({} bytes): {}", len, err);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn mlock_munlock_small_buffer() {
        let buf = [0u8; 64];
        let ptr = buf.as_ptr();
        let len = buf.len();

        assert!(mlock_slice(ptr, len));
        assert!(munlock_slice(ptr, len));
    }

    #[test]
    fn mlock_zero_length_is_noop() {
        let buf = [0u8; 1];
        assert!(mlock_slice(buf.as_ptr(), 0));
        assert!(munlock_slice(buf.as_ptr(), 0));
    }

    #[test]
    fn mark_dontdump_does_not_panic() {
        let buf = [0u8; 64];
        mark_dontdump(buf.as_ptr(), buf.len());
    }

    #[test]
    fn mark_dontdump_zero_length_is_noop() {
        let buf = [0u8; 1];
        mark_dontdump(buf.as_ptr(), 0);
    }
}
