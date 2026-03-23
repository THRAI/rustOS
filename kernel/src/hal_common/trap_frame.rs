//! TrapFrame definition shared by the HAL facade.
//!
//! The layout stays `#[repr(C)]` so each architecture's trap assembly can save
//! state at fixed offsets, while the helper methods below hide per-arch register
//! numbering from higher layers.
//! Total size: 37 * 8 = 296 bytes.

/// TrapFrame holds all register state saved on trap entry.
///
/// Field order: reg0..reg31 (32 GPR slots), arch_status, arch_pc, arch_cause,
/// arch_fault, kernel_tp.
/// Slot meanings depend on the active architecture's trap save layout.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// Architecture-defined register save slots.
    pub regs: [usize; 32],
    /// Architecture-private status snapshot
    pub arch_status: usize,
    /// Architecture-private program counter snapshot
    pub arch_pc: usize,
    /// Architecture-private cause bits snapshot
    pub arch_cause: usize,
    /// Architecture-private fault address / trap auxiliary snapshot
    pub arch_fault: usize,
    /// Saved kernel tp (per-CPU data pointer)
    pub kernel_tp: usize,
}

// Compile-time size check: 37 fields * 8 bytes = 296
const _: () = assert!(core::mem::size_of::<TrapFrame>() == 37 * 8);

impl TrapFrame {
    /// Create a zeroed TrapFrame.
    pub const fn zero() -> Self {
        Self {
            regs: [0; 32],
            arch_status: 0,
            arch_pc: 0,
            arch_cause: 0,
            arch_fault: 0,
            kernel_tp: 0,
        }
    }

    #[cfg(target_arch = "loongarch64")]
    const REG_SP_SLOT: usize = 3;
    #[cfg(not(target_arch = "loongarch64"))]
    const REG_SP_SLOT: usize = 2;

    const REG_RA_SLOT: usize = 1;

    #[cfg(target_arch = "loongarch64")]
    const REG_TLS_SLOT: usize = 2;
    #[cfg(not(target_arch = "loongarch64"))]
    const REG_TLS_SLOT: usize = 4;

    #[cfg(target_arch = "loongarch64")]
    const REG_ARG0_SLOT: usize = 4;
    #[cfg(not(target_arch = "loongarch64"))]
    const REG_ARG0_SLOT: usize = 10;

    #[cfg(target_arch = "loongarch64")]
    const REG_SYSCALL_NR_SLOT: usize = 11;
    #[cfg(not(target_arch = "loongarch64"))]
    const REG_SYSCALL_NR_SLOT: usize = 17;

    #[cfg(target_arch = "loongarch64")]
    const USER_STATUS_PPLV_MASK: usize = 0x3;
    #[cfg(target_arch = "loongarch64")]
    const USER_STATUS_PPLV_USER: usize = 0x3;
    #[cfg(target_arch = "loongarch64")]
    const USER_STATUS_IE: usize = 1 << 2;

    #[cfg(not(target_arch = "loongarch64"))]
    const USER_STATUS_SPP: usize = 1 << 8;
    #[cfg(not(target_arch = "loongarch64"))]
    const USER_STATUS_SPIE: usize = 1 << 5;
    #[cfg(not(target_arch = "loongarch64"))]
    const USER_STATUS_FS_MASK: usize = 0x3 << 13;
    #[cfg(not(target_arch = "loongarch64"))]
    const USER_STATUS_FS_INITIAL: usize = 1 << 13;

    /// Program counter.
    #[inline]
    pub fn pc(&self) -> usize {
        self.arch_pc
    }

    /// Set program counter.
    #[inline]
    pub fn set_pc(&mut self, val: usize) {
        self.arch_pc = val;
    }

    /// Architecture-private status snapshot.
    #[inline]
    pub fn status(&self) -> usize {
        self.arch_status
    }

    /// Set architecture-private status snapshot.
    #[inline]
    pub fn set_status(&mut self, val: usize) {
        self.arch_status = val;
    }

    /// Sanitize architecture-private status bits for a userspace return path.
    #[inline]
    pub fn normalize_user_status(status: usize) -> usize {
        #[cfg(target_arch = "loongarch64")]
        {
            (status & !Self::USER_STATUS_PPLV_MASK)
                | Self::USER_STATUS_PPLV_USER
                | Self::USER_STATUS_IE
        }
        #[cfg(not(target_arch = "loongarch64"))]
        {
            let mut status = (status & !Self::USER_STATUS_SPP) | Self::USER_STATUS_SPIE;
            if status & Self::USER_STATUS_FS_MASK == 0 {
                status |= Self::USER_STATUS_FS_INITIAL;
            }
            status
        }
    }

    /// Canonical initial status for the first return to userspace.
    #[inline]
    pub fn initial_user_status() -> usize {
        Self::normalize_user_status(0)
    }

    /// Advance the program counter to the next instruction (useful for skipping ecall).
    #[inline]
    pub fn advance_pc(&mut self) {
        self.arch_pc = self.arch_pc.wrapping_add(4);
    }

    /// Advance past the current syscall/trap instruction.
    #[inline]
    pub fn advance_syscall_pc(&mut self) {
        self.advance_pc();
    }

    /// Get syscall/function argument by ABI index (a0..a7).
    /// Panics if n > 7.
    #[inline]
    pub fn arg(&self, n: usize) -> usize {
        assert!(n < 8, "arg index out of range: {n}");
        self.regs[Self::REG_ARG0_SLOT + n]
    }

    /// Set syscall/function argument by ABI index (a0..a7).
    #[inline]
    pub fn set_arg(&mut self, n: usize, val: usize) {
        assert!(n < 8, "set_arg index out of range: {n}");
        self.regs[Self::REG_ARG0_SLOT + n] = val;
    }

    /// Set return value (a0).
    #[inline]
    pub fn set_ret_val(&mut self, val: usize) {
        self.regs[Self::REG_ARG0_SLOT] = val;
    }

    /// Set the user-visible stack pointer register.
    #[inline]
    pub fn set_sp(&mut self, val: usize) {
        self.regs[Self::REG_SP_SLOT] = val;
    }

    /// Set return address register.
    #[inline]
    pub fn set_ra(&mut self, val: usize) {
        self.regs[Self::REG_RA_SLOT] = val;
    }

    /// Syscall number register slot (policy defined by architecture ABI layer).
    #[inline]
    pub fn syscall_nr(&self) -> usize {
        self.regs[Self::REG_SYSCALL_NR_SLOT]
    }

    /// Set user TLS/thread pointer register.
    #[inline]
    pub fn set_tls(&mut self, val: usize) {
        self.regs[Self::REG_TLS_SLOT] = val;
    }

    /// Prepare a standard user entry state.
    #[inline]
    pub fn prepare_user_entry(&mut self, entry: usize, sp: usize) {
        self.arch_pc = entry;
        self.set_sp(sp);
        self.arch_status = Self::initial_user_status();
    }

    /// Current user-visible stack pointer.
    #[inline]
    pub fn sp(&self) -> usize {
        self.regs[Self::REG_SP_SLOT]
    }

    /// Current return address register value.
    #[inline]
    pub fn ra(&self) -> usize {
        self.regs[Self::REG_RA_SLOT]
    }

    /// Architecture-private trap cause register snapshot.
    #[inline]
    pub fn scause(&self) -> usize {
        self.arch_cause
    }

    /// Architecture-private cause snapshot.
    #[inline]
    pub fn cause_bits(&self) -> usize {
        self.arch_cause
    }

    /// Architecture-private fault/trap auxiliary register snapshot.
    #[inline]
    pub fn stval(&self) -> usize {
        self.arch_fault
    }

    /// Architecture-private fault address / trap auxiliary value.
    #[inline]
    pub fn fault_addr(&self) -> usize {
        self.arch_fault
    }

    //TODO: implement several setter methods for process module, like skip-fork-call, etc.
}
