#![cfg_attr(not(test), no_std)]

extern crate alloc;

/// No-op kprintln for test builds (the real macro lives in the kernel crate).
#[cfg(test)]
#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {};
}

/// No-op klog for test builds (the real macro lives in the kernel crate).
#[cfg(test)]
#[macro_export]
macro_rules! klog {
    ($($arg:tt)*) => {};
}

pub mod allocator;
pub mod pmap;
pub mod uio;
pub mod vm;

#[cfg(test)]
mod pmap_tests;
