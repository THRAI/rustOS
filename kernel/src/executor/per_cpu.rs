//! Per-CPU data structure.
//!
//! Each CPU has a PerCpu holding its run queue, timer wheel, and identity.
//! Accessed via the tp register (hot path) or global array (cross-CPU).

use hal_common::{IrqSafeSpinLock, RunQueue, TimerWheel};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Maximum number of CPUs supported.
pub const MAX_CPUS: usize = 8;

/// Per-CPU data: run queue, timer wheel, hart/cpu identity, preemption flag, fixup pointer.
pub struct PerCpu {
    pub run_queue: RunQueue<async_task::Runnable>,
    pub timer_wheel: IrqSafeSpinLock<TimerWheel>,
    pub hartid: usize,
    pub cpu_id: usize,
    /// Set by timer IRQ, checked by yield_now for cooperative preemption.
    pub needs_reschedule: AtomicBool,
    /// Exception fixup pointer: when non-zero, the trap handler redirects
    /// load/store page faults to this address (the copy_user_chunk landing pad)
    /// instead of panicking. Set by copy_user_chunk prologue, cleared by epilogue
    /// and landing pad.
    pub pcb_onfault: AtomicUsize,
}

impl PerCpu {
    /// Create a new PerCpu for the given CPU.
    pub fn new(cpu_id: usize, hartid: usize) -> Self {
        Self {
            run_queue: RunQueue::new(),
            timer_wheel: IrqSafeSpinLock::new(TimerWheel::new()),
            hartid,
            cpu_id,
            needs_reschedule: AtomicBool::new(false),
            pcb_onfault: AtomicUsize::new(0),
        }
    }
}

/// Global per-CPU data array. Each element is initialized via spin::Once
/// to bypass const-initialization limits of VecDeque/Vec inside RunQueue/TimerWheel.
static PER_CPU_DATA: [spin::Once<PerCpu>; MAX_CPUS] = [
    spin::Once::new(), spin::Once::new(), spin::Once::new(), spin::Once::new(),
    spin::Once::new(), spin::Once::new(), spin::Once::new(), spin::Once::new(),
];

/// Initialize PerCpu for a given CPU. Called once per CPU during boot.
pub fn init_per_cpu(cpu_id: usize, hartid: usize) {
    PER_CPU_DATA[cpu_id].call_once(|| PerCpu::new(cpu_id, hartid));
}

/// Get PerCpu for a specific CPU by logical ID.
/// Panics if accessed before init -- correct behavior for a pre-boot access bug.
pub fn get(cpu_id: usize) -> &'static PerCpu {
    PER_CPU_DATA[cpu_id]
        .get()
        .expect("PerCpu not initialized")
}

/// Get PerCpu for the current CPU via tp register.
///
/// In Phase 1, tp is set in rust_main after PerCpu init.
/// For Phase 1 (single hart), this reads the tp register which points
/// to the PerCpu struct.
#[inline]
pub fn current() -> &'static PerCpu {
    let tp: usize;
    unsafe {
        core::arch::asm!("mv {}, tp", out(reg) tp);
    }
    // tp points directly to the PerCpu struct
    assert!(tp != 0, "tp register not initialized (PerCpu not set up)");
    unsafe { &*(tp as *const PerCpu) }
}

/// Set the tp register to point to the PerCpu for the given CPU.
/// Called once per CPU during boot after init_per_cpu.
pub unsafe fn set_tp(cpu_id: usize) {
    let per_cpu = get(cpu_id) as *const PerCpu as usize;
    core::arch::asm!("mv tp, {}", in(reg) per_cpu);
}
