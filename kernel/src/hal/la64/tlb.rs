#[inline]
pub fn flush_addr(vaddr: usize) {
    unsafe {
        core::arch::asm!(
            "dbar 0",
            "invtlb 0x5, $zero, {}",
            in(reg) vaddr,
            options(nostack)
        );
    }
}

pub fn flush_all() {
    unsafe {
        core::arch::asm!("dbar 0", "invtlb 0x0, $zero, $zero", options(nostack));
    }
}

pub fn flush_asid(asid: usize) {
    let _ = asid;
    flush_all();
}
