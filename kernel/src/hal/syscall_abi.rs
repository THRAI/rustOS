//! Syscall ABI facade.

use super::trap::TrapFrame;

#[inline]
pub fn nr(tf: &TrapFrame) -> usize {
    #[cfg(target_arch = "riscv64")]
    {
        return tf.syscall_nr();
    }

    #[cfg(target_arch = "loongarch64")]
    {
        return super::la64::syscall_abi::nr(tf);
    }
}

#[inline]
pub fn args(tf: &TrapFrame) -> [usize; 6] {
    #[cfg(target_arch = "riscv64")]
    {
        [
            tf.arg(0),
            tf.arg(1),
            tf.arg(2),
            tf.arg(3),
            tf.arg(4),
            tf.arg(5),
        ]
    }
    #[cfg(target_arch = "loongarch64")]
    {
        super::la64::syscall_abi::args(tf)
    }
}

#[inline]
pub fn set_return(tf: &mut TrapFrame, ret: usize) {
    #[cfg(target_arch = "riscv64")]
    {
        tf.set_ret_val(ret);
    }
    #[cfg(target_arch = "loongarch64")]
    {
        super::la64::syscall_abi::set_return(tf, ret);
    }
}

#[inline]
pub fn advance(tf: &mut TrapFrame) {
    #[cfg(target_arch = "riscv64")]
    {
        tf.advance_syscall_pc();
    }
    #[cfg(target_arch = "loongarch64")]
    {
        super::la64::syscall_abi::advance(tf);
    }
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
    #[cfg(target_arch = "riscv64")]
    {
        tf.prepare_user_entry(entry, sp);
        tf.set_arg(0, argc);
        tf.set_arg(1, argv);
        tf.set_arg(2, envp);
    }
    #[cfg(target_arch = "loongarch64")]
    {
        super::la64::syscall_abi::setup_exec(tf, entry, sp, argc, argv, envp);
    }
}

#[inline]
pub fn setup_clone_child(tf: &mut TrapFrame, child_stack: Option<usize>, tls: Option<usize>) {
    #[cfg(target_arch = "riscv64")]
    {
        if let Some(sp) = child_stack {
            tf.set_sp(sp);
        }
        if let Some(tp) = tls {
            tf.set_tls(tp);
        }
    }
    #[cfg(target_arch = "loongarch64")]
    {
        super::la64::syscall_abi::setup_clone_child(tf, child_stack, tls);
    }
}
