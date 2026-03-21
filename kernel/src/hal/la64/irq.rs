const CRMD_IE: usize = 1 << 2;

#[inline(always)]
pub fn disable_and_save() -> usize {
    let saved: usize;
    let new: usize;
    unsafe {
        core::arch::asm!("csrrd {}, 0x0", out(reg) saved);
        new = saved & !CRMD_IE;
        core::arch::asm!("csrwr {}, 0x0", in(reg) new);
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
    let crmd: usize;
    let new: usize;
    unsafe {
        core::arch::asm!("csrrd {}, 0x0", out(reg) crmd);
        new = crmd | CRMD_IE;
        core::arch::asm!("csrwr {}, 0x0", in(reg) new);
    }
}

#[inline(always)]
pub fn disable() {
    let crmd: usize;
    let new: usize;
    unsafe {
        core::arch::asm!("csrrd {}, 0x0", out(reg) crmd);
        new = crmd & !CRMD_IE;
        core::arch::asm!("csrwr {}, 0x0", in(reg) new);
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

pub fn self_check() -> bool {
    let before = is_enabled();
    let saved = disable_and_save();
    let disabled = !is_enabled();
    restore(saved);
    let after = is_enabled();
    disabled && before == after
}
