//! Generic page table walk parameterized by `LEVELS`.
//!
//! Sv39: LEVELS=3, VPN indices extracted via `vpn_index`.
//! The walk is identity-mapped: PA is used directly as a pointer.
//! A `phys_to_virt()` indirection will be added for higher-half kernel.

pub use crate::hal::rv64::paging::walk::{vpn_index, walk};
