//! Libc stubs for lwext4's C code.
//!
//! lwext4 internally calls malloc/free/calloc/realloc and various libc
//! functions. We provide them here, routing allocations through Rust's
//! global allocator with a hidden header for size tracking.

use core::alloc::Layout;

const HEADER_SIZE: usize = 16; // size(8) + padding(8), maintains 16-byte alignment

#[no_mangle]
pub unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
    unsafe {
        if size == 0 {
            return core::ptr::null_mut();
        }
        let total = size + HEADER_SIZE;
        let layout = match Layout::from_size_align(total, 16) {
            Ok(l) => l,
            Err(_) => return core::ptr::null_mut(),
        };
        let ptr = alloc::alloc::alloc(layout);
        if ptr.is_null() {
            return ptr;
        }
        *(ptr as *mut usize) = size;
        ptr.add(HEADER_SIZE)
    }
}

#[no_mangle]
pub unsafe extern "C" fn free(ptr: *mut u8) {
    unsafe {
        if ptr.is_null() {
            return;
        }
        let real_ptr = ptr.sub(HEADER_SIZE);
        let size = *(real_ptr as *const usize);
        let total = size + HEADER_SIZE;
        let layout = Layout::from_size_align_unchecked(total, 16);
        alloc::alloc::dealloc(real_ptr, layout);
    }
}

#[no_mangle]
pub unsafe extern "C" fn calloc(nmemb: usize, size: usize) -> *mut u8 {
    unsafe {
        let total = match nmemb.checked_mul(size) {
            Some(t) => t,
            None => return core::ptr::null_mut(),
        };
        let ptr = malloc(total);
        if !ptr.is_null() {
            core::ptr::write_bytes(ptr, 0, total);
        }
        ptr
    }
}

#[no_mangle]
pub unsafe extern "C" fn realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
    unsafe {
        if ptr.is_null() {
            return malloc(new_size);
        }
        if new_size == 0 {
            free(ptr);
            return core::ptr::null_mut();
        }
        let old_size = *(ptr.sub(HEADER_SIZE) as *const usize);
        let new_ptr = malloc(new_size);
        if !new_ptr.is_null() {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, old_size.min(new_size));
            free(ptr);
        }
        new_ptr
    }
}

// --- Memory operations ---
// memset, memcpy, memmove are provided in assembly (hal/rv64/memops.S)
// to prevent LLVM from lowering core::ptr intrinsics back into calls
// to these functions, which causes infinite recursion.

#[no_mangle]
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    unsafe {
        for i in 0..n {
            let a = *s1.add(i);
            let b = *s2.add(i);
            if a != b {
                return a as i32 - b as i32;
            }
        }
        0
    }
}

// --- String operations ---

#[no_mangle]
pub unsafe extern "C" fn strlen(s: *const u8) -> usize {
    unsafe {
        let mut len = 0;
        while *s.add(len) != 0 {
            len += 1;
        }
        len
    }
}

#[no_mangle]
pub unsafe extern "C" fn strcmp(s1: *const u8, s2: *const u8) -> i32 {
    unsafe {
        let mut i = 0;
        loop {
            let a = *s1.add(i);
            let b = *s2.add(i);
            if a != b || a == 0 {
                return a as i32 - b as i32;
            }
            i += 1;
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn strncmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    unsafe {
        for i in 0..n {
            let a = *s1.add(i);
            let b = *s2.add(i);
            if a != b || a == 0 {
                return a as i32 - b as i32;
            }
        }
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn strcpy(dst: *mut u8, src: *const u8) -> *mut u8 {
    unsafe {
        let mut i = 0;
        loop {
            let c = *src.add(i);
            *dst.add(i) = c;
            if c == 0 {
                break;
            }
            i += 1;
        }
        dst
    }
}

#[no_mangle]
pub unsafe extern "C" fn strncpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    unsafe {
        let mut i = 0;
        while i < n {
            let c = *src.add(i);
            *dst.add(i) = c;
            if c == 0 {
                break;
            }
            i += 1;
        }
        while i < n {
            *dst.add(i) = 0;
            i += 1;
        }
        dst
    }
}

// --- printf stubs (no-ops, lwext4 uses these for debug logging) ---

#[no_mangle]
pub unsafe extern "C" fn printf(_fmt: *const u8, _: ...) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn snprintf(_buf: *mut u8, _size: usize, _fmt: *const u8, _: ...) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn puts(_s: *const u8) -> i32 {
    0
}

// --- qsort ---

#[no_mangle]
pub unsafe extern "C" fn qsort(
    base: *mut u8,
    nmemb: usize,
    size: usize,
    compar: unsafe extern "C" fn(*const u8, *const u8) -> i32,
) {
    unsafe {
        // Simple insertion sort — lwext4 only qsorts small arrays (dir entries)
        for i in 1..nmemb {
            for j in (1..=i).rev() {
                let a = base.add(j * size);
                let b = base.add((j - 1) * size);
                if compar(a, b) < 0 {
                    // swap elements byte-by-byte
                    for k in 0..size {
                        let tmp = *a.add(k);
                        *a.add(k) = *b.add(k);
                        *b.add(k) = tmp;
                    }
                } else {
                    break;
                }
            }
        }
    }
}

// --- abort ---

#[no_mangle]
pub extern "C" fn abort() -> ! {
    panic!("libc abort() called");
}

// --- newlib/libc stubs needed by lwext4 C code ---

/// newlib's _impure_ptr (struct _reent*). lwext4 references it via stdout macro.
#[no_mangle]
pub static _impure_ptr: usize = 0;

/// stdout — lwext4 debug code references this directly (e.g. fprintf(stdout, ...)).
#[no_mangle]
pub static stdout: usize = 0;

/// fflush — no-op, lwext4 calls this after printf for debug output.
#[no_mangle]
pub unsafe extern "C" fn fflush(_stream: *mut u8) -> i32 {
    0
}
