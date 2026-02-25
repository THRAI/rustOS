//! Task (process) structure.
//!
//! Each Task holds its PID, parent link (Weak to avoid circular Arc),
//! children list, VmMap, state, exit status, and a parent waker for
//! wait4 notification.
//!
//! Per-task TrapFrame persists across .await points (not on kernel stack).
//! Per-task kernel stack (8KB, 2 pages from frame allocator) is used by
//! __user_trap / trap_return for the setjmp/longjmp trap mechanism.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use hal_common::{PhysAddr, TrapFrame, PAGE_SIZE};
use hal_common::SpinMutex as Mutex;

use crate::mm::vm::vm_map::VmMap;
use crate::mm::allocator::{frame_alloc_contiguous, frame_free_contiguous};
use crate::mm::pmap::{self, Pmap};
use crate::fs::fd_table::FdTable;

use super::pid::alloc_pid;
use super::signal::SignalState;

/// Kernel stack size: 2 pages (8KB).
const KSTACK_ORDER: usize = 1; // 2^1 = 2 pages
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
pub struct Task {
    /// Process ID (unique, monotonic).
    pub pid: u32,
    /// Parent process (Weak to prevent circular Arc).
    pub parent: Weak<Task>,
    /// Child processes.
    pub children: Mutex<Vec<Arc<Task>>>,
    /// Virtual address space.
    pub vm_map: Mutex<VmMap>,
    /// File descriptor table placeholder (expanded in VFS plan).
    pub fd_table: Mutex<FdTable>,
    /// Current state (Running / Zombie). Stored as AtomicU8 for lock-free access.
    state: AtomicU8,
    /// Exit status, set by sys_exit with Release ordering.
    pub exit_status: AtomicI32,
    /// Waker for parent's WaitChildFuture. Set by wait4 before scanning.
    pub parent_waker: Mutex<Option<core::task::Waker>>,
    /// User-mode register state. Persists across .await points (not on kernel stack).
    pub trap_frame: Mutex<TrapFrame>,
    /// Per-process page table (Sv39).
    pub pmap: Mutex<Pmap>,
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
    pub top_level_waker: Mutex<Option<core::task::Waker>>,
}

/// Allocate a kernel stack (2 pages) and return (base, sp_top).
fn alloc_kstack() -> (PhysAddr, usize) {
    let base = frame_alloc_contiguous(KSTACK_ORDER)
        .expect("failed to allocate kernel stack");
    let sp = base.as_usize() + KSTACK_SIZE;
    (base, sp)
}

impl Task {
    /// Create a new task with the given parent.
    pub fn new(parent: Weak<Task>) -> Arc<Self> {
        let (kstack_base, kernel_sp) = alloc_kstack();
        // Inherit pgid from parent if available, else use own pid
        let pid = alloc_pid();
        let pgid = parent.upgrade().map_or(pid, |p| p.pgid.load(Ordering::Relaxed));
        Arc::new(Self {
            pid,
            parent,
            children: Mutex::new(Vec::new()),
            vm_map: Mutex::new(VmMap::new()),
            fd_table: Mutex::new(FdTable::new()),
            state: AtomicU8::new(TaskState::Running as u8),
            exit_status: AtomicI32::new(0),
            parent_waker: Mutex::new(None),
            trap_frame: Mutex::new(TrapFrame::zero()),
            pmap: Mutex::new(pmap::pmap_create()),
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
        Arc::new(Self {
            pid,
            parent: Weak::new(),
            children: Mutex::new(Vec::new()),
            vm_map: Mutex::new(VmMap::new()),
            fd_table: Mutex::new(FdTable::new_with_stdio()),
            state: AtomicU8::new(TaskState::Running as u8),
            exit_status: AtomicI32::new(0),
            parent_waker: Mutex::new(None),
            trap_frame: Mutex::new(TrapFrame::zero()),
            pmap: Mutex::new(pmap::pmap_create()),
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
}

impl Drop for Task {
    fn drop(&mut self) {
        // Free page table pages.
        pmap::pmap_destroy(&mut self.pmap.lock());
        // Free the per-task kernel stack back to the buddy allocator.
        frame_free_contiguous(self.kstack_base, KSTACK_ORDER);
    }
}
