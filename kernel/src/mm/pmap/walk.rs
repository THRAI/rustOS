//! Generic page table walk parameterized by `LEVELS`.
//!
//! Sv39: LEVELS=3, VPN indices extracted via `vpn_index`.
//! The walk is identity-mapped: PA is used directly as a pointer.
//! A `phys_to_virt()` indirection will be added for higher-half kernel.

#[cfg(target_arch = "loongarch64")]
pub use crate::hal::la64::paging::walk::{vpn_index, walk};
#[cfg(target_arch = "riscv64")]
pub use crate::hal::rv64::paging::walk::{vpn_index, walk};
