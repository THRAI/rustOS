pub mod asid;
pub mod pte;
pub mod shootdown;
pub mod walk;

use crate::{
    hal_common::{PhysAddr, VirtAddr},
    mm::{self, vm::VmPage},
};

const CSR_ASID: usize = 0x18;
const CSR_PGDL: usize = 0x19;
const CSR_PGDH: usize = 0x1a;
const CSR_CRMD: usize = 0x0;

const CRMD_PG: usize = 1 << 4;
const CRMD_DA: usize = 1 << 3;
const ASID_MASK: usize = 0x3ff;

pub fn activate_current(root_pa: usize, asid: u16) {
    let root = root_pa & !0xfff;
    let asid_bits = (asid as usize) & ASID_MASK;
    unsafe {
        core::arch::asm!("csrwr {}, {}", in(reg) root, const CSR_PGDL);
        core::arch::asm!("csrwr {}, {}", in(reg) root, const CSR_PGDH);
        core::arch::asm!("csrwr {}, {}", in(reg) asid_bits, const CSR_ASID);

        let mut crmd: usize;
        core::arch::asm!("csrrd {}, {}", out(reg) crmd, const CSR_CRMD);
        crmd |= CRMD_PG;
        crmd &= !CRMD_DA;
        core::arch::asm!("csrwr {}, {}", in(reg) crmd, const CSR_CRMD);
    }
}

pub fn deactivate_current() {
    unsafe {
        let mut crmd: usize;
        core::arch::asm!("csrrd {}, {}", out(reg) crmd, const CSR_CRMD);
        crmd &= !CRMD_PG;
        crmd |= CRMD_DA;
        core::arch::asm!("csrwr {}, {}", in(reg) crmd, const CSR_CRMD);
    }
}

pub fn debug_status() -> (bool, bool, usize) {
    let crmd: usize;
    let asid: usize;
    unsafe {
        core::arch::asm!("csrrd {}, {}", out(reg) crmd, const CSR_CRMD);
        core::arch::asm!("csrrd {}, {}", out(reg) asid, const CSR_ASID);
    }
    (crmd & CRMD_PG != 0, crmd & CRMD_DA != 0, asid & ASID_MASK)
}

pub fn flush_addr_asid(_vaddr: usize, _asid: usize) {
    unsafe {
        core::arch::asm!("invtlb 0x0, $zero, $zero");
    }
}

pub fn create_arch_root_mappings() -> (&'static VmPage, &'static VmPage) {
    let root = mm::alloc_pte_l2_sync().expect("failed to allocate la64 root page table");
    mm::pmap_zero_page(root.phys());

    let l1 = mm::alloc_pte_l1_sync().expect("failed to allocate la64 paging directory");
    mm::pmap_zero_page(l1.phys());

    let root_ptr = root.phys().as_usize() as *mut u64;
    let l1_entry = pte::encode_pte(
        l1.phys().as_usize(),
        pte::PteFlags::V | pte::PteFlags::PRESENT,
    );
    unsafe {
        root_ptr.write_volatile(l1_entry);
    }

    (root, l1)
}

#[inline]
pub unsafe fn walk(
    root_pa: PhysAddr,
    va: VirtAddr,
    alloc: bool,
    allocator: &mut dyn FnMut(usize) -> Option<PhysAddr>,
) -> Option<*mut u64> {
    unsafe { walk::walk(root_pa, va, alloc, allocator) }
}
