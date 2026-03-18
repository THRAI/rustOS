use alloc::sync::Arc;

use crate::{hal::TrapFrame, proc::Task};

#[repr(C)]
pub struct TrapContext {
    pub trap_frame_ptr: usize,
    pub kernel_sp: usize,
    pub kernel_tp: usize,
    pub scratch0: usize,
    pub scratch1: usize,
}

pub const TC_SIZE: usize = core::mem::size_of::<TrapContext>();

extern "C" {
    fn __trap_return(tf_ptr: usize, tc_ptr: usize);
    fn __kernel_trap();
}

pub fn trap_return(task: &Arc<Task>) {
    let tf_ptr = {
        let tf = task.trap_frame.lock();
        &*tf as *const TrapFrame as usize
    };

    let kernel_tp: usize;
    unsafe {
        core::arch::asm!("move {}, $tp", out(reg) kernel_tp);
    }

    let tc_ptr = task.kernel_sp - TC_SIZE;
    let tc = tc_ptr as *mut TrapContext;
    unsafe {
        (*tc).trap_frame_ptr = tf_ptr;
        (*tc).kernel_sp = tc_ptr;
        (*tc).kernel_tp = kernel_tp;
        (*tc).scratch0 = 0;
        (*tc).scratch1 = 0;

        __trap_return(tf_ptr, tc_ptr);

        core::arch::asm!("csrwr {}, 0xc", in(reg) __kernel_trap as *const () as usize);
    }
}
