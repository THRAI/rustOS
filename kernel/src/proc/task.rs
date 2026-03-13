//! Task (process) structure.
//!
//! Each Task holds its PID, parent link (Weak to avoid circular Arc),
//! children list, VmMap, state, exit status, and a parent waker for
//! wait4 notification.
//!
//! Per-task TrapFrame persists across .await points (not on kernel stack).
//! Per-task kernel stack (8KB, 2 pages from frame allocator) is used by
//! __user_trap / trap_return for the setjmp/longjmp trap mechanism.

use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU8, AtomicUsize, Ordering};

// use crate::hal_common::IrqSafeSpinLock;
use crate::proc::{alloc_pid, SignalState};
use crate::{
    fs::FdTable,
    hal_common::{PhysAddr, SpinMutex as Mutex, TrapFrame, PAGE_SIZE},
    mm::{frame_alloc_contiguous, frame_free_contiguous, pmap, vm::VmMap},
};

/// Kernel stack size: 4 pages (16KB).
const KSTACK_ORDER: usize = 2; // 2^2 = 4 pages
const KSTACK_SIZE: usize = PAGE_SIZE * (1 << KSTACK_ORDER);

// ---------------------------------------------------------------------------
// TaskState
// ---------------------------------------------------------------------------

/// Process lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TaskState {
    Running = 0,
    Zombie = 1,
}

impl TaskState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => TaskState::Running,
            _ => TaskState::Zombie,
        }
    }
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

/// A process / task.
//TODO: can we group fields into different groups, like mm, proc, etc.
pub struct Task {
    /// Process ID (unique, monotonic).
    pub pid: u32,
    /// Parent process (Weak to prevent circular Arc).
    pub parent: Weak<Task>,
    /// Child processes.
    pub children: Mutex<Vec<Arc<Task>>, 4>,
    /// Virtual address space.
    ///
    // SAFETY: SpinMutex (no IRQ disable). Currently safe because the timer
    // IRQ handler only sets `needs_reschedule` and advances TimerWheel —
    // it never accesses any VmMap. Page faults are synchronous traps on the
    // faulting hart, not asynchronous IRQs, so they cannot preempt a holder
    // on the same hart. Must be upgraded to IrqSafeSpinLock if any future
    // IRQ/IPI path needs to inspect or modify a VmMap.
    pub vm_map: Mutex<VmMap, 1>,
    /// File descriptor table placeholder (expanded in VFS plan).
    pub fd_table: Mutex<FdTable, 4>,
    /// Current working directory (absolute normalized path).
    pub cwd: Mutex<String, 4>,
    /// Current state (Running / Zombie). Stored as AtomicU8 for lock-free access.
    state: AtomicU8,
    /// Exit status, set by sys_exit with Release ordering.
    pub exit_status: AtomicI32,
    /// Waker for parent's WaitChildFuture. Set by wait4 before scanning.
    pub parent_waker: Mutex<Option<core::task::Waker>, 4>,
    /// User-mode register state. Persists across .await points (not on kernel stack).
    pub trap_frame: Mutex<TrapFrame, 4>,
    /// Kernel stack base (2 pages from frame allocator, page-aligned).
    kstack_base: PhysAddr,
    /// Kernel stack pointer (top of kstack, used by trap_return / __user_trap).
    pub kernel_sp: usize,
    /// Program break (brk). Page-aligned end of heap.
    pub brk: AtomicUsize,
    /// Signal state (actions, pending, blocked, altstack).
    pub signals: SignalState,
    /// Process group ID.
    pub pgid: AtomicU32,
    /// Top-level waker for async signal injection (wake from kill).
    pub top_level_waker: Mutex<Option<core::task::Waker>, 4>,
}

/// Allocate a kernel stack (2 pages) and return (base, sp_top).
fn alloc_kstack() -> (PhysAddr, usize) {
    let base = frame_alloc_contiguous(KSTACK_ORDER).expect("failed to allocate kernel stack");
    let sp = base.as_usize() + KSTACK_SIZE;
    (base, sp)
}

// TODO: consider extract some methods from other modules here to ease syscall implementation
impl Task {
    /// Create a new task with the given parent.
    pub fn new(parent: Weak<Task>) -> Arc<Self> {
        let (kstack_base, kernel_sp) = alloc_kstack();
        // Inherit pgid from parent if available, else use own pid
        let pid = alloc_pid();
        let pgid = parent
            .upgrade()
            .map_or(pid, |p| p.pgid.load(Ordering::Relaxed));
        let pmap = Arc::new(Mutex::new(pmap::pmap_create()));
        // crate::kprintln!(
        //     "[task] new pid={} kstack_base={:#x} kernel_sp={:#x}",
        //     pid,
        //     kstack_base.as_usize(),
        //     kernel_sp
        // );
        Arc::new(Self {
            pid,
            parent,
            children: Mutex::new(Vec::new()),
            vm_map: Mutex::new(VmMap::new(pmap)),
            fd_table: Mutex::new(FdTable::new()),
            cwd: Mutex::new(String::from("/")),
            state: AtomicU8::new(TaskState::Running as u8),
            exit_status: AtomicI32::new(0),
            parent_waker: Mutex::new(None),
            trap_frame: Mutex::new(TrapFrame::zero()),
            kstack_base,
            kernel_sp,
            brk: AtomicUsize::new(0),
            signals: SignalState::new(),
            pgid: AtomicU32::new(pgid),
            top_level_waker: Mutex::new(None),
        })
    }

    /// Create init (pid 1) with no parent. Stdio fds pre-populated.
    pub fn new_init() -> Arc<Self> {
        let (kstack_base, kernel_sp) = alloc_kstack();
        let pid = alloc_pid();
        let pmap = Arc::new(Mutex::new(pmap::pmap_create()));
        crate::kprintln!(
            "[task] new_init pid={} kstack_base={:#x} kernel_sp={:#x}",
            pid,
            kstack_base.as_usize(),
            kernel_sp
        );
        Arc::new(Self {
            pid,
            parent: Weak::new(),
            children: Mutex::new(Vec::new()),
            vm_map: Mutex::new(VmMap::new(pmap)),
            fd_table: Mutex::new(FdTable::new_with_stdio()),
            cwd: Mutex::new(String::from("/")),
            state: AtomicU8::new(TaskState::Running as u8),
            exit_status: AtomicI32::new(0),
            parent_waker: Mutex::new(None),
            trap_frame: Mutex::new(TrapFrame::zero()),
            kstack_base,
            kernel_sp,
            brk: AtomicUsize::new(0),
            signals: SignalState::new(),
            pgid: AtomicU32::new(pid),
            top_level_waker: Mutex::new(None),
        })
    }

    /// Get current state.
    pub fn state(&self) -> TaskState {
        TaskState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// Transition to Zombie state (Release ordering for exit_status visibility).
    pub fn set_zombie(&self) {
        self.state.store(TaskState::Zombie as u8, Ordering::Release);
    }

    /// Get PID of parent, or 0 if orphan/init.
    pub fn ppid(&self) -> u32 {
        match self.parent.upgrade() {
            Some(p) => p.pid,
            None => 0,
        }
    }

    /// Release heavyweight per-process resources once the task has exited.
    ///
    /// This keeps zombie tasks lightweight so parent-side `wait4()` reaping
    /// does not end up dropping a full address space on the current kernel stack.
    pub fn release_zombie_resources(&self) {
        self.vm_map.lock().clear();
        *self.fd_table.lock() = FdTable::new();
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        // Free the per-task kernel stack back to the buddy allocator.
        frame_free_contiguous(self.kstack_base, KSTACK_ORDER);
    }
}
