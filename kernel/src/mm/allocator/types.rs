//! Strongly-typed frame wrappers for the allocator subsystem.
//!
//! `TypedFrame<T>` associates a `PhysAddr` with a usage marker type,
//! providing compile-time tracking of page roles (UserAnon, FileCache).

use hal_common::PhysAddr;
use core::marker::PhantomData;

/// Marker for anonymous user pages.
#[derive(Debug)]
pub struct UserAnon;

/// Marker for file-cache pages.
#[derive(Debug)]
pub struct FileCache;

/// A physical frame tagged with its usage type.
///
/// RAII: dropping a `TypedFrame` frees the underlying physical page
/// via `frame_free` (in production builds; test stubs are no-ops).
#[derive(Debug)]
pub struct TypedFrame<T> {
    pub phys: PhysAddr,
    pub _marker: PhantomData<T>,
}

impl<T> TypedFrame<T> {
    /// Get the physical address of this frame.
    pub fn phys(&self) -> PhysAddr {
        self.phys
    }

    /// Convenience: get a mutable byte slice over the page.
    ///
    /// # Safety
    /// Caller must ensure the physical address is identity-mapped or
    /// accessible through the kernel direct map.
    pub unsafe fn as_bytes_mut(&mut self) -> &mut [u8] {
        self.phys.as_mut_slice()
    }
}

impl<T> Drop for TypedFrame<T> {
    fn drop(&mut self) {
        // In test builds the stub frame_free is a no-op.
        // In production, return the frame to the allocator.
        super::frame_free(self.phys);
    }
}
