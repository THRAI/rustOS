//! User/kernel copy with page-at-a-time chunking and POSIX short-read semantics.
//!
//! `uiomove` copies data between a kernel buffer and a user buffer, one page-chunk
//! at a time. Each chunk never crosses a page boundary, so a fault on one page
//! doesn't prevent reporting bytes already copied from previous pages.

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
#[inline]
pub fn chunk_size(addr: usize, remaining: usize) -> usize {
    let page_remaining = PAGE_SIZE - (addr & (PAGE_SIZE - 1));
    core::cmp::min(remaining, page_remaining)
}

/// Copy `len` bytes between kernel buffer `kern` and user buffer `user`.
///
/// Uses the real `copy_user_chunk` assembly (with pcb_onfault fixup).
/// Returns `Ok(UioResult { done })` with the number of bytes copied, or
/// `Err(EFAULT)` if the first chunk faults (nothing was copied).
///
/// POSIX short-read semantics: if some chunks succeed before a fault,
/// returns `Ok` with `done < len`.
pub fn uiomove(
    kern: *mut u8,
    user: *mut u8,
    len: usize,
    dir: UioDir,
) -> Result<UioResult, Errno> {
    use crate::hal::rv64::copy_user::copy_user_chunk;

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
                copy_user_chunk(
                    kern.add(kern_off),
                    (user as *const u8).add(user_off),
                    chunk,
                )
            },
            UioDir::CopyOut => unsafe {
                copy_user_chunk(
                    user.add(user_off),
                    (kern as *const u8).add(kern_off),
                    chunk,
                )
            },
        };

        if ret == EFAULT_RAW {
            if total_copied > 0 {
                return Ok(UioResult { done: total_copied });
            }
            return Err(Errno::EFAULT);
        }

        total_copied += chunk;
        kern_off += chunk;
        user_off += chunk;
        remaining -= chunk;
    }

    Ok(UioResult { done: total_copied })
}
