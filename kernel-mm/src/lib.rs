#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod allocator;
pub mod uio;
pub mod vm;

#[cfg(test)]
mod pmap_tests;
