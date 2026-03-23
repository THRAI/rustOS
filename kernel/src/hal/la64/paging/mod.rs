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
const CSR_PWCL: usize = 0x1c;
const CSR_PWCH: usize = 0x1d;
const CSR_STLBPS: usize = 0x1e;
const CSR_CRMD: usize = 0x0;
const CSR_TLBREHI: usize = 0x8e;

const CRMD_PG: usize = 1 << 4;
const CRMD_DA: usize = 1 << 3;
const ASID_MASK: usize = 0x3ff;

const PWCL_PTBASE_SHIFT: usize = 0;
const PWCL_PTWIDTH_SHIFT: usize = 5;
const PWCL_DIR1_BASE_SHIFT: usize = 10;
const PWCL_DIR1_WIDTH_SHIFT: usize = 15;
const PWCL_DIR2_BASE_SHIFT: usize = 20;
const PWCL_DIR2_WIDTH_SHIFT: usize = 25;
const PWCL_PTE_WIDTH_SHIFT: usize = 30;

const PWCH_DIR3_BASE_SHIFT: usize = 0;
const PWCH_DIR3_WIDTH_SHIFT: usize = 6;
const PWCH_DIR4_BASE_SHIFT: usize = 12;
const PWCH_DIR4_WIDTH_SHIFT: usize = 18;

#[inline]
pub fn init_hw() {
    let pwcl = (12usize << PWCL_PTBASE_SHIFT)
        | (9usize << PWCL_PTWIDTH_SHIFT)
        | (21usize << PWCL_DIR1_BASE_SHIFT)
        | (9usize << PWCL_DIR1_WIDTH_SHIFT)
        | (30usize << PWCL_DIR2_BASE_SHIFT)
        | (9usize << PWCL_DIR2_WIDTH_SHIFT)
        | (0usize << PWCL_PTE_WIDTH_SHIFT);
    let pwch = (39usize << PWCH_DIR3_BASE_SHIFT)
        | (9usize << PWCH_DIR3_WIDTH_SHIFT)
        | (0usize << PWCH_DIR4_BASE_SHIFT)
        | (0usize << PWCH_DIR4_WIDTH_SHIFT);

    unsafe {
        core::arch::asm!("csrwr {}, {}", in(reg) pwcl, const CSR_PWCL);
        core::arch::asm!("csrwr {}, {}", in(reg) pwch, const CSR_PWCH);
        core::arch::asm!("csrwr {}, {}", in(reg) 12usize, const CSR_STLBPS);
        core::arch::asm!("csrwr {}, {}", in(reg) 12usize, const CSR_TLBREHI);
        core::arch::asm!("invtlb 0x0, $zero, $zero");
    }
}

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
        // Keep bring-up deterministic until LA64 ASID-specific shootdown semantics
        // are fully validated: drop any stale translations after a page-table switch.
        core::arch::asm!("invtlb 0x0, $zero, $zero");
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
    crate::hal::la64::tlb::flush_addr(_vaddr);
}

pub fn create_arch_root_mappings() -> (&'static VmPage, &'static VmPage) {
    let root = mm::alloc_pte_l2_sync().expect("failed to allocate la64 root page table");
    mm::pmap_zero_page(root.phys());

    let l1 = mm::alloc_pte_l1_sync().expect("failed to allocate la64 paging placeholder");
    mm::pmap_zero_page(l1.phys());

    // On LA64 the PGD CSR points at the highest-level directory page itself.
    // Keep the root empty here and let the generic walker allocate real child
    // directories on demand. We still return a second frame to satisfy the
    // shared `(root, l1)` facade contract during bring-up.

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
