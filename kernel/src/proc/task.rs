//! Task (process) structure.
//!
//! Each Task holds its PID, parent link (Weak to avoid circular Arc),
//! children list, VmMap, state, exit status, and a parent waker for
//! wait4 notification.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use spin::Mutex;

use crate::mm::vm::vm_map::VmMap;
use crate::fs::fd_table::FdTable;

use super::pid::alloc_pid;

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
}

impl Task {
    /// Create a new task with the given parent.
    pub fn new(parent: Weak<Task>) -> Arc<Self> {
        Arc::new(Self {
            pid: alloc_pid(),
            parent,
            children: Mutex::new(Vec::new()),
            vm_map: Mutex::new(VmMap::new()),
            fd_table: Mutex::new(FdTable::new()),
            state: AtomicU8::new(TaskState::Running as u8),
            exit_status: AtomicI32::new(0),
            parent_waker: Mutex::new(None),
        })
    }

    /// Create init (pid 1) with no parent.
    pub fn new_init() -> Arc<Self> {
        Arc::new(Self {
            pid: alloc_pid(),
            parent: Weak::new(),
            children: Mutex::new(Vec::new()),
            vm_map: Mutex::new(VmMap::new()),
            fd_table: Mutex::new(FdTable::new()),
            state: AtomicU8::new(TaskState::Running as u8),
            exit_status: AtomicI32::new(0),
            parent_waker: Mutex::new(None),
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
