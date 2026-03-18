const EFAULT: usize = 14;
const USER_MAX_VA: usize = usize::MAX >> 1;

#[inline]
fn is_user_range(ptr: usize, len: usize) -> bool {
    ptr < USER_MAX_VA && ptr.checked_add(len).is_some_and(|end| end <= USER_MAX_VA)
}

pub unsafe fn copy_user_chunk(dst: *mut u8, src: *const u8, len: usize) -> usize {
    if len == 0 {
        return 0;
    }

    let dst_addr = dst as usize;
    let src_addr = src as usize;
    let src_user = is_user_range(src_addr, len);
    let dst_user = is_user_range(dst_addr, len);

    if !src_user && !dst_user {
        return EFAULT;
    }

    unsafe {
        core::ptr::copy_nonoverlapping(src, dst, len);
    }

    0
}
