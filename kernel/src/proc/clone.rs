//! Unified process/thread creation via clone(2).
//!
//! `do_clone()` replaces the old `fork()` function with flag-driven dispatch.
//! COW VM logic lives in `VmMap::cow_fork_into()`.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use crate::{
    hal_common::{Errno, KernelResult},
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

/// Unified process/thread creation primitive.
/// Replaces fork() with flag-driven dispatch.
pub fn do_clone(
    parent: &Arc<Task>,
    flags: CloneFlags,
    child_stack: usize,
    tls: usize,
) -> KernelResult<Arc<Task>> {
    // Validate: CLONE_THREAD requires CLONE_VM | CLONE_SIGHAND
    if flags.contains(CloneFlags::THREAD) && !flags.contains(CloneFlags::VM | CloneFlags::SIGHAND) {
        return Err(Errno::Einval);
    }

    let child = Task::new(Arc::downgrade(parent));
    klog!(
        proc,
        debug,
        "do_clone parent={} child={} flags={:?}",
        parent.pid,
        child.pid,
        flags
    );

    // --- Address space ---
    // For now, CLONE_VM is not wired (needs Task Arc-wrapping, done in C2)
    // Always do COW fork for now
    #[cfg(not(feature = "fork-hardcopy"))]
    {
        let mut parent_vm = parent.vm_map.write();
        let mut child_vm = child.vm_map.write();
        parent_vm.cow_fork_into(&mut child_vm)?;
    }
    #[cfg(feature = "fork-hardcopy")]
    {
        super::fork::deep_copy_pages(parent, &child);
    }

    // --- File descriptors ---
    // CLONE_FILES not wired yet (needs Arc-wrapping, done in C2)
    {
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

    // Copy signal actions from parent.
    {
        let parent_actions = parent.signals.actions.lock();
        *child.signals.actions.lock() = *parent_actions;
    }

    // Copy blocked mask from parent.
    child.signals.blocked.store(
        parent.signals.blocked.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );

    // --- Parent-child linkage ---
    parent.children.lock().push(Arc::clone(&child));

    Ok(child)
}
