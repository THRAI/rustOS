//! User-copy facade.

pub use super::rv64::copy_user::copy_user_chunk;

#[inline]
pub unsafe fn copy_from_user(dst: *mut u8, src: *const u8, len: usize) -> usize {
    unsafe { copy_user_chunk(dst, src, len) }
}

#[inline]
pub unsafe fn copy_to_user(dst: *mut u8, src: *const u8, len: usize) -> usize {
    unsafe { copy_user_chunk(dst, src, len) }
}
