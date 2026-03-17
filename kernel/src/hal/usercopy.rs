//! User-copy facade.

#[inline]
pub unsafe fn copy_user_chunk(dst: *mut u8, src: *const u8, len: usize) -> usize {
    #[cfg(target_arch = "riscv64")]
    {
        return unsafe { super::rv64::copy_user::copy_user_chunk(dst, src, len) };
    }

    #[cfg(target_arch = "loongarch64")]
    {
        return unsafe { super::la64::usercopy::copy_user_chunk(dst, src, len) };
    }
}

#[inline]
pub unsafe fn copy_from_user(dst: *mut u8, src: *const u8, len: usize) -> usize {
    unsafe { copy_user_chunk(dst, src, len) }
}

#[inline]
pub unsafe fn copy_to_user(dst: *mut u8, src: *const u8, len: usize) -> usize {
    unsafe { copy_user_chunk(dst, src, len) }
}
