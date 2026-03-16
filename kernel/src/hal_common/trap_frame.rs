//! Machine-independent TrapFrame definition.
//!
//! Layout is `#[repr(C)]` so assembly code can access fields at known offsets.
//! 32 GPRs (x0-x31) + sstatus + sepc + scause + stval + kernel_tp = 37 fields.
//! Total size: 37 * 8 = 296 bytes.

/// TrapFrame holds all register state saved on trap entry.
///
/// Field order: x0..x31 (32 GPRs), sstatus, sepc, scause, stval, kernel_tp.
/// x0 is always zero but included for consistent indexing (offset = reg_index * 8).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// x0-x31: General purpose registers
    pub x: [usize; 32],
    /// Supervisor status register
    pub sstatus: usize,
    /// Supervisor exception program counter
    pub sepc: usize,
    /// Supervisor cause register
    pub scause: usize,
    /// Supervisor trap value
    pub stval: usize,
    /// Saved kernel tp (per-CPU data pointer)
    pub kernel_tp: usize,
}

// Compile-time size check: 37 fields * 8 bytes = 296
const _: () = assert!(core::mem::size_of::<TrapFrame>() == 37 * 8);

impl TrapFrame {
    /// Create a zeroed TrapFrame.
    pub const fn zero() -> Self {
        Self {
            x: [0; 32],
            sstatus: 0,
            sepc: 0,
            scause: 0,
            stval: 0,
            kernel_tp: 0,
        }
    }

    /// Program counter (sepc).
    #[inline]
    pub fn pc(&self) -> usize {
        self.sepc
    }

    /// Set program counter (sepc).
    #[inline]
    pub fn set_pc(&mut self, val: usize) {
        self.sepc = val;
    }

    /// Advance the program counter to the next instruction (useful for skipping ecall).
    #[inline]
    pub fn advance_pc(&mut self) {
        self.sepc = self.sepc.wrapping_add(4);
    }

    /// Advance past the current syscall/trap instruction.
    #[inline]
    pub fn advance_syscall_pc(&mut self) {
        self.advance_pc();
    }

    /// Get syscall/function argument by index (a0=x10 .. a7=x17).
    /// Panics if n > 7.
    #[inline]
    pub fn arg(&self, n: usize) -> usize {
        assert!(n < 8, "arg index out of range: {n}");
        self.x[10 + n]
    }

    /// Set syscall/function argument by index (a0=x10 .. a7=x17).
    #[inline]
    pub fn set_arg(&mut self, n: usize, val: usize) {
        assert!(n < 8, "set_arg index out of range: {n}");
        self.x[10 + n] = val;
    }

    /// Set return value (a0 = x10).
    #[inline]
    pub fn set_ret_val(&mut self, val: usize) {
        self.x[10] = val;
    }

    /// Set stack pointer (x2).
    #[inline]
    pub fn set_sp(&mut self, val: usize) {
        self.x[2] = val;
    }

    /// Set return address (x1).
    #[inline]
    pub fn set_ra(&mut self, val: usize) {
        self.x[1] = val;
    }

    /// Syscall number register (a7 = x17 on rv64 Linux ABI).
    #[inline]
    pub fn syscall_nr(&self) -> usize {
        self.x[17]
    }

    /// Set user TLS/thread pointer (x4).
    #[inline]
    pub fn set_tls(&mut self, val: usize) {
        self.x[4] = val;
    }

    /// Prepare a standard user entry state.
    #[inline]
    pub fn prepare_user_entry(&mut self, entry: usize, sp: usize) {
        self.sepc = entry;
        self.set_sp(sp);
        self.sstatus = (1 << 5) | (1 << 13);
    }

    /// Stack pointer (x2).
    #[inline]
    pub fn sp(&self) -> usize {
        self.x[2]
    }

    /// Return address (x1).
    #[inline]
    pub fn ra(&self) -> usize {
        self.x[1]
    }

    /// Supervisor cause register.
    #[inline]
    pub fn scause(&self) -> usize {
        self.scause
    }

    /// Supervisor trap value.
    #[inline]
    pub fn stval(&self) -> usize {
        self.stval
    }

    //TODO: implement several setter methods for process module, like skip-fork-call, etc.
}
