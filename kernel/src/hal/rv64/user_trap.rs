//! User-mode trap entry/exit: trap_return and __user_trap integration.
//!
//! The setjmp/longjmp model:
//!   run_tasks() calls trap_return(tf_ptr, tc_ptr)
//!   trap_return saves callee-saved regs, loads user state, sret to user
//!   user runs... ecall/fault/IRQ... __user_trap fires
//!   __user_trap saves user regs to TrapFrame, restores callee-saved regs
//!   __user_trap "returns" to the call site of trap_return() in run_tasks()
//!
//! TrapContext sits at the top of the per-task kernel stack.
//! sscratch points to it while in user mode.

use alloc::sync::Arc;

use crate::{hal_common::TrapFrame, proc::Task};

/// Placed at the top of the per-task kernel stack.
/// sscratch points here when CPU is in user mode.
/// __user_trap reads these to find TrapFrame and kernel state.
/// Includes scratch slots so __user_trap can save t0/t1 before clobbering.
#[repr(C)]
pub struct TrapContext {
    /// Pointer to the task's TrapFrame.
    pub trap_frame_ptr: usize, // offset 0
    /// Kernel sp to restore (callee-saved save area).
    pub kernel_sp: usize, // offset 8
    /// Kernel tp (PerCpu pointer) to restore.
    pub kernel_tp: usize, // offset 16
    /// Scratch slot for saving user t0 in __user_trap.
    pub scratch0: usize, // offset 24
    /// Scratch slot for saving user t1 in __user_trap.
    pub scratch1: usize, // offset 32
}

pub const TC_SIZE: usize = core::mem::size_of::<TrapContext>();

extern "C" {
    /// Assembly: save callee-saved, load user regs from TrapFrame, sret.
    /// Returns when __user_trap fires and restores callee-saved.
    /// a0 = TrapFrame pointer, a1 = TrapContext pointer
    fn __trap_return(tf_ptr: usize, tc_ptr: usize);
}

/// Prepare TrapContext and call the assembly trap_return.
///
/// This "returns" twice: once to userspace (sret), once back here
/// when the user traps and __user_trap restores callee-saved regs.
pub fn trap_return(task: &Arc<Task>) {
    // Get raw pointer to TrapFrame inside the Mutex.
    // SAFETY: This task is only polled by one CPU at a time.
    let tf_ptr = {
        let tf = task.trap_frame.lock();
        &*tf as *const TrapFrame as usize
    };

    // Kernel tp
    let kernel_tp: usize;
    unsafe {
        core::arch::asm!("mv {}, tp", out(reg) kernel_tp);
    }

    // Place TrapContext at top of kernel stack.
    let kstack_top = task.kernel_sp;
    let tc_ptr = kstack_top - TC_SIZE;
    let tc = tc_ptr as *mut TrapContext;
    unsafe {
        (*tc).trap_frame_ptr = tf_ptr;
        (*tc).kernel_sp = tc_ptr;
        (*tc).kernel_tp = kernel_tp;
    }

    unsafe {
        __trap_return(tf_ptr, tc_ptr);
    }

    // Execution resumes here after __user_trap restores callee-saved regs.
    // User registers are saved in task.trap_frame by __user_trap.

    // Restore stvec to __kernel_trap for kernel-mode traps.
    extern "C" {
        fn __kernel_trap();
    }
    unsafe {
        core::arch::asm!(
            "csrw stvec, {}",
            in(reg) __kernel_trap as *const () as usize,
        );
    }
}
