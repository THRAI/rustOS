//! Signal ABI facade.

use crate::{
    hal::TrapFrame,
    hal_common::{Errno, PAGE_SIZE},
};

#[cfg(target_arch = "riscv64")]
pub const USER_MAX_VA: usize = 0x0000_0040_0000_0000;
#[cfg(target_arch = "riscv64")]
pub const SIGCODE_VA: usize = 0x0000_003F_FFFF_F000;
#[cfg(target_arch = "riscv64")]
const RT_SIGRETURN_SYSNO: usize = 139;

#[inline]
pub fn sigcode_va() -> usize {
    #[cfg(target_arch = "riscv64")]
    {
        return SIGCODE_VA;
    }
    #[cfg(target_arch = "loongarch64")]
    {
        return super::la64::signal_abi::sigcode_va();
    }
}

#[inline]
pub fn rt_sigreturn_sysno() -> usize {
    #[cfg(target_arch = "riscv64")]
    {
        return RT_SIGRETURN_SYSNO;
    }
    #[cfg(target_arch = "loongarch64")]
    {
        return super::la64::signal_abi::rt_sigreturn_sysno();
    }
}

pub fn build_sigcode_page() -> [u8; PAGE_SIZE] {
    #[cfg(target_arch = "riscv64")]
    {
        let mut page = [0u8; PAGE_SIZE];
        let li_a7: u32 = 0x08b0_0893;
        let ecall: u32 = 0x0000_0073;
        page[0..4].copy_from_slice(&li_a7.to_le_bytes());
        page[4..8].copy_from_slice(&ecall.to_le_bytes());
        page
    }
    #[cfg(target_arch = "loongarch64")]
    {
        super::la64::signal_abi::build_sigcode_page()
    }
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
    #[cfg(target_arch = "riscv64")]
    {
        tf.set_pc(handler);
        tf.set_arg(0, sig);
        tf.set_arg(1, siginfo_ptr);
        tf.set_arg(2, ucontext_ptr);
        tf.set_sp(new_sp);
        tf.set_ra(restorer.unwrap_or(sigcode_va()));
        let mut status = tf.status();
        status = (status & !(1 << 8)) | (1 << 5);
        if status & (3 << 13) == 0 {
            status |= 1 << 13;
        }
        tf.set_status(status);
    }
    #[cfg(target_arch = "loongarch64")]
    {
        super::la64::signal_abi::setup_signal_entry(
            tf,
            handler,
            sig,
            siginfo_ptr,
            ucontext_ptr,
            new_sp,
            restorer,
        );
    }
}

pub fn validate_sigreturn_frame(saved: &TrapFrame) -> Result<(), Errno> {
    #[cfg(target_arch = "riscv64")]
    {
        if saved.pc() >= USER_MAX_VA {
            return Err(Errno::Einval);
        }
        Ok(())
    }
    #[cfg(target_arch = "loongarch64")]
    {
        super::la64::signal_abi::validate_sigreturn_frame(saved)
    }
}

pub fn restore_after_sigreturn(tf: &mut TrapFrame, saved: &TrapFrame) -> Result<(), Errno> {
    #[cfg(target_arch = "riscv64")]
    {
        validate_sigreturn_frame(saved)?;
        *tf = *saved;
        let mut status = saved.status();
        status = (status & !(1 << 8)) | (1 << 5);
        if status & (3 << 13) == 0 {
            status |= 1 << 13;
        }
        tf.set_status(status);
        Ok(())
    }
    #[cfg(target_arch = "loongarch64")]
    {
        super::la64::signal_abi::restore_after_sigreturn(tf, saved)
    }
}
