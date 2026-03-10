//! Machine-independent trap module.
//!
//! Re-exports arch-specific trap init so other kernel code calls `trap::init()`.

use crate::hal;

/// Initialize trap infrastructure for the current architecture.
pub fn init() {
    hal::trap::init();
}
