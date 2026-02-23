//! Safe Rust wrapper for copy_user_chunk assembly.
//!
//! copy_user_chunk is a byte-at-a-time copy with BSD-style pcb_onfault
//! exception fixup. On a page fault during the copy, the trap handler
//! redirects to the landing pad which returns EFAULT (14).

use crate::executor::per_cpu::PerCpu;
use core::arch::global_asm;

/// Byte offset of `pcb_onfault` within PerCpu, computed at compile time.
/// The assembly uses this to store/clear the landing pad address via tp.
const OFFSET_ONFAULT: usize = core::mem::offset_of!(PerCpu, pcb_onfault);

/// SUM bit in sstatus (bit 18) — permits S-mode access to U-mode pages.
const SSTATUS_SUM: usize = 1 << 18;

// Emit the copy_user_chunk assembly with compile-time constants.
global_asm!(
    ".section .text",
    ".globl copy_user_chunk",
    ".align 4",
    // extern "C" fn copy_user_chunk(dst: *mut u8, src: *const u8, len: usize) -> usize
    // a0 = dst, a1 = src, a2 = len
    // Returns 0 on success, 14 (EFAULT) on fault.
    "copy_user_chunk:",
    // Prologue: set pcb_onfault to landing pad address
    "    la      t0, .Lcopy_fault",
    "    sd      t0, {onfault}(tp)",
    // Set sstatus.SUM=1 to allow S-mode access to U-mode pages
    "    li      t0, {sum}",
    "    csrs    sstatus, t0",
    // Copy loop (byte-at-a-time)
    "    beqz    a2, .Lcopy_done",
    ".Lcopy_loop:",
    "    lb      t0, 0(a1)",
    "    sb      t0, 0(a0)",
    "    addi    a0, a0, 1",
    "    addi    a1, a1, 1",
    "    addi    a2, a2, -1",
    "    bnez    a2, .Lcopy_loop",
    // Epilogue: clear SUM, clear pcb_onfault, return 0
    ".Lcopy_done:",
    "    li      t0, {sum}",
    "    csrc    sstatus, t0",
    "    sd      zero, {onfault}(tp)",
    "    li      a0, 0",
    "    ret",
    // Landing pad: fault occurred during copy
    ".Lcopy_fault:",
    "    li      t0, {sum}",
    "    csrc    sstatus, t0",
    "    sd      zero, {onfault}(tp)",
    "    li      a0, 14",   // EFAULT
    "    ret",
    onfault = const OFFSET_ONFAULT,
    sum = const SSTATUS_SUM,
);

extern "C" {
    /// Copy `len` bytes from `src` to `dst`. Returns 0 on success, 14 (EFAULT)
    /// if a page fault occurs during the copy (via pcb_onfault landing pad).
    ///
    /// SAFETY: Caller must ensure at least one of src/dst is a valid kernel pointer.
    /// The other may be a user pointer (possibly invalid — that's the whole point).
    /// Must be called with tp pointing to a valid PerCpu struct.
    pub fn copy_user_chunk(dst: *mut u8, src: *const u8, len: usize) -> usize;
}
