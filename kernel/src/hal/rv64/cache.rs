//! rv64 cache operations for DMA correctness.
//!
//! QEMU virt has coherent DMA, so fence instructions suffice.
//! Real hardware with Zicbom would use cbo.clean/cbo.inval instead.

/// Write-back data cache for address range.
/// On QEMU virt (coherent), a store fence is sufficient.
/// TODO: Use cbo.clean on real hardware with Zicbom extension.
#[inline]
pub fn dcache_wb(_addr: usize, _len: usize) {
    unsafe {
        core::arch::asm!("fence w, w");
    }
}

/// Invalidate data cache for address range.
/// On QEMU virt (coherent), a load fence is sufficient.
/// TODO: Use cbo.inval on real hardware with Zicbom extension.
#[inline]
pub fn dcache_inv(_addr: usize, _len: usize) {
    unsafe {
        core::arch::asm!("fence r, r");
    }
}

/// Full I/O fence for MMIO ordering.
#[inline]
pub fn fence_io() {
    unsafe {
        core::arch::asm!("fence iorw, iorw");
    }
}
