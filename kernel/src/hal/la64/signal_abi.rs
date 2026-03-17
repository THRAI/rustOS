use crate::hal_common::{trap_frame::TrapFrame, Errno, PAGE_SIZE};

pub const USER_MAX_VA: usize = usize::MAX >> 1;
pub const SIGCODE_VA: usize = USER_MAX_VA - PAGE_SIZE + 1;
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
    [0u8; PAGE_SIZE]
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
    Ok(())
}
