//! Runtime lock ordering validator (debug builds only).
//!
//! # Design
//!
//! Each lock carries a compile-time `const LEVEL: u8` parameter encoding
//! its position in the kernel lock ordering hierarchy (see
//! `kernel/docs/LOCK_ORDERING.md`).  At runtime, each hart maintains a
//! bitmask + per-level refcount tracking which levels are currently held.
//!
//! On every `lock()` call, `on_acquire(LEVEL)` checks:
//!
//!     held >> (LEVEL + 1) != 0  →  panic (ordering violation)
//!
//! This enforces monotonically increasing acquisition order while
//! permitting same-level holding (e.g. two Level-4 per-task locks).
//! Per-level refcounts allow order-independent release — the bitmask
//! bit is cleared only when the last lock at that level is dropped.
//!
//! # Why per-hart (not per-task)?
//!
//! Lock ordering must hold globally across all contexts on an SMP system.
//! Consider: Hart A holds L4, gets interrupted, IRQ handler holds L7 and
//! spins on L2 — meanwhile Hart B holds L2 and spins on L4.  That's a
//! real deadlock.  Per-hart tracking catches the L7→L2 inversion because
//! L4 and L7 are both on Hart A's stack.
//!
//! The level split (non-IRQ-safe locks = L0-6, IRQ-safe locks = L7-9)
//! ensures IRQ handlers only acquire locks above any non-IRQ-safe lock,
//! so no false positives occur from context mixing.
//!
//! # Activation
//!
//! Lockdep is dormant until `enable()` is called (after `set_tp()` on
//! the boot hart).  All lock acquisitions before that point — heap init,
//! FDT parse, per-CPU setup — are single-hart with no concurrency, so
//! ordering violations are impossible.
//!
//! # Zero release-build cost
//!
//! The real implementation is behind `#[cfg(all(debug_assertions,
//! target_os = "none"))]`.  Release builds and host-target test builds
//! get empty `#[inline(always)]` stubs that compile away entirely.

// ---------------------------------------------------------------------------
// Active implementation: debug kernel builds (target_os = "none")
// ---------------------------------------------------------------------------
#[cfg(all(debug_assertions, target_os = "none"))]
mod inner {
    use core::sync::atomic::{AtomicBool, Ordering};

    const MAX_HARTS: usize = 8;
    const MAX_LEVELS: usize = 16;

    /// Per-hart lock state: bitmask of held levels + per-level refcount.
    #[derive(Clone, Copy)]
    struct LockState {
        /// Bit `i` is set iff `counts[i] > 0`.
        held: u16,
        /// How many locks at each level are currently held.
        counts: [u8; MAX_LEVELS],
    }

    impl LockState {
        const fn new() -> Self {
            Self {
                held: 0,
                counts: [0; MAX_LEVELS],
            }
        }
    }

    /// Per-hart lock state.  No synchronisation needed — each hart
    /// exclusively accesses its own slot (indexed by `cpu_id`).
    ///
    /// # Safety
    ///
    /// Mutable access is sound because:
    /// 1. Each hart only writes to `STATES[cpu_id]` (its own slot).
    /// 2. `cpu_id` is obtained from `per_cpu::current().cpu_id` which
    ///    reads the `tp` register (per-hart, no sharing).
    /// 3. IRQ handlers on the same hart access the SAME slot, but
    ///    interrupts are serialised (the handler runs to completion
    ///    before returning to the interrupted context), so there is
    ///    no data race.
    static mut STATES: [LockState; MAX_HARTS] = [LockState::new(); MAX_HARTS];

    /// Global enable flag.  `false` until the boot hart calls `enable()`
    /// after `set_tp()` (i.e., once `per_cpu::current()` is safe to call).
    static LOCKDEP_ENABLED: AtomicBool = AtomicBool::new(false);

    /// Activate lockdep.  Call once on the boot hart, immediately after
    /// `executor::set_tp(cpu0)` in `rust_main()`.
    pub fn enable() {
        LOCKDEP_ENABLED.store(true, Ordering::Release);
    }

    /// Record acquisition of a lock at `level`.
    ///
    /// Panics if any lock at a *higher* level is already held on this
    /// hart (ordering violation).  Same-level is allowed.
    #[inline]
    pub fn on_acquire(level: u8) {
        if !LOCKDEP_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let idx = crate::executor::per_cpu::current().cpu_id;
        // SAFETY: `idx` < MAX_HARTS (per_cpu init guarantees), and only
        // this hart accesses STATES[idx].
        let s = unsafe { &mut STATES[idx] };
        // Check: is any level > `level` currently held?
        let above = s.held >> (level as u32 + 1);
        if above != 0 {
            let offending = (level + 1) + above.trailing_zeros() as u8;
            violation_panic(level, offending);
        }
        s.counts[level as usize] += 1;
        s.held |= 1u16 << level;
    }

    /// Record release of a lock at `level`.
    ///
    /// Decrements the per-level refcount; clears the bitmask bit when
    /// the last lock at that level is released.  Order-independent —
    /// supports non-LIFO drop sequences.
    #[inline]
    pub fn on_release(level: u8) {
        if !LOCKDEP_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let idx = crate::executor::per_cpu::current().cpu_id;
        // SAFETY: same as on_acquire.
        let s = unsafe { &mut STATES[idx] };
        debug_assert!(
            s.counts[level as usize] > 0,
            "lockdep: release of unheld level {}",
            level
        );
        s.counts[level as usize] -= 1;
        if s.counts[level as usize] == 0 {
            s.held &= !(1u16 << level);
        }
    }

    /// Print violation message and panic.
    ///
    /// Marked `#[cold]` + `#[inline(never)]` to keep the hot path small.
    /// Uses `panic!` which routes through the kernel panic handler →
    /// `kprintln!` → SBI putchar.  This is safe because `PRINT_LOCK`
    /// (raw `AtomicBool`) is outside the lockdep hierarchy (Level 10).
    #[cold]
    #[inline(never)]
    fn violation_panic(wanted: u8, held: u8) -> ! {
        panic!(
            "lockdep: ordering violation — acquiring L{} while L{} is held\n\
             Hint: locks must be acquired in non-decreasing level order.\n\
             See kernel/docs/LOCK_ORDERING.md for the full hierarchy.",
            wanted, held
        );
    }
}

// ---------------------------------------------------------------------------
// Stub implementation: release builds + host-target tests
// ---------------------------------------------------------------------------
#[cfg(not(all(debug_assertions, target_os = "none")))]
mod inner {
    #[inline(always)]
    pub fn enable() {}
    #[inline(always)]
    pub fn on_acquire(_level: u8) {}
    #[inline(always)]
    pub fn on_release(_level: u8) {}
}

pub use inner::{enable, on_acquire, on_release};
