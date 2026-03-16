//! Syscall ABI facade.

use super::trap::TrapFrame;

#[inline]
pub fn nr(tf: &TrapFrame) -> usize {
    tf.syscall_nr()
}

#[inline]
pub fn args(tf: &TrapFrame) -> [usize; 6] {
    [
        tf.arg(0),
        tf.arg(1),
        tf.arg(2),
        tf.arg(3),
        tf.arg(4),
        tf.arg(5),
    ]
}

#[inline]
pub fn set_return(tf: &mut TrapFrame, ret: usize) {
    tf.set_ret_val(ret);
}

#[inline]
pub fn advance(tf: &mut TrapFrame) {
    tf.advance_syscall_pc();
}

#[inline]
pub fn setup_exec(
    tf: &mut TrapFrame,
    entry: usize,
    sp: usize,
    argc: usize,
    argv: usize,
    envp: usize,
) {
    tf.prepare_user_entry(entry, sp);
    tf.set_arg(0, argc);
    tf.set_arg(1, argv);
    tf.set_arg(2, envp);
}

#[inline]
pub fn setup_clone_child(tf: &mut TrapFrame, child_stack: Option<usize>, tls: Option<usize>) {
    if let Some(sp) = child_stack {
        tf.set_sp(sp);
    }
    if let Some(tp) = tls {
        tf.set_tls(tp);
    }
}
