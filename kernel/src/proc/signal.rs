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
use bitflags::bitflags;
use core::sync::atomic::{AtomicU64, Ordering};

use hal_common::SpinMutex as Mutex;
use hal_common::{TrapFrame, VirtAddr, PAGE_SIZE};

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

/// Strongly-typed POSIX signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Signal(u8);

impl Signal {
    #[allow(unused)]
    pub const fn new(sig: u8) -> Option<Self> {
        if Self::is_legit(sig) {
            Some(Self(sig))
        } else {
            None
        }
    }

    pub const fn is_legit(sig: u8) -> bool {
        sig >= 1 && sig <= MAX_SIG
    }

    pub const fn new_unchecked(sig: u8) -> Self {
        Self(sig)
    }

    pub const fn as_u8(self) -> u8 {
        self.0
    }

    pub const fn as_bit(self) -> u64 {
        1u64 << (self.0 - 1)
    }
}

impl core::fmt::Display for Signal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match self.0 {
            SIGHUP => "SIGHUP",
            SIGINT => "SIGINT",
            SIGILL => "SIGILL",
            SIGBUS => "SIGBUS",
            SIGFPE => "SIGFPE",
            SIGKILL => "SIGKILL",
            SIGUSR1 => "SIGUSR1",
            SIGSEGV => "SIGSEGV",
            SIGUSR2 => "SIGUSR2",
            SIGPIPE => "SIGPIPE",
            SIGALRM => "SIGALRM",
            SIGTERM => "SIGTERM",
            SIGCHLD => "SIGCHLD",
            SIGCONT => "SIGCONT",
            SIGSTOP => "SIGSTOP",
            _ => return write!(f, "SIG?({})", self.0),
        };
        write!(f, "{}({})", name, self.0)
    }
}

/// A strongly-typed bitmap of posix signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SigSet(pub u64);

impl SigSet {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_u64(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub fn add(&mut self, sig: Signal) -> &mut Self {
        self.0 |= sig.as_bit();
        self
    }

    #[allow(unused)]
    pub fn remove(&mut self, sig: Signal) -> &mut Self {
        self.0 &= !sig.as_bit();
        self
    }

    pub const fn contains(self, sig: Signal) -> bool {
        self.0 & sig.as_bit() != 0
    }

    #[allow(unused)]
    pub const fn contains_bit(self, bits: u64) -> bool {
        self.0 & bits != 0
    }

    pub fn union(self, other: SigSet) -> SigSet {
        SigSet(self.0 | other.0)
    }

    pub fn intersect(self, other: SigSet) -> SigSet {
        SigSet(self.0 & other.0)
    }

    pub fn difference(self, other: SigSet) -> SigSet {
        SigSet(self.0 & !other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// An atomic equivalent of `SigSet`, wrapping `AtomicU64`.
pub struct AtomicSigSet(AtomicU64);

impl AtomicSigSet {
    pub const fn new(set: SigSet) -> Self {
        Self(AtomicU64::new(set.as_u64()))
    }

    pub fn load(&self, order: Ordering) -> SigSet {
        SigSet::from_u64(self.0.load(order))
    }

    pub fn store(&self, set: SigSet, order: Ordering) {
        self.0.store(set.as_u64(), order)
    }

    pub fn fetch_add(&self, sig: Signal, order: Ordering) -> SigSet {
        SigSet::from_u64(self.0.fetch_or(sig.as_bit(), order))
    }

    pub fn fetch_remove(&self, sig: Signal, order: Ordering) -> SigSet {
        SigSet::from_u64(self.0.fetch_and(!sig.as_bit(), order))
    }

    pub fn fetch_union(&self, set: SigSet, order: Ordering) -> SigSet {
        SigSet::from_u64(self.0.fetch_or(set.as_u64(), order))
    }

    #[allow(unused)]
    pub fn fetch_intersect(&self, set: SigSet, order: Ordering) -> SigSet {
        SigSet::from_u64(self.0.fetch_and(set.as_u64(), order))
    }

    pub fn fetch_difference(&self, set: SigSet, order: Ordering) -> SigSet {
        SigSet::from_u64(self.0.fetch_and(!set.as_u64(), order))
    }
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

bitflags! {
    /// POSIX SigAction flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct SAFlags: u64 {
        const NOCLDSTOP = 1;
        const NOCLDWAIT = 2;
        const SIGINFO   = 4;
        const ONSTACK   = 0x0800_0000;
        const RESTART   = 0x1000_0000;
        const NODEFER   = 0x4000_0000;
        const RESETHAND = 0x8000_0000;
        const RESTORER  = 0x0400_0000;
    }
}

pub const SA_NOCLDSTOP: u64 = SAFlags::NOCLDSTOP.bits();
pub const SA_NOCLDWAIT: u64 = SAFlags::NOCLDWAIT.bits();
pub const SA_SIGINFO: u64 = SAFlags::SIGINFO.bits();
pub const SA_ONSTACK: u64 = SAFlags::ONSTACK.bits();
pub const SA_RESTART: u64 = SAFlags::RESTART.bits();
pub const SA_NODEFER: u64 = SAFlags::NODEFER.bits();
pub const SA_RESETHAND: u64 = SAFlags::RESETHAND.bits();
pub const SA_RESTORER: u64 = SAFlags::RESTORER.bits();

// ---------------------------------------------------------------------------
// SigAction
// ---------------------------------------------------------------------------

pub const SIG_DFL: usize = 0;
pub const SIG_IGN: usize = 1;

#[derive(Debug, Clone, Copy)]
pub struct SigAction {
    pub handler: usize, // SIG_DFL, SIG_IGN, or function pointer
    pub flags: u64,
    pub restorer: usize, // SA_RESTORER trampoline (unused, we use sigcode page)
    pub mask: u64,       // signals blocked during handler execution
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
    pub pending: AtomicSigSet,
    /// Blocked signals bitmap.
    pub blocked: AtomicSigSet,
    /// Sigaltstack: (ss_sp, ss_size, ss_flags). 0 = not set.
    pub altstack: Mutex<(usize, usize, u32)>,
}

impl SignalState {
    pub fn new() -> Self {
        Self {
            actions: Mutex::new([SigAction::default(); MAX_SIG as usize]),
            pending: AtomicSigSet::new(SigSet::empty()),
            blocked: AtomicSigSet::new(SigSet::empty()),
            altstack: Mutex::new((0, 0, 0)),
        }
    }

    /// Post a signal (atomic, lock-free).
    pub fn post_signal(&self, sig: u8) {
        klog!(
            signal,
            debug,
            "post_signal sig={}",
            Signal::new_unchecked(sig)
        );
        self.pending
            .fetch_add(Signal::new_unchecked(sig), Ordering::Release);
    }

    /// Check if there are unmasked pending signals.
    #[allow(unused)]
    pub fn has_unmasked_pending(&self) -> bool {
        let pending = self.pending.load(Ordering::Acquire);
        let blocked = self.blocked.load(Ordering::Relaxed);
        let mut unblockable = SigSet::empty();
        unblockable
            .add(Signal::new_unchecked(SIGKILL))
            .add(Signal::new_unchecked(SIGSTOP));

        let deliverable = pending.difference(blocked.difference(unblockable));
        !deliverable.is_empty()
    }

    /// Check if there are unmasked pending signals that would actually
    /// interrupt (not default-ignored like SIGCHLD with SIG_DFL).
    pub fn has_actionable_pending(&self) -> bool {
        let pending = self.pending.load(Ordering::Acquire);
        let blocked = self.blocked.load(Ordering::Relaxed);
        let mut unblockable = SigSet::empty();
        unblockable
            .add(Signal::new_unchecked(SIGKILL))
            .add(Signal::new_unchecked(SIGSTOP));

        let deliverable = pending.difference(blocked.difference(unblockable));
        if deliverable.is_empty() {
            return false;
        }

        // Check each deliverable signal: skip those with default-ignore disposition
        let actions = self.actions.lock();
        let mut bits = deliverable.as_u64();
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
                SIG_IGN => {}     // Explicitly ignored
                _ => return true, // User handler installed
            }
            bits &= !(1u64 << bit);
        }
        false
    }

    /// Check if the given signal's action has SA_RESTART set.
    pub fn is_restart(&self, sig: u8) -> bool {
        if !Signal::is_legit(sig) {
            return false;
        }
        let actions = self.actions.lock();
        actions[(sig - 1) as usize].flags & SAFlags::RESTART.bits() != 0
    }

    /// Dequeue the highest-priority unmasked pending signal. Returns None if none.
    pub fn dequeue_signal(&self) -> Option<u8> {
        loop {
            let pending = self.pending.load(Ordering::Acquire);
            let blocked = self.blocked.load(Ordering::Relaxed);

            let mut unblockable = SigSet::empty();
            unblockable
                .add(Signal::new_unchecked(SIGKILL))
                .add(Signal::new_unchecked(SIGSTOP));
            let deliverable = pending.difference(blocked.difference(unblockable));

            if deliverable.is_empty() {
                return None;
            }

            // Pick lowest-numbered signal
            let bit = deliverable.as_u64().trailing_zeros() as u8;
            let sig = Signal::new_unchecked(bit + 1);

            // Atomically clear the bit
            let old = self.pending.fetch_remove(sig, Ordering::AcqRel);
            if old.contains(sig) {
                return Some(sig.as_u8());
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
const _SYS_RT_SIGRETURN: usize = 139;

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

pub const SIGFRAME_SIZE: usize = core::mem::size_of::<SigFrame>();

// ---------------------------------------------------------------------------
// sendsig: build sigframe on user stack, redirect to handler
// ---------------------------------------------------------------------------

/// Linux siginfo_t size on rv64 (128 bytes).
const SIGINFO_SIZE: usize = 128;

/// Build a minimal siginfo_t in a 128-byte buffer.
/// Fields: si_signo (i32 @ 0), si_errno (i32 @ 4), si_code (i32 @ 8).
fn build_siginfo(sig: u8) -> [u8; SIGINFO_SIZE] {
    let mut buf = [0u8; SIGINFO_SIZE];
    // si_signo at offset 0
    let signo = sig as i32;
    buf[0..4].copy_from_slice(&signo.to_le_bytes());
    // si_errno at offset 4 = 0
    // si_code at offset 8: SI_KERNEL=128 (generic)
    let si_code: i32 = 128;
    buf[8..12].copy_from_slice(&si_code.to_le_bytes());
    buf
}

/// Build a signal frame on the user stack and redirect execution to the handler.
/// Returns Ok(()) on success, Err(()) if the user stack is trashed (caller should kill).
pub fn sendsig(task: &Arc<Task>, sig: u8, action: &SigAction) -> Result<(), ()> {
    klog!(
        signal,
        debug,
        "sendsig pid={} sig={} handler={:#x}",
        task.pid,
        Signal(sig),
        action.handler
    );
    let mut tf = task.trap_frame.lock();
    let sig_state = &task.signals;

    klog!(
        signal,
        error,
        "sendsig pid={} saving sepc={:#x} a0={:#x} sp={:#x}",
        task.pid,
        tf.sepc,
        tf.x[10],
        tf.x[2]
    );

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

    // If SA_SIGINFO, allocate siginfo_t on the user stack first
    let siginfo_va = if action.flags & SA_SIGINFO != 0 {
        sp = (sp - SIGINFO_SIZE) & !0xF;
        let si = build_siginfo(sig);
        let ok = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(sp as *mut u8, si.as_ptr(), SIGINFO_SIZE)
        };
        if ok != 0 {
            return Err(());
        }
        sp // pointer to siginfo_t
    } else {
        0 // NULL when SA_SIGINFO not set
    };

    // Align SP down and make room for SigFrame
    sp = (sp - SIGFRAME_SIZE) & !0xF; // 16-byte aligned

    // Build the sigframe in kernel memory
    let frame = SigFrame {
        saved_tf: *tf,
        signo: sig as u32,
        _pad: 0,
        saved_mask: sig_state.blocked.load(Ordering::Relaxed).as_u64(),
    };

    // Update blocked mask (add this signal's mask, plus the signal itself
    // unless SA_NOMASK/SA_NODEFER is set - but we don't implement those yet,
    // so standard POSIX says the signal is blocked while handled).
    let mut new_blocked = SigSet::from_u64(frame.saved_mask | action.mask);
    new_blocked.add(Signal::new_unchecked(sig));
    sig_state.blocked.store(new_blocked, Ordering::Relaxed);
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

    klog!(
        signal,
        error,
        "sendsig pid={} sigframe at {:#x}..{:#x} (size={})",
        task.pid,
        sp,
        sp + SIGFRAME_SIZE,
        SIGFRAME_SIZE
    );

    // Block signals specified in sa_mask + the signal itself during handler
    let block_mask = action.mask | Signal::new_unchecked(sig).as_bit();
    sig_state
        .blocked
        .fetch_union(SigSet::from_u64(block_mask), Ordering::Release);

    // Redirect trap frame to handler
    //TODO: manually controlling trap frame is acceptable. make it a method.
    tf.sepc = action.handler;
    tf.x[10] = sig as usize; // a0 = signo
    tf.x[11] = siginfo_va; // a1 = siginfo (valid ptr if SA_SIGINFO, else NULL)
    tf.x[12] = sp; // a2 = ucontext (pointer to sigframe)
    tf.x[2] = sp; // sp = sigframe
    tf.x[1] = SIGCODE_VA; // ra = sigreturn trampoline

    // Sanitize sstatus: SPP=0 (user mode), SPIE=1, FS>=Initial
    tf.sstatus = (tf.sstatus & !(1 << 8)) | (1 << 5); // clear SPP, set SPIE
    if tf.sstatus & (3 << 13) == 0 {
        tf.sstatus |= 1 << 13; // FS=Initial if Off
    }

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
    klog!(
        signal,
        debug,
        "check_pending pid={} sig={}",
        task.pid,
        Signal(sig)
    );

    // SIGKILL: always fatal, no handler
    if sig == SIGKILL {
        klog!(
            signal,
            debug,
            "check_pending pid={} SIGKILL -> fatal",
            task.pid
        );
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
                SigDefault::Terminate => {
                    klog!(
                        signal,
                        debug,
                        "check_pending pid={} sig={} SIG_DFL -> Terminate",
                        task.pid,
                        Signal(sig)
                    );
                    Err(sig)
                }
                SigDefault::Ignore => Ok(false),
                SigDefault::Stop => Ok(false), // simplified
                SigDefault::Continue => Ok(false),
            }
        }
        SIG_IGN => Ok(false),
        _handler => {
            klog!(
                signal,
                debug,
                "check_pending pid={} sig={} -> user handler {:#x}",
                task.pid,
                Signal(sig),
                _handler
            );
            // Deliver to user handler via sendsig
            match sendsig(task, sig, &action) {
                Ok(()) => Ok(true),
                Err(()) => {
                    klog!(
                        signal,
                        debug,
                        "check_pending pid={} sendsig FAILED -> SIGILL",
                        task.pid
                    );
                    // sendsig failed (bad user stack) — kill with SIGILL
                    Err(SIGILL)
                }
            }
        }
    }
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

pub(crate) fn find_task_by_pid(pid: u32) -> Option<Arc<Task>> {
    let registry = TASK_REGISTRY.lock();
    registry.iter().find(|t| t.pid == pid).cloned()
}

pub(crate) fn for_each_task(mut f: impl FnMut(&Arc<Task>)) {
    let registry = TASK_REGISTRY.lock();
    for t in registry.iter() {
        f(t);
    }
}

pub(crate) fn kill_pgrp(pgid: u32, sig: u8) {
    if sig == 0 {
        return;
    }
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
// Map sigcode page into a process's address space
// ---------------------------------------------------------------------------

/// Map the sigcode trampoline page into the given pmap.
/// Called during exec (and for init).
pub fn map_sigcode_page(pmap: &mut crate::mm::pmap::Pmap) {
    use crate::mm::pmap::pmap_enter;

    let frame =
        crate::mm::allocator::alloc_raw_frame_sync(crate::mm::allocator::PageRole::SigTrampoline)
            .expect("sigcode page alloc failed");
    let page_data = build_sigcode_page();

    // Write sigcode to the physical frame
    unsafe {
        frame.as_mut_slice().copy_from_slice(&page_data);
    }

    // Map as read-only + user-accessible
    let prot = crate::map_perm!(R, X, U);
    let _ = pmap_enter(pmap, VirtAddr::new(SIGCODE_VA), frame, prot, false);
}
