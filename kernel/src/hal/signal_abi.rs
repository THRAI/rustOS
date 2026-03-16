//! Signal ABI facade.

use crate::{
    hal::TrapFrame,
    hal_common::{Errno, PAGE_SIZE},
};

pub const USER_MAX_VA: usize = 0x0000_0040_0000_0000;
pub const SIGCODE_VA: usize = 0x0000_003F_FFFF_F000;
const RT_SIGRETURN_SYSNO: usize = 139;

#[inline]
pub fn sigcode_va() -> usize {
    SIGCODE_VA
}

#[inline]
pub fn rt_sigreturn_sysno() -> usize {
    RT_SIGRETURN_SYSNO
}

pub fn build_sigcode_page() -> [u8; PAGE_SIZE] {
    let mut page = [0u8; PAGE_SIZE];
    let li_a7: u32 = 0x08b0_0893;
    let ecall: u32 = 0x0000_0073;
    page[0..4].copy_from_slice(&li_a7.to_le_bytes());
    page[4..8].copy_from_slice(&ecall.to_le_bytes());
    page
}

pub fn setup_signal_entry(
    tf: &mut TrapFrame,
    handler: usize,
    sig: usize,
    siginfo_ptr: usize,
    ucontext_ptr: usize,
    new_sp: usize,
    restorer: Option<usize>,
) {
    tf.set_pc(handler);
    tf.set_arg(0, sig);
    tf.set_arg(1, siginfo_ptr);
    tf.set_arg(2, ucontext_ptr);
    tf.set_sp(new_sp);
    tf.set_ra(restorer.unwrap_or(sigcode_va()));
    tf.sstatus = (tf.sstatus & !(1 << 8)) | (1 << 5);
    if tf.sstatus & (3 << 13) == 0 {
        tf.sstatus |= 1 << 13;
    }
}

pub fn validate_sigreturn_frame(saved: &TrapFrame) -> Result<(), Errno> {
    if saved.pc() >= USER_MAX_VA {
        return Err(Errno::Einval);
    }
    Ok(())
}

pub fn restore_after_sigreturn(tf: &mut TrapFrame, saved: &TrapFrame) -> Result<(), Errno> {
    validate_sigreturn_frame(saved)?;
    *tf = *saved;
    tf.sstatus = (saved.sstatus & !(1 << 8)) | (1 << 5);
    if tf.sstatus & (3 << 13) == 0 {
        tf.sstatus |= 1 << 13;
    }
    Ok(())
}
