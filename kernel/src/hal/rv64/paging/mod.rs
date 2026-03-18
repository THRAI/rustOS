pub mod asid;
pub mod pte;
pub mod shootdown;
pub mod walk;

use crate::{
    hal_common::{PhysAddr, VirtAddr},
    mm::{self, vm::VmPage},
};

#[inline]
pub fn activate_current(root_pa: usize, asid: u16) {
    let satp: usize = (8usize << 60) | ((asid as usize) << 44) | (root_pa >> 12);
    unsafe {
        core::arch::asm!(
            "csrw satp, {}",
            "sfence.vma zero, zero",
            in(reg) satp,
        );
    }
}

#[inline]
pub fn deactivate_current() {
    unsafe {
        core::arch::asm!("csrw satp, zero", "sfence.vma zero, zero");
    }
}

#[inline]
pub fn flush_addr_asid(vaddr: usize, asid: usize) {
    unsafe {
        core::arch::asm!("sfence.vma {}, {}", in(reg) vaddr, in(reg) asid);
    }
}

pub fn create_arch_root_mappings() -> (&'static mut VmPage, &'static mut VmPage) {
    let frame = mm::alloc_pte_l2_sync().expect("failed to allocate page table directory");
    mm::pmap_zero_page(frame.phys());

    let root_ptr = frame.phys().as_usize() as *mut u64;
    let kernel_giga_pte = pte::encode_pte(
        0x8000_0000,
        pte::PteFlags::V
            | pte::PteFlags::R
            | pte::PteFlags::W
            | pte::PteFlags::X
            | pte::PteFlags::A
            | pte::PteFlags::D
            | pte::PteFlags::G,
    );
    unsafe {
        root_ptr.add(2).write(kernel_giga_pte);
    }

    let l1_frame = mm::alloc_pte_l1_sync().expect("pmap_create: out of memory for MMIO L1 PT");
    mm::pmap_zero_page(l1_frame.phys());
    let l1_ptr = l1_frame.phys().as_usize() as *mut u64;

    let root0_pte = pte::encode_pte(l1_frame.phys().as_usize(), pte::PteFlags::V);
    unsafe {
        root_ptr.add(0).write(root0_pte);
    }

    let mmio_mega_pte = pte::encode_pte(
        0x1000_0000,
        pte::PteFlags::V
            | pte::PteFlags::R
            | pte::PteFlags::W
            | pte::PteFlags::A
            | pte::PteFlags::D
            | pte::PteFlags::G,
    );
    let plic_flags = pte::PteFlags::V
        | pte::PteFlags::R
        | pte::PteFlags::W
        | pte::PteFlags::A
        | pte::PteFlags::D
        | pte::PteFlags::G;
    unsafe {
        l1_ptr.add(128).write(mmio_mega_pte);
        l1_ptr
            .add(96)
            .write(pte::encode_pte(0x0C00_0000, plic_flags));
        l1_ptr
            .add(97)
            .write(pte::encode_pte(0x0C20_0000, plic_flags));
    }

    (frame, l1_frame)
}

#[inline]
pub unsafe fn walk_sv39(
    root_pa: PhysAddr,
    va: VirtAddr,
    alloc: bool,
    allocator: &mut dyn FnMut(usize) -> Option<PhysAddr>,
) -> Option<*mut u64> {
    unsafe { walk::walk::<3>(root_pa, va, alloc, allocator) }
}
