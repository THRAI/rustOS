//! Move-only handle to a physical page in the FRAME_META array.
//!
//! `PageRef` owns the `VPRC_OBJREF` bit on the page's FRAME_META entry.
//! Dropping a PageRef clears VPRC_OBJREF and frees the physical frame
//! if no pmap wires remain.
//!
//! NOT Clone — exactly one VmObject owns each page at any time (FreeBSD
//! VPRC_OBJREF semantics). Transfer between objects is done by moving
//! the PageRef.

use crate::{hal_common::PhysAddr, mm::vm::VmPage};

/// A move-only reference to a physical page, backed by the FRAME_META array.
pub struct PageRef {
    pa: PhysAddr,
}

impl PageRef {
    /// Create a new PageRef, claiming object ownership of the page.
    ///
    /// The caller must have just allocated this frame (refcount == 1 from
    /// finalize_alloc). This function is the transfer of ownership from
    /// the allocator to the VmObject.
    pub fn new(pa: PhysAddr) -> Self {
        // The frame was allocated with refs=1 by finalize_alloc.
        // That refcount serves as our OBJREF — no additional increment needed.
        Self { pa }
    }

    /// Get the physical address.
    #[inline]
    pub fn phys(&self) -> PhysAddr {
        self.pa
    }

    /// Resolve to the FRAME_META entry.
    ///
    /// Returns `&'static VmPage` — the FRAME_META array is allocated at boot
    /// and entries are never freed.
    #[inline]
    pub fn meta(&self) -> &'static VmPage {
        // SAFETY: PageRef can only be created for valid allocated frames.
        // FRAME_META entries are 'static.
        crate::mm::get_frame_meta(self.pa)
            .expect("PageRef: invalid PhysAddr — not in FRAME_META range")
    }
}

impl Drop for PageRef {
    fn drop(&mut self) {
        // Decrement refcount. If it hits 0, free the frame.
        // This replaces VmObject::release_unmapped_page.
        if let Some(meta) = crate::mm::get_frame_meta(self.pa) {
            let old_ref = meta.dec_ref();
            debug_assert!(
                old_ref > 0,
                "PageRef drop: refcount was already 0 for pa={:#x}",
                self.pa.as_usize()
            );
            if old_ref == 1 {
                // We were the last reference. Free the physical frame.
                // SAFETY: Acquire fence ensures all prior writes (from other CPUs that
                // decremented refs) are visible before we return the frame to the pool.
                core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);
                meta.set_role(crate::mm::PageRole::Free);
                crate::mm::free_raw_frame(self.pa);
            }
        }
        // If get_frame_meta returns None (e.g. in host-side tests where
        // FRAME_META is not initialised), silently skip — test addresses
        // won't map to real entries and the "leak" is harmless.
    }
}

// PageRef is intentionally NOT Clone — move-only.

// Debug impl for logging:
impl core::fmt::Debug for PageRef {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PageRef({:#x})", self.pa.as_usize())
    }
}
