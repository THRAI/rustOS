//! Stub pmap for host-side unit tests.
//!
//! The real pmap lives in the kernel crate and touches hardware page tables.
//! This stub provides the same API surface so that fault.rs compiles and
//! tests can verify fault logic without real page table walks.

use super::vm::vm_map::MapPerm;
use hal_common::{PhysAddr, VirtAddr};

pub struct Pmap {
    _dummy: (),
}

impl Pmap {
    pub fn dummy() -> Self {
        Pmap { _dummy: () }
    }
}

pub fn pmap_create() -> Pmap {
    Pmap::dummy()
}

/// No-op: record that a mapping was created.
pub fn pmap_enter(
    _pmap: &mut Pmap,
    _va: VirtAddr,
    _pa: PhysAddr,
    _prot: MapPerm,
    _wired: bool,
) -> Result<(), ()> {
    Ok(())
}

/// No-op: record that permissions were changed.
pub fn pmap_protect(_pmap: &mut Pmap, _va_start: VirtAddr, _va_end: VirtAddr, _prot: MapPerm) {}
