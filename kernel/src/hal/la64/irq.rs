const CRMD_IE: usize = 1 << 2;

#[inline(always)]
pub fn disable_and_save() -> usize {
    let saved: usize;
    unsafe {
        core::arch::asm!("csrrd {}, 0x0", out(reg) saved);
        core::arch::asm!("csrclr {}, 0x0", in(reg) CRMD_IE);
    }
    saved
}

#[inline(always)]
pub fn restore(saved: usize) {
    if saved & CRMD_IE != 0 {
        enable();
    }
}

#[inline(always)]
pub fn enable() {
    unsafe {
        core::arch::asm!("csrset {}, 0x0", in(reg) CRMD_IE);
    }
}

#[inline(always)]
pub fn disable() {
    unsafe {
        core::arch::asm!("csrclr {}, 0x0", in(reg) CRMD_IE);
    }
}

#[inline(always)]
pub fn is_enabled() -> bool {
    let crmd: usize;
    unsafe {
        core::arch::asm!("csrrd {}, 0x0", out(reg) crmd);
    }
    crmd & CRMD_IE != 0
}
