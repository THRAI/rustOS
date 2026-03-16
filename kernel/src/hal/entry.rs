//! HAL entry facade.
//!
//! Phase 1 keeps the actual `global_asm!` inclusion in `main.rs` because moving
//! it into a submodule currently triggers a linker section-overlap failure with
//! the existing linker script/debug-section layout. The architectural ownership
//! boundary still lives here conceptually; later phases can revisit the wiring.
