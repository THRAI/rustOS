#![cfg_attr(not(test), no_std)]

extern crate alloc;

/// Real macros injected by the kernel or dummy implementations for mm testing
#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {};
}

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
