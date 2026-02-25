//! POSIX signal delivery: sigaction, sigprocmask, kill, sendsig, sigreturn.
//!
//! Signal numbers are 1-indexed (matching Linux). Pending/blocked sets are
//! u64 bitmaps where bit (signo - 1) represents signal `signo`.
//!
//! Delivery path: kill → post_signal → check_pending_signals (at userret) →
//! sendsig (build sigframe on user stack) → handler runs in user mode →
//! sigreturn trampoline → sys_sigreturn restores context.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use hal_common::{Errno, PhysAddr, TrapFrame, VirtAddr, PAGE_SIZE};
use hal_common::SpinMutex as Mutex;

use super::task::Task;

// ---------------------------------------------------------------------------
// Signal numbers (1-indexed, matching Linux)
// ---------------------------------------------------------------------------

pub const SIGHUP: u8 = 1;
pub const SIGINT: u8 = 2;
pub const SIGILL: u8 = 4;
pub const SIGBUS: u8 = 7;
pub const SIGFPE: u8 = 8;
pub const SIGKILL: u8 = 9;
pub const SIGUSR1: u8 = 10;
pub const SIGSEGV: u8 = 11;
pub const SIGUSR2: u8 = 12;
pub const SIGPIPE: u8 = 13;
pub const SIGALRM: u8 = 14;
pub const SIGTERM: u8 = 15;
pub const SIGCHLD: u8 = 17;
pub const SIGCONT: u8 = 18;
pub const SIGSTOP: u8 = 19;

pub const MAX_SIG: u8 = 64;

/// Convert signal number to bitmask bit.
#[inline]
fn sig_bit(sig: u8) -> u64 {
    debug_assert!(sig >= 1 && sig <= MAX_SIG);
    1u64 << (sig - 1)
}

/// Public version of sig_bit for cross-module use.
#[inline]
pub fn sig_bit_pub(sig: u8) -> u64 {
    sig_bit(sig)
}

// ---------------------------------------------------------------------------
// Default signal dispositions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigDefault {
    Terminate,
    Ignore,
    Stop,
    Continue,
}

pub fn default_action(sig: u8) -> SigDefault {
    match sig {
        SIGCHLD => SigDefault::Ignore,
        SIGCONT => SigDefault::Continue,
        SIGSTOP => SigDefault::Stop,
        _ => SigDefault::Terminate,
    }
}

// ---------------------------------------------------------------------------
// SA flags
// ---------------------------------------------------------------------------

pub const SA_SIGINFO: u64 = 4;
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_NOCLDWAIT: u64 = 2;
pub const SA_ONSTACK: u64 = 0x0800_0000;
pub const SA_RESTORER: u64 = 0x0400_0000;

// ---------------------------------------------------------------------------
// SigAction
// ---------------------------------------------------------------------------

pub const SIG_DFL: usize = 0;
pub const SIG_IGN: usize = 1;

#[derive(Debug, Clone, Copy)]
pub struct SigAction {
    pub handler: usize,   // SIG_DFL, SIG_IGN, or function pointer
    pub flags: u64,
    pub restorer: usize,  // SA_RESTORER trampoline (unused, we use sigcode page)
    pub mask: u64,        // signals blocked during handler execution
}

impl SigAction {
    pub const fn default() -> Self {
        Self {
            handler: SIG_DFL,
            flags: 0,
            restorer: 0,
            mask: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-task signal state
// ---------------------------------------------------------------------------

/// Signal state embedded in Task.
pub struct SignalState {
    /// Per-signal actions (indexed by signo - 1, 0..63).
    pub actions: Mutex<[SigAction; MAX_SIG as usize]>,
    /// Pending signals bitmap (atomic for lock-free post_signal).
    pub pending: AtomicU64,
    /// Blocked signals bitmap.
    pub blocked: AtomicU64,
    /// Sigaltstack: (ss_sp, ss_size, ss_flags). 0 = not set.
    pub altstack: Mutex<(usize, usize, u32)>,
}

impl SignalState {
    pub fn new() -> Self {
        Self {
            actions: Mutex::new([SigAction::default(); MAX_SIG as usize]),
            pending: AtomicU64::new(0),
            blocked: AtomicU64::new(0),
            altstack: Mutex::new((0, 0, 0)),
        }
    }

    /// Post a signal (atomic, lock-free).
    pub fn post_signal(&self, sig: u8) {
        self.pending.fetch_or(sig_bit(sig), Ordering::Release);
    }

    /// Check if there are unmasked pending signals (raw bitmap check).
    pub fn has_unmasked_pending(&self) -> bool {
        let pending = self.pending.load(Ordering::Acquire);
        let blocked = self.blocked.load(Ordering::Relaxed);
        let unblockable = sig_bit(SIGKILL) | sig_bit(SIGSTOP);
        (pending & (!blocked | unblockable)) != 0
    }

    /// Check if there are unmasked pending signals that would actually
    /// interrupt (not default-ignored like SIGCHLD with SIG_DFL).
    pub fn has_actionable_pending(&self) -> bool {
        let pending = self.pending.load(Ordering::Acquire);
        let blocked = self.blocked.load(Ordering::Relaxed);
        let unblockable = sig_bit(SIGKILL) | sig_bit(SIGSTOP);
        let deliverable = pending & (!blocked | unblockable);
        if deliverable == 0 {
            return false;
        }
        // Check each deliverable signal: skip those with default-ignore disposition
        let actions = self.actions.lock();
        let mut bits = deliverable;
        while bits != 0 {
            let bit = bits.trailing_zeros() as u8;
            let sig = bit + 1;
            let action = &actions[bit as usize];
            match action.handler {
                SIG_DFL => {
                    if default_action(sig) != SigDefault::Ignore {
                        return true; // Would terminate/stop
                    }
                }
                SIG_IGN => {} // Explicitly ignored
                _ => return true, // User handler installed
            }
            bits &= !(1u64 << bit);
        }
        false
    }

    /// Check if the given signal's action has SA_RESTART set.
    pub fn is_restart(&self, sig: u8) -> bool {
        if sig < 1 || sig > MAX_SIG {
            return false;
        }
        let actions = self.actions.lock();
        actions[(sig - 1) as usize].flags & SA_RESTART != 0
    }

    /// Dequeue the highest-priority unmasked pending signal. Returns None if none.
    pub fn dequeue_signal(&self) -> Option<u8> {
        loop {
            let pending = self.pending.load(Ordering::Acquire);
            let blocked = self.blocked.load(Ordering::Relaxed);
            let unblockable = sig_bit(SIGKILL) | sig_bit(SIGSTOP);
            let deliverable = pending & (!blocked | unblockable);
            if deliverable == 0 {
                return None;
            }
            // Pick lowest-numbered signal
            let bit = deliverable.trailing_zeros() as u8;
            let sig = bit + 1;
            let mask = 1u64 << bit;
            // Atomically clear the bit
            let old = self.pending.fetch_and(!mask, Ordering::AcqRel);
            if old & mask != 0 {
                return Some(sig);
            }
            // Race: someone else cleared it, retry
        }
    }
}

// ---------------------------------------------------------------------------
// Sigcode page: mapped RO in every process at a fixed VA
// ---------------------------------------------------------------------------

/// Fixed VA for the sigcode (sigreturn trampoline) page.
/// Placed just above USER_STACK_TOP to avoid collisions.
pub const SIGCODE_VA: usize = 0x0000_003F_FFFF_F000;

/// SYS_rt_sigreturn on rv64 Linux = 139
const SYS_RT_SIGRETURN: usize = 139;

/// Build the sigcode page contents: `li a7, 139; ecall; unimp`
/// Returns a page-sized buffer.
pub fn build_sigcode_page() -> [u8; PAGE_SIZE] {
    let mut page = [0u8; PAGE_SIZE];
    // RISC-V instructions (little-endian):
    // li a7, 139  =>  addi a7, zero, 139  =>  08b00893
    // ecall       =>  00000073
    // unimp       =>  00000000 (c.unimp = 0x0000)
    let li_a7: u32 = 0x08b0_0893; // addi a7, x0, 139
    let ecall: u32 = 0x0000_0073;
    page[0..4].copy_from_slice(&li_a7.to_le_bytes());
    page[4..8].copy_from_slice(&ecall.to_le_bytes());
    // Rest is zeros (unimp / padding)
    page
}

// ---------------------------------------------------------------------------
// Signal frame (sigframe) on user stack
// ---------------------------------------------------------------------------

/// Signal frame pushed onto user stack before redirecting to handler.
/// Layout must match what sigreturn expects.
#[repr(C)]
pub struct SigFrame {
    /// Saved trap frame (full register state at point of interruption).
    pub saved_tf: TrapFrame,
    /// Signal number.
    pub signo: u32,
    /// Padding for alignment.
    pub _pad: u32,
    /// Signal mask to restore on sigreturn.
    pub saved_mask: u64,
}

const SIGFRAME_SIZE: usize = core::mem::size_of::<SigFrame>();

// ---------------------------------------------------------------------------
// sendsig: build sigframe on user stack, redirect to handler
// ---------------------------------------------------------------------------

/// Build a signal frame on the user stack and redirect execution to the handler.
/// Returns Ok(()) on success, Err(()) if the user stack is trashed (caller should kill).
pub fn sendsig(task: &Arc<Task>, sig: u8, action: &SigAction) -> Result<(), ()> {
    let mut tf = task.trap_frame.lock();
    let sig_state = &task.signals;

    // Determine stack pointer for signal frame
    let mut sp = tf.x[2]; // current user SP

    // Check SA_ONSTACK
    if action.flags & SA_ONSTACK != 0 {
        let altstack = sig_state.altstack.lock();
        let (ss_sp, ss_size, ss_flags) = *altstack;
        if ss_sp != 0 && ss_flags == 0 {
            // Use altstack if not already on it
            let alt_top = ss_sp + ss_size;
            if sp < ss_sp || sp >= alt_top {
                sp = alt_top;
            }
        }
    }

    // Align SP down and make room for SigFrame
    sp = (sp - SIGFRAME_SIZE) & !0xF; // 16-byte aligned

    // Build the sigframe in kernel memory
    let frame = SigFrame {
        saved_tf: *tf,
        signo: sig as u32,
        _pad: 0,
        saved_mask: sig_state.blocked.load(Ordering::Relaxed),
    };

    // Copyout to user stack using pcb_onfault guard
    let ok = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            sp as *mut u8,
            &frame as *const SigFrame as *const u8,
            SIGFRAME_SIZE,
        )
    };
    if ok != 0 {
        return Err(()); // user stack trashed
    }

    // Block signals specified in sa_mask + the signal itself during handler
    let block_mask = action.mask | sig_bit(sig);
    sig_state.blocked.fetch_or(block_mask, Ordering::Release);

    // Redirect trap frame to handler
    tf.sepc = action.handler;
    tf.x[10] = sig as usize;           // a0 = signo
    tf.x[11] = 0;                       // a1 = siginfo (NULL for now)
    tf.x[12] = sp;                       // a2 = ucontext (pointer to sigframe)
    tf.x[2] = sp;                        // sp = sigframe
    tf.x[1] = SIGCODE_VA;               // ra = sigreturn trampoline

    // Sanitize sstatus: SPP=0 (user mode), SPIE=1
    tf.sstatus = (tf.sstatus & !(1 << 8)) | (1 << 5); // clear SPP, set SPIE

    Ok(())
}

// ---------------------------------------------------------------------------
// sigreturn: restore context from sigframe
// ---------------------------------------------------------------------------

/// Restore trap frame from the signal frame on the user stack.
/// Returns Ok(()) on success, Err(Errno) on invalid frame.
pub fn sys_sigreturn(task: &Arc<Task>) -> Result<(), Errno> {
    let sp = task.trap_frame.lock().x[2]; // current SP points to sigframe

    // Copyin the sigframe from user memory
    let mut frame = core::mem::MaybeUninit::<SigFrame>::uninit();
    let ok = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            frame.as_mut_ptr() as *mut u8,
            sp as *const u8,
            SIGFRAME_SIZE,
        )
    };
    if ok != 0 {
        return Err(Errno::EFAULT);
    }
    let frame = unsafe { frame.assume_init() };

    // Validate sepc: must be in user space (< 0x0000_0040_0000_0000)
    const USER_MAX_VA: usize = 0x0000_0040_0000_0000;
    if frame.saved_tf.sepc >= USER_MAX_VA {
        return Err(Errno::EINVAL);
    }

    // Restore trap frame with sanitization
    {
        let mut tf = task.trap_frame.lock();
        // Restore all GPRs
        tf.x = frame.saved_tf.x;
        tf.sepc = frame.saved_tf.sepc;
        // Sanitize sstatus: SPP cleared (user mode), SPIE set
        tf.sstatus = (frame.saved_tf.sstatus & !(1 << 8)) | (1 << 5);
        // scause/stval not restored (kernel-only)
    }

    // Restore signal mask
    task.signals.blocked.store(frame.saved_mask, Ordering::Release);

    Ok(())
}

// ---------------------------------------------------------------------------
// check_pending_signals: called at userret
// ---------------------------------------------------------------------------

/// Check and deliver pending signals. Called before returning to user mode.
/// Returns true if a signal was delivered (trap frame modified).
/// Returns Err(sig) if the process should be killed (SIGKILL or unhandled fatal).
pub fn check_pending_signals(task: &Arc<Task>) -> Result<bool, u8> {
    let sig = match task.signals.dequeue_signal() {
        Some(s) => s,
        None => return Ok(false),
    };

    // SIGKILL: always fatal, no handler
    if sig == SIGKILL {
        return Err(sig);
    }

    // SIGSTOP/SIGCONT: simplified handling (just ignore for now)
    if sig == SIGSTOP || sig == SIGCONT {
        return Ok(false);
    }

    let action = {
        let actions = task.signals.actions.lock();
        actions[(sig - 1) as usize]
    };

    match action.handler {
        SIG_DFL => {
            match default_action(sig) {
                SigDefault::Terminate => Err(sig),
                SigDefault::Ignore => Ok(false),
                SigDefault::Stop => Ok(false),   // simplified
                SigDefault::Continue => Ok(false),
            }
        }
        SIG_IGN => Ok(false),
        _handler => {
            // Deliver to user handler via sendsig
            match sendsig(task, sig, &action) {
                Ok(()) => Ok(true),
                Err(()) => {
                    // sendsig failed (bad user stack) — kill with SIGILL
                    Err(SIGILL)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// sys_sigaction
// ---------------------------------------------------------------------------

/// Linux rt_sigaction struct layout (rv64):
/// - sa_handler: u64
/// - sa_flags: u64
/// - sa_restorer: u64
/// - sa_mask: u64
const SIGACTION_USER_SIZE: usize = 32; // 4 * 8

pub fn sys_sigaction(
    task: &Arc<Task>,
    sig: usize,
    act_ptr: usize,
    oldact_ptr: usize,
) -> Result<usize, Errno> {
    if sig < 1 || sig > MAX_SIG as usize {
        return Err(Errno::EINVAL);
    }
    // Cannot change SIGKILL or SIGSTOP
    if sig == SIGKILL as usize || sig == SIGSTOP as usize {
        return Err(Errno::EINVAL);
    }

    let idx = sig - 1;
    let mut actions = task.signals.actions.lock();

    // Write old action to user memory
    if oldact_ptr != 0 {
        let old = actions[idx];
        let buf: [u64; 4] = [
            old.handler as u64,
            old.flags,
            old.restorer as u64,
            old.mask,
        ];
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                oldact_ptr as *mut u8,
                buf.as_ptr() as *const u8,
                SIGACTION_USER_SIZE,
            )
        };
        if rc != 0 {
            return Err(Errno::EFAULT);
        }
    }

    // Read new action from user memory
    if act_ptr != 0 {
        let mut buf = [0u64; 4];
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                buf.as_mut_ptr() as *mut u8,
                act_ptr as *const u8,
                SIGACTION_USER_SIZE,
            )
        };
        if rc != 0 {
            return Err(Errno::EFAULT);
        }
        actions[idx] = SigAction {
            handler: buf[0] as usize,
            flags: buf[1],
            restorer: buf[2] as usize,
            mask: buf[3],
        };
    }

    Ok(0)
}

// ---------------------------------------------------------------------------
// sys_sigprocmask
// ---------------------------------------------------------------------------

pub const SIG_BLOCK: usize = 0;
pub const SIG_UNBLOCK: usize = 1;
pub const SIG_SETMASK: usize = 2;

pub fn sys_sigprocmask(
    task: &Arc<Task>,
    how: usize,
    set_ptr: usize,
    oldset_ptr: usize,
) -> Result<usize, Errno> {
    let sig_state = &task.signals;

    // Write old mask
    if oldset_ptr != 0 {
        let old = sig_state.blocked.load(Ordering::Relaxed);
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                oldset_ptr as *mut u8,
                &old as *const u64 as *const u8,
                8,
            )
        };
        if rc != 0 {
            return Err(Errno::EFAULT);
        }
    }

    // Read new set and apply
    if set_ptr != 0 {
        let mut new_set: u64 = 0;
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                &mut new_set as *mut u64 as *mut u8,
                set_ptr as *const u8,
                8,
            )
        };
        if rc != 0 {
            return Err(Errno::EFAULT);
        }

        // Cannot block SIGKILL or SIGSTOP
        let unblockable = sig_bit(SIGKILL) | sig_bit(SIGSTOP);
        new_set &= !unblockable;

        match how {
            SIG_BLOCK => { sig_state.blocked.fetch_or(new_set, Ordering::Release); }
            SIG_UNBLOCK => { sig_state.blocked.fetch_and(!new_set, Ordering::Release); }
            SIG_SETMASK => { sig_state.blocked.store(new_set, Ordering::Release); }
            _ => return Err(Errno::EINVAL),
        }
    }

    Ok(0)
}

// ---------------------------------------------------------------------------
// sys_kill + global task registry
// ---------------------------------------------------------------------------

use hal_common::SpinMutex;

/// Global task registry for kill() iteration.
static TASK_REGISTRY: SpinMutex<Vec<Arc<Task>>> = SpinMutex::new(Vec::new());

/// Register a task in the global registry (called on task creation).
pub fn register_task(task: &Arc<Task>) {
    TASK_REGISTRY.lock().push(Arc::clone(task));
}

/// Unregister a task from the global registry (called on zombie reap).
pub fn unregister_task(pid: u32) {
    TASK_REGISTRY.lock().retain(|t| t.pid != pid);
}

/// Send a signal to a process or process group.
pub fn sys_kill(sender: &Arc<Task>, pid: isize, sig: u8) -> Result<usize, Errno> {
    if sig > MAX_SIG {
        return Err(Errno::EINVAL);
    }

    if pid > 0 {
        // Send to specific process
        let target = {
            let registry = TASK_REGISTRY.lock();
            registry.iter().find(|t| t.pid == pid as u32).cloned()
        };
        match target {
            Some(t) => {
                if sig > 0 {
                    t.signals.post_signal(sig);
                    // Wake the task's top-level waker if set
                    if let Some(w) = t.top_level_waker.lock().take() {
                        w.wake();
                    }
                }
                Ok(0)
            }
            None => Err(Errno::ESRCH),
        }
    } else if pid == 0 {
        // Send to all processes in sender's process group
        let pgid = sender.pgid.load(Ordering::Relaxed);
        kill_pgrp(pgid, sig);
        Ok(0)
    } else if pid == -1 {
        // Send to all processes (except init)
        let registry = TASK_REGISTRY.lock();
        for t in registry.iter() {
            if t.pid != 1 && sig > 0 {
                t.signals.post_signal(sig);
                if let Some(w) = t.top_level_waker.lock().take() {
                    w.wake();
                }
            }
        }
        Ok(0)
    } else {
        // pid < -1: send to process group |pid|
        let pgid = (-pid) as u32;
        kill_pgrp(pgid, sig);
        Ok(0)
    }
}

fn kill_pgrp(pgid: u32, sig: u8) {
    if sig == 0 { return; }
    let registry = TASK_REGISTRY.lock();
    for t in registry.iter() {
        if t.pgid.load(Ordering::Relaxed) == pgid {
            t.signals.post_signal(sig);
            if let Some(w) = t.top_level_waker.lock().take() {
                w.wake();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// sys_setpgid / sys_getpgid
// ---------------------------------------------------------------------------

pub fn sys_setpgid(task: &Arc<Task>, pid: u32, pgid: u32) -> Result<usize, Errno> {
    let target_pid = if pid == 0 { task.pid } else { pid };
    let new_pgid = if pgid == 0 { target_pid } else { pgid };

    if target_pid == task.pid {
        task.pgid.store(new_pgid, Ordering::Release);
        Ok(0)
    } else {
        // Find child with matching pid
        let children = task.children.lock();
        for child in children.iter() {
            if child.pid == target_pid {
                child.pgid.store(new_pgid, Ordering::Release);
                return Ok(0);
            }
        }
        Err(Errno::ESRCH)
    }
}

pub fn sys_getpgid(task: &Arc<Task>, pid: u32) -> Result<usize, Errno> {
    if pid == 0 {
        Ok(task.pgid.load(Ordering::Relaxed) as usize)
    } else {
        let registry = TASK_REGISTRY.lock();
        for t in registry.iter() {
            if t.pid == pid {
                return Ok(t.pgid.load(Ordering::Relaxed) as usize);
            }
        }
        Err(Errno::ESRCH)
    }
}

// ---------------------------------------------------------------------------
// Map sigcode page into a process's address space
// ---------------------------------------------------------------------------

/// Map the sigcode trampoline page into the given pmap.
/// Called during exec (and for init).
pub fn map_sigcode_page(pmap: &mut crate::mm::pmap::Pmap) {
    use crate::mm::allocator::frame_alloc_sync;
    use crate::mm::pmap::pmap_enter;
    use crate::mm::vm::vm_map::MapPerm;

    let frame = frame_alloc_sync().expect("sigcode page alloc failed");
    let page_data = build_sigcode_page();

    // Write sigcode to the physical frame
    unsafe {
        core::ptr::copy_nonoverlapping(
            page_data.as_ptr(),
            frame.as_usize() as *mut u8,
            PAGE_SIZE,
        );
    }

    // Map as read-only + user-accessible
    let prot = MapPerm::R | MapPerm::X | MapPerm::U;
    let _ = pmap_enter(pmap, VirtAddr::new(SIGCODE_VA), frame, prot, false);
}
