/// Runtime feature flags for the BSD-Async Rust OS Kernel.
///
/// Compile-time features (`cfg(feature = "...")`) control what code is included,
/// but runtime feature flags allow toggling behavior without recompilation.
/// This is useful for:
/// - Gradual rollout of new syscall implementations
/// - A/B testing kernel scheduling policies
/// - Disabling unstable features in production builds
///
/// # Usage
/// ```rust
/// use crate::feature_flags;
///
/// if feature_flags::is_enabled(Flag::NewSchedPolicy) {
///     // new scheduling path
/// } else {
///     // legacy path
/// }
/// ```
use core::sync::atomic::{AtomicU32, Ordering};

/// Each bit in the flags word represents one runtime feature toggle.
static FLAGS: AtomicU32 = AtomicU32::new(0);

/// Known runtime feature flags.
///
/// Add new flags here as needed. Each flag occupies one bit (max 32 flags).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    /// Use the new CoW-optimised fork path instead of the deep-copy fallback.
    CowFork = 1 << 0,
    /// Enable the async I/O path for VFS read/write.
    AsyncVfsIo = 1 << 1,
    /// Enable verbose syscall tracing at runtime (independent of log features).
    SyscallTrace = 1 << 2,
    /// Use the new signal delivery mechanism.
    NewSignalDelivery = 1 << 3,
    /// Enable experimental mmap optimisations.
    MmapOptimised = 1 << 4,
}

/// Check whether a runtime feature flag is currently enabled.
#[inline]
pub fn is_enabled(flag: Flag) -> bool {
    FLAGS.load(Ordering::Relaxed) & (flag as u32) != 0
}

/// Enable a runtime feature flag.
#[inline]
pub fn enable(flag: Flag) {
    FLAGS.fetch_or(flag as u32, Ordering::Relaxed);
}

/// Disable a runtime feature flag.
#[inline]
pub fn disable(flag: Flag) {
    FLAGS.fetch_and(!(flag as u32), Ordering::Relaxed);
}

/// Enable a set of default flags during early boot.
/// Called from `kernel_main` before any user processes are spawned.
pub fn init_defaults() {
    // CowFork is stable and on by default
    enable(Flag::CowFork);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enable_disable() {
        // Reset
        FLAGS.store(0, Ordering::Relaxed);
        assert!(!is_enabled(Flag::CowFork));

        enable(Flag::CowFork);
        assert!(is_enabled(Flag::CowFork));
        assert!(!is_enabled(Flag::AsyncVfsIo));

        enable(Flag::AsyncVfsIo);
        assert!(is_enabled(Flag::CowFork));
        assert!(is_enabled(Flag::AsyncVfsIo));

        disable(Flag::CowFork);
        assert!(!is_enabled(Flag::CowFork));
        assert!(is_enabled(Flag::AsyncVfsIo));
    }

    #[test]
    fn test_init_defaults() {
        FLAGS.store(0, Ordering::Relaxed);
        init_defaults();
        assert!(is_enabled(Flag::CowFork));
        assert!(!is_enabled(Flag::AsyncVfsIo));
    }
}
