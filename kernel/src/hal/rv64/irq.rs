//! IRQ control for rv64 (sstatus.SIE manipulation)

/// Enable interrupts: set sstatus.SIE bit.
#[inline(always)]
pub fn enable() {
    unsafe {
        core::arch::asm!("csrsi sstatus, 0x2");
    }
}

/// Disable interrupts: clear sstatus.SIE bit.
#[inline(always)]
pub fn disable() {
    unsafe {
        core::arch::asm!("csrci sstatus, 0x2");
    }
}

/// Disable interrupts and return the previous sstatus value.
/// Use with `restore()` for scoped IRQ disable.
#[inline(always)]
pub fn disable_and_save() -> usize {
    let saved: usize;
    unsafe {
        core::arch::asm!(
            "csrrci {}, sstatus, 0x2",
            out(reg) saved,
        );
    }
    saved
}

/// Restore the SIE bit from a previously saved sstatus value.
#[inline(always)]
pub fn restore(saved: usize) {
    if saved & 0x2 != 0 {
        unsafe {
            core::arch::asm!("csrsi sstatus, 0x2");
        }
    }
}

/// Check if interrupts are currently enabled.
#[inline(always)]
pub fn is_enabled() -> bool {
    let sstatus: usize;
    unsafe {
        core::arch::asm!("csrr {}, sstatus", out(reg) sstatus);
    }
    sstatus & 0x2 != 0
}
