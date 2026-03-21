//! Paging facade for arch-heavy pmap helpers.

use core::sync::atomic::AtomicBool;

use crate::{
    executor::MAX_CPUS,
    hal_common::{PhysAddr, VirtAddr},
    mm::{self, vm::VmPage},
};

#[inline]
pub fn alloc_asid() -> (u16, u64) {
    crate::mm::pmap::asid::alloc_asid()
}

#[inline]
pub fn global_asid_generation() -> u64 {
    crate::mm::pmap::asid::global_generation()
}

#[inline]
pub fn handle_shootdown_ipi(cpu_id: usize) {
    crate::mm::pmap::shootdown::handle_shootdown_ipi(cpu_id)
}

#[inline]
pub fn shootdown(active: &[AtomicBool; MAX_CPUS], va_start: usize, va_end: usize, asid: u16) {
    crate::mm::pmap::shootdown::pmap_shootdown(active, va_start, va_end, asid)
}

#[inline]
pub fn broadcast_flush_all() {
    crate::mm::pmap::shootdown::ipi_broadcast_flush_all()
}

#[inline]
pub fn pmap_activate(pmap: &mut crate::mm::Pmap) {
    crate::mm::pmap::pmap_activate(pmap)
}

#[inline]
pub fn pmap_deactivate(pmap: &mut crate::mm::Pmap) {
    crate::mm::pmap::pmap_deactivate(pmap)
}

#[inline]
pub fn activate_current(root_pa: usize, asid: u16) {
    #[cfg(target_arch = "riscv64")]
    {
        crate::hal::rv64::paging::activate_current(root_pa, asid);
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::hal::la64::paging::activate_current(root_pa, asid);
    }
}

#[inline]
pub fn deactivate_current() {
    #[cfg(target_arch = "riscv64")]
    {
        crate::hal::rv64::paging::deactivate_current();
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::hal::la64::paging::deactivate_current();
    }
}

#[inline]
pub fn flush_addr_asid(vaddr: usize, asid: usize) {
    #[cfg(target_arch = "riscv64")]
    {
        crate::hal::rv64::paging::flush_addr_asid(vaddr, asid);
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::hal::la64::paging::flush_addr_asid(vaddr, asid);
    }
}

pub fn create_arch_root_mappings() -> (&'static VmPage, &'static VmPage) {
    #[cfg(target_arch = "riscv64")]
    {
        crate::hal::rv64::paging::create_arch_root_mappings()
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::hal::la64::paging::create_arch_root_mappings()
    }
}

#[inline]
pub unsafe fn walk_sv39(
    root_pa: PhysAddr,
    va: VirtAddr,
    alloc: bool,
    allocator: &mut dyn FnMut(usize) -> Option<PhysAddr>,
) -> Option<*mut u64> {
    #[cfg(target_arch = "riscv64")]
    {
        return unsafe { crate::hal::rv64::paging::walk_sv39(root_pa, va, alloc, allocator) };
    }
    #[cfg(target_arch = "loongarch64")]
    {
        return unsafe { crate::hal::la64::paging::walk(root_pa, va, alloc, allocator) };
    }
}
