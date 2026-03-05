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

/// Module alias so that hardlinked source files using `crate::mm::*` paths
/// (the kernel crate's module hierarchy) also resolve correctly in this crate.
pub mod mm {
    pub use crate::allocator;
    pub use crate::pmap;
    pub use crate::vm;
}

#[cfg(test)]
mod pmap_tests;
