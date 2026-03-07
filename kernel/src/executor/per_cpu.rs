//! Per-CPU data structure.
//!
//! Each CPU has a PerCpu holding its run queue, timer wheel, and identity.
//! Accessed via the tp register (hot path) or global array (cross-CPU).

use crate::hal_common::{IrqSafeSpinLock, RunQueue, TimerWheel};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

extern crate alloc;
use alloc::boxed::Box;

/// Maximum number of CPUs supported.
pub const MAX_CPUS: usize = 8;

/// Per-CPU data: run queue, timer wheel, hart/cpu identity, preemption flag, fixup pointer.
pub struct PerCpu {
    pub run_queue: RunQueue<async_task::Runnable>,
    pub timer_wheel: IrqSafeSpinLock<TimerWheel>,
    pub hartid: usize,
    pub cpu_id: usize,
    pub needs_reschedule: AtomicBool,
    pub pcb_onfault: AtomicUsize,
}

/// Global per-CPU pointers. Null = not initialized.
static PER_CPU_PTRS: [AtomicPtr<PerCpu>; MAX_CPUS] = [
    AtomicPtr::new(core::ptr::null_mut()),
    AtomicPtr::new(core::ptr::null_mut()),
    AtomicPtr::new(core::ptr::null_mut()),
    AtomicPtr::new(core::ptr::null_mut()),
    AtomicPtr::new(core::ptr::null_mut()),
    AtomicPtr::new(core::ptr::null_mut()),
    AtomicPtr::new(core::ptr::null_mut()),
    AtomicPtr::new(core::ptr::null_mut()),
];

/// Initialize PerCpu for a given CPU. Called once per CPU during boot.
/// Heap-allocates to avoid stack overflow from large struct move.
pub fn init_per_cpu(cpu_id: usize, hartid: usize) {
    crate::klog!(sched, debug, "per_cpu init cpu {} hart {}", cpu_id, hartid);

    // Build fields individually to minimize stack usage
    crate::klog!(sched, debug, "per_cpu   RunQueue::new()");
    let rq = RunQueue::new();
    crate::klog!(sched, debug, "per_cpu   TimerWheel::new()");
    let tw = TimerWheel::new();
    crate::klog!(sched, debug, "per_cpu   Box::new(PerCpu)");

    let pc = Box::new(PerCpu {
        run_queue: rq,
        timer_wheel: IrqSafeSpinLock::new(tw),
        hartid,
        cpu_id,
        needs_reschedule: AtomicBool::new(false),
        pcb_onfault: AtomicUsize::new(0),
    });
    let ptr = Box::into_raw(pc);
    PER_CPU_PTRS[cpu_id].store(ptr, Ordering::Release);
    crate::klog!(sched, debug, "per_cpu   done, ptr={:p}", ptr);
}

/// Get PerCpu for a specific CPU by logical ID.
pub fn get(cpu_id: usize) -> &'static PerCpu {
    let ptr = PER_CPU_PTRS[cpu_id].load(Ordering::Acquire);
    assert!(!ptr.is_null(), "PerCpu not initialized for cpu {}", cpu_id);
    unsafe { &*ptr }
}

/// Get PerCpu for the current CPU via tp register.
#[inline]
pub fn current() -> &'static PerCpu {
    let tp: usize;
    unsafe {
        core::arch::asm!("mv {}, tp", out(reg) tp);
    }
    assert!(tp != 0, "tp register not initialized (PerCpu not set up)");
    unsafe { &*(tp as *const PerCpu) }
}

/// Set the tp register to point to the PerCpu for the given CPU.
pub unsafe fn set_tp(cpu_id: usize) {
    let per_cpu = get(cpu_id) as *const PerCpu as usize;
    core::arch::asm!("mv tp, {}", in(reg) per_cpu);
}
