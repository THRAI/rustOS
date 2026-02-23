//! User/kernel copy with page-at-a-time chunking and POSIX short-read semantics.
//!
//! This module contains the portable logic for `uiomove`. The actual byte-copy
//! function is injected as a parameter, allowing host tests to use a mock while
//! the kernel uses the real `copy_user_chunk` assembly.

use hal_common::{Errno, PAGE_SIZE};

/// Direction of the copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UioDir {
    /// Copy from user buffer to kernel buffer (read from user).
    CopyIn,
    /// Copy from kernel buffer to user buffer (write to user).
    CopyOut,
}

/// Result of a successful uiomove operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UioResult {
    /// Number of bytes successfully copied.
    pub done: usize,
}

/// EFAULT error code matching the assembly landing pad return value.
const EFAULT_RAW: usize = 14;

/// Compute the chunk size that won't cross a page boundary.
/// `addr` is the user-space address, `remaining` is bytes left to copy.
#[inline]
pub fn chunk_size(addr: usize, remaining: usize) -> usize {
    let page_remaining = PAGE_SIZE - (addr & (PAGE_SIZE - 1));
    core::cmp::min(remaining, page_remaining)
}

/// Copy `len` bytes between kernel buffer `kern` and user buffer `user`,
/// one page-chunk at a time.
///
/// `copy_fn(dst, src, len) -> 0` on success, `14` (EFAULT) on fault.
///
/// POSIX short-read semantics: if some chunks succeed before a fault,
/// returns `Ok` with `done < len`. If the first chunk faults, returns
/// `Err(EFAULT)`.
pub fn uiomove(
    kern: *mut u8,
    user: *mut u8,
    len: usize,
    dir: UioDir,
    copy_fn: unsafe fn(*mut u8, *const u8, usize) -> usize,
) -> Result<UioResult, Errno> {
    if len == 0 {
        return Ok(UioResult { done: 0 });
    }

    let mut total_copied: usize = 0;
    let mut kern_off: usize = 0;
    let mut user_off: usize = 0;
    let mut remaining = len;

    while remaining > 0 {
        let user_addr = user as usize + user_off;
        let chunk = chunk_size(user_addr, remaining);

        let ret = match dir {
            UioDir::CopyIn => unsafe {
                // user -> kernel
                copy_fn(
                    kern.add(kern_off),
                    (user as *const u8).add(user_off),
                    chunk,
                )
            },
            UioDir::CopyOut => unsafe {
                // kernel -> user
                copy_fn(
                    user.add(user_off),
                    (kern as *const u8).add(kern_off),
                    chunk,
                )
            },
        };

        if ret == EFAULT_RAW {
            if total_copied > 0 {
                // Short read/write: return what we got so far
                return Ok(UioResult { done: total_copied });
            }
            // First chunk faulted — nothing copied
            return Err(Errno::EFAULT);
        }

        total_copied += chunk;
        kern_off += chunk;
        user_off += chunk;
        remaining -= chunk;
    }

    Ok(UioResult { done: total_copied })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock copy function: always succeeds (memcpy).
    unsafe fn mock_copy_ok(dst: *mut u8, src: *const u8, len: usize) -> usize {
        core::ptr::copy_nonoverlapping(src, dst, len);
        0
    }

    /// Mock copy function: always faults.
    unsafe fn mock_copy_fault(_dst: *mut u8, _src: *const u8, _len: usize) -> usize {
        14 // EFAULT
    }

    /// Track call count via a static. Not thread-safe but tests run serially.
    static mut FAULT_AFTER_CALLS: usize = 0;
    static mut CALL_COUNT: usize = 0;

    /// Mock copy function: succeeds for first N calls, then faults.
    unsafe fn mock_copy_partial(dst: *mut u8, src: *const u8, len: usize) -> usize {
        let count = unsafe { CALL_COUNT };
        unsafe { CALL_COUNT = count + 1 };
        if count >= unsafe { FAULT_AFTER_CALLS } {
            return 14;
        }
        core::ptr::copy_nonoverlapping(src, dst, len);
        0
    }

    fn reset_partial_mock(fault_after: usize) {
        unsafe {
            FAULT_AFTER_CALLS = fault_after;
            CALL_COUNT = 0;
        }
    }

    #[test]
    fn chunk_size_within_page() {
        // Address at offset 100 within a page, 50 bytes remaining
        assert_eq!(chunk_size(100, 50), 50);
    }

    #[test]
    fn chunk_size_crosses_page() {
        // Address at offset 4000 within a page (4096 - 4000 = 96 bytes to boundary)
        // but we want 200 bytes — should clamp to 96
        assert_eq!(chunk_size(4000, 200), 96);
    }

    #[test]
    fn chunk_size_page_aligned() {
        // Page-aligned address: full page available
        assert_eq!(chunk_size(4096, 8192), 4096);
    }

    #[test]
    fn uiomove_zero_length() {
        let mut kern = [0u8; 1];
        let mut user = [0u8; 1];
        let r = uiomove(
            kern.as_mut_ptr(),
            user.as_mut_ptr(),
            0,
            UioDir::CopyIn,
            mock_copy_ok,
        )
        .unwrap();
        assert_eq!(r.done, 0);
    }

    #[test]
    fn uiomove_copy_in_small() {
        let mut kern = [0u8; 8];
        let mut user = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let r = uiomove(
            kern.as_mut_ptr(),
            user.as_mut_ptr(),
            8,
            UioDir::CopyIn,
            mock_copy_ok,
        )
        .unwrap();
        assert_eq!(r.done, 8);
        assert_eq!(kern, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn uiomove_copy_out_small() {
        let mut kern = [10u8, 20, 30, 40];
        let mut user = [0u8; 4];
        let r = uiomove(
            kern.as_mut_ptr(),
            user.as_mut_ptr(),
            4,
            UioDir::CopyOut,
            mock_copy_ok,
        )
        .unwrap();
        assert_eq!(r.done, 4);
        assert_eq!(user, [10, 20, 30, 40]);
    }

    #[test]
    fn uiomove_fault_on_first_chunk() {
        let mut kern = [0u8; 16];
        let mut user = [0u8; 16];
        let r = uiomove(
            kern.as_mut_ptr(),
            user.as_mut_ptr(),
            16,
            UioDir::CopyIn,
            mock_copy_fault,
        );
        assert_eq!(r, Err(Errno::EFAULT));
    }

    #[test]
    fn uiomove_short_read_partial_fault() {
        // Simulate: 2 page-sized chunks, fault on second
        reset_partial_mock(1); // succeed on first call, fault on second

        let mut kern = [0u8; 8192];
        // Use a page-aligned user address so each chunk is exactly PAGE_SIZE
        // We simulate this by using a buffer and noting the chunking logic
        // will split based on user pointer alignment.
        let mut user = [0xAAu8; 8192];

        let r = uiomove(
            kern.as_mut_ptr(),
            user.as_mut_ptr(),
            8192,
            UioDir::CopyIn,
            mock_copy_partial,
        )
        .unwrap();

        // First chunk succeeded (size depends on user ptr alignment),
        // second chunk faulted — short read
        assert!(r.done > 0);
        assert!(r.done < 8192);
    }

    #[test]
    fn uiomove_multi_page_full_success() {
        let mut kern = [0u8; 8192];
        let mut user = [0xBBu8; 8192];
        let r = uiomove(
            kern.as_mut_ptr(),
            user.as_mut_ptr(),
            8192,
            UioDir::CopyIn,
            mock_copy_ok,
        )
        .unwrap();
        assert_eq!(r.done, 8192);
        assert!(kern.iter().all(|&b| b == 0xBB));
    }
}
