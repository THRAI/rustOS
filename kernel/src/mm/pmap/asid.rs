//! Generation-based ASID allocator.
//!
//! Each pmap stores (asid, generation). On `pmap_activate`, if the pmap's
//! generation is stale, a new ASID is allocated. When the ASID space is
//! exhausted, the generation increments and all CPUs flush their TLBs.

pub fn alloc_asid() -> (u16, u64) {
    #[cfg(target_arch = "riscv64")]
    {
        crate::hal::rv64::paging::asid::alloc_asid()
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::hal::la64::paging::asid::alloc_asid()
    }
}

#[inline]
pub fn global_generation() -> u64 {
    #[cfg(target_arch = "riscv64")]
    {
        crate::hal::rv64::paging::asid::global_generation()
    }
    #[cfg(target_arch = "loongarch64")]
    {
        crate::hal::la64::paging::asid::global_generation()
    }
}
