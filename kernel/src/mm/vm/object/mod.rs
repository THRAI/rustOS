//! VmObject: BSD-style VM object with shadow chains, pager I/O, page cache.

pub mod page_daemon;
mod pager;
mod vm_object;

pub use pager::{AnonPager, Pager, VnodePager};
pub use vm_object::{VObjIndex, VmObject};
