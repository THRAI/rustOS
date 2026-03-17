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
        let satp: usize = (8usize << 60) | ((asid as usize) << 44) | (root_pa >> 12);
        unsafe {
            core::arch::asm!(
                "csrw satp, {}",
                "sfence.vma zero, zero",
                in(reg) satp,
            );
        }
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
        unsafe {
            core::arch::asm!("csrw satp, zero", "sfence.vma zero, zero");
        }
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
        unsafe {
            core::arch::asm!("sfence.vma {}, {}", in(reg) vaddr, in(reg) asid);
        }
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::hal::la64::paging::flush_addr_asid(vaddr, asid);
    }
}

pub fn create_arch_root_mappings() -> (&'static mut VmPage, &'static mut VmPage) {
    #[cfg(target_arch = "riscv64")]
    {
        let frame = mm::alloc_pte_l2_sync().expect("failed to allocate page table directory");
        mm::pmap_zero_page(frame.phys());

        let root_ptr = frame.phys().as_usize() as *mut u64;
        let kernel_giga_pte = crate::hal::rv64::paging::pte::encode_pte(
            0x8000_0000,
            crate::hal::rv64::paging::pte::PteFlags::V
                | crate::hal::rv64::paging::pte::PteFlags::R
                | crate::hal::rv64::paging::pte::PteFlags::W
                | crate::hal::rv64::paging::pte::PteFlags::X
                | crate::hal::rv64::paging::pte::PteFlags::A
                | crate::hal::rv64::paging::pte::PteFlags::D
                | crate::hal::rv64::paging::pte::PteFlags::G,
        );
        unsafe {
            root_ptr.add(2).write(kernel_giga_pte);
        }

        let l1_frame = mm::alloc_pte_l1_sync().expect("pmap_create: out of memory for MMIO L1 PT");
        mm::pmap_zero_page(l1_frame.phys());
        let l1_ptr = l1_frame.phys().as_usize() as *mut u64;

        let root0_pte = crate::hal::rv64::paging::pte::encode_pte(
            l1_frame.phys().as_usize(),
            crate::hal::rv64::paging::pte::PteFlags::V,
        );
        unsafe {
            root_ptr.add(0).write(root0_pte);
        }

        let mmio_mega_pte = crate::hal::rv64::paging::pte::encode_pte(
            0x1000_0000,
            crate::hal::rv64::paging::pte::PteFlags::V
                | crate::hal::rv64::paging::pte::PteFlags::R
                | crate::hal::rv64::paging::pte::PteFlags::W
                | crate::hal::rv64::paging::pte::PteFlags::A
                | crate::hal::rv64::paging::pte::PteFlags::D
                | crate::hal::rv64::paging::pte::PteFlags::G,
        );
        let plic_flags = crate::hal::rv64::paging::pte::PteFlags::V
            | crate::hal::rv64::paging::pte::PteFlags::R
            | crate::hal::rv64::paging::pte::PteFlags::W
            | crate::hal::rv64::paging::pte::PteFlags::A
            | crate::hal::rv64::paging::pte::PteFlags::D
            | crate::hal::rv64::paging::pte::PteFlags::G;
        unsafe {
            l1_ptr.add(128).write(mmio_mega_pte);
            l1_ptr
                .add(96)
                .write(crate::hal::rv64::paging::pte::encode_pte(
                    0x0C00_0000,
                    plic_flags,
                ));
            l1_ptr
                .add(97)
                .write(crate::hal::rv64::paging::pte::encode_pte(
                    0x0C20_0000,
                    plic_flags,
                ));
        }

        (frame, l1_frame)
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
    unsafe { crate::hal::rv64::paging::walk::walk::<3>(root_pa, va, alloc, allocator) }
}
