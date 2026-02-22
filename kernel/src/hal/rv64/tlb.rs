//! rv64 TLB flush operations.
//!
//! All use sfence.vma with appropriate register operands.

/// Flush TLB entry for a single virtual address (all ASIDs).
#[inline]
pub fn flush_addr(vaddr: usize) {
    unsafe {
        core::arch::asm!("sfence.vma {}, zero", in(reg) vaddr);
    }
}

/// Flush all TLB entries for a given ASID.
#[inline]
pub fn flush_asid(asid: usize) {
    unsafe {
        core::arch::asm!("sfence.vma zero, {}", in(reg) asid);
    }
}

/// Flush entire TLB (all addresses, all ASIDs).
#[inline]
pub fn flush_all() {
    unsafe {
        core::arch::asm!("sfence.vma zero, zero");
    }
}
