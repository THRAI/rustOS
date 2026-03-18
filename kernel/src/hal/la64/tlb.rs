pub fn flush_all() {
    unsafe {
        core::arch::asm!("invtlb 0x0, $zero, $zero");
    }
}

pub fn flush_asid(_asid: usize) {
    unsafe {
        core::arch::asm!("invtlb 0x0, $zero, $zero");
    }
}
