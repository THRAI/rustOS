//! Unified process/thread creation via clone(2).
//!
//! `do_clone()` replaces the old `fork()` function with flag-driven dispatch.
//! COW VM logic lives in `VmMap::cow_fork_into()`.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{
    hal_common::{Errno, KernelResult, SpinMutex},
    proc::{SigSet, Task},
};

bitflags::bitflags! {
    /// Linux clone(2) flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CloneFlags: u64 {
        const VM             = 0x0000_0100;
        const FILES          = 0x0000_0400;
        const SIGHAND        = 0x0000_0800;
        const VFORK          = 0x0000_4000;
        const THREAD         = 0x0001_0000;
        const SETTLS         = 0x0008_0000;
        const PARENT_SETTID  = 0x0010_0000;
        const CHILD_CLEARTID = 0x0020_0000;
        const CHILD_SETTID   = 0x0100_0000;
    }
}

// ---------------------------------------------------------------------------
// VforkDone: parent-blocks-until-child-exits/execs
// ---------------------------------------------------------------------------

/// Completion handle for vfork semantics.
///
/// The parent spins (or yields) on `is_done()` after spawning a CLONE_VFORK
/// child. The child calls `signal()` on exit or exec to unblock the parent.
pub struct VforkDone {
    done: AtomicBool,
    waker: SpinMutex<Option<core::task::Waker>, 4>,
}

impl VforkDone {
    pub fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            waker: SpinMutex::new(None),
        }
    }

    /// Signal completion — child has exited or exec'd.
    pub fn signal(&self) {
        self.done.store(true, Ordering::Release);
        if let Some(w) = self.waker.lock().take() {
            w.wake();
        }
    }

    /// Check if child has signalled completion.
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
}

// ---------------------------------------------------------------------------
// do_clone
// ---------------------------------------------------------------------------

/// Unified process/thread creation primitive.
/// Replaces fork() with flag-driven dispatch.
///
/// Returns the child `Task` and an optional `VforkDone` handle (set when
/// `CLONE_VFORK` is requested — the caller must block on it).
pub fn do_clone(
    parent: &Arc<Task>,
    flags: CloneFlags,
    child_stack: usize,
    tls: usize,
    child_tid: usize,
) -> KernelResult<(Arc<Task>, Option<Arc<VforkDone>>)> {
    // Validate: CLONE_THREAD requires CLONE_VM | CLONE_SIGHAND
    if flags.contains(CloneFlags::THREAD) && !flags.contains(CloneFlags::VM | CloneFlags::SIGHAND) {
        return Err(Errno::Einval);
    }

    let mut child = Task::new(Arc::downgrade(parent));
    klog!(
        proc,
        debug,
        "do_clone parent={} child={} flags={:?}",
        parent.pid,
        child.pid,
        flags
    );

    // --- Address space ---
    if flags.contains(CloneFlags::VM) {
        // Share address space: point child's vm_map Arc to parent's.
        // child refcount == 1 (just created), so Arc::get_mut is safe.
        let task_mut = Arc::get_mut(&mut child).expect("child is not shared yet");
        task_mut.vm_map = Arc::clone(&parent.vm_map);
    } else {
        // COW fork
        let mut parent_vm = parent.vm_map.write();
        let mut child_vm = child.vm_map.write();
        parent_vm.cow_fork_into(&mut child_vm)?;
    }

    // --- File descriptors ---
    if flags.contains(CloneFlags::FILES) {
        // Share fd_table: point child's Arc to parent's
        let task_mut = Arc::get_mut(&mut child).expect("child is not shared yet");
        task_mut.fd_table = Arc::clone(&parent.fd_table);
    } else {
        // Copy (default fork path)
        let parent_fds = parent.fd_table.lock();
        *child.fd_table.lock() = parent_fds.fork();
    }

    // --- CWD ---
    {
        *child.cwd.lock() = parent.cwd.lock().clone();
    }

    // --- Trap frame ---
    {
        let mut child_tf = child.trap_frame.lock();
        *child_tf = *parent.trap_frame.lock();
        child_tf.set_ret_val(0);
        child_tf.advance_pc(); // skip ecall — parent's sepc is advanced by dispatcher, child's must be done here
        if child_stack != 0 {
            child_tf.x[2] = child_stack;
        }
        if flags.contains(CloneFlags::SETTLS) && tls != 0 {
            child_tf.x[4] = tls;
        }
    }

    // --- brk ---
    child
        .brk
        .store(parent.brk.load(Ordering::Relaxed), Ordering::Relaxed);

    // --- Signals ---
    // Clear pending signals in child (POSIX: pending signals not inherited).
    child
        .signals
        .pending
        .store(SigSet::empty(), Ordering::Relaxed);

    // Signal actions: CLONE_SIGHAND shares the handler table.
    // Without CLONE_SIGHAND, child gets a private copy.
    if flags.contains(CloneFlags::SIGHAND) {
        let task_mut = Arc::get_mut(&mut child).expect("child is not shared yet");
        task_mut.signals.actions = Arc::clone(&parent.signals.actions);
    } else {
        let parent_actions = parent.signals.actions.lock();
        *child.signals.actions.lock() = *parent_actions;
    }

    // Copy blocked mask from parent.
    child.signals.blocked.store(
        parent.signals.blocked.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );

    // --- Thread-group semantics ---
    if flags.contains(CloneFlags::THREAD) {
        let task_mut = Arc::get_mut(&mut child).expect("child is not shared yet");
        task_mut.tgid = parent.tgid;
    }

    // --- Child TID semantics ---
    if flags.contains(CloneFlags::CHILD_CLEARTID) && child_tid != 0 {
        child
            .clear_child_tid
            .store(child_tid, Ordering::Relaxed);
    }

    if flags.contains(CloneFlags::CHILD_SETTID) && child_tid != 0 {
        let tid = child.pid;
        let rc = unsafe {
            crate::hal::copy_user_chunk(
                child_tid as *mut u8,
                &tid as *const u32 as *const u8,
                core::mem::size_of::<u32>(),
            )
        };
        if rc != 0 {
            return Err(Errno::Efault);
        }
    }

    // --- CLONE_VFORK ---
    let vfork_done = if flags.contains(CloneFlags::VFORK) {
        let vfork = Arc::new(VforkDone::new());
        let task_mut = Arc::get_mut(&mut child).expect("child is not shared yet");
        task_mut.vfork_done = Some(Arc::clone(&vfork));
        Some(vfork)
    } else {
        None
    };

    // --- Parent-child linkage ---
    // CLONE_THREAD tasks are threads in the same thread group; they should
    // not participate in process-level wait4/SIGCHLD child accounting.
    if !flags.contains(CloneFlags::THREAD) {
        parent.children.lock().push(Arc::clone(&child));
    }

    Ok((child, vfork_done))
}
