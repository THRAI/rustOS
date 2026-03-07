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

pub mod mm {
    pub use crate::allocator;
    pub use crate::pmap;
    pub use crate::uio;
    pub use crate::vm;
}

#[cfg(all(test, not(target_os = "none")))]
mod pmap_tests;
