//! Physical page metadata and state machine (`VmPage`).
//!
//! Handles cross-thread sync (`exBusy`/`sBusy`), object linkage, and hardware
//! referencing. Aligned to FreeBSD's definition.

use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU8, Ordering};

use bitflags::bitflags;

use crate::{
    hal_common::PhysAddr,
    mm::{
        vm::{register_waker, remove_waker, wake_all, VmObject},
        PageRole,
    },
};

bitflags! {
    /// Readers-Writer Busy Lock state.
    pub struct BusyState: u32 {
        /// exBusy: the page is in flux (I/O, being destroyed, changing identity).
        const EXCLUSIVE   = 1 << 31;
        /// Sleepers are waiting on this page.
        const WANTED      = 1 << 30;
        /// sBusy: readers mask.
        const SHARED_MASK = 0x3FFF_FFFF;
    }
}

/// A physical page's metadata structure. One per physical frame in the system.
/// Array is pre-allocated at boot.
#[repr(C)]
#[derive(Debug)]
pub struct VmPage {
    /// Which object owns this page (if any).
    pub object: AtomicPtr<VmObject>,
    /// Logical offset within the object.
    pub pindex: u64,
    /// The physical address this struct represents.
    pub phys_addr: PhysAddr,

    /// State machine lock (exBusy / sBusy).
    busy_state: AtomicU32,

    /// Replaces the old `FrameMeta` reference count.
    /// Also handles VPRC_BLOCKED logic.
    pub refs: AtomicU32,

    /// Role of the page (from `PageRole` enum).
    pub role_flags: AtomicU8,
    /// 1 if the page contains valid data, 0 if data is pending/I/O.
    pub valid: AtomicU8,
    /// Has the page been modified since the last page-out?
    pub dirty: AtomicU8,
}

impl VmPage {
    pub const fn new() -> Self {
        Self {
            object: AtomicPtr::new(core::ptr::null_mut()),
            pindex: 0,
            phys_addr: PhysAddr::new(0),
            busy_state: AtomicU32::new(0),
            refs: AtomicU32::new(0),
            role_flags: AtomicU8::new(PageRole::Free as u8),
            valid: AtomicU8::new(0),
            dirty: AtomicU8::new(0),
        }
    }

    /// Create a dummy page for tests with a specific physical address.
    pub fn new_test(pa: PhysAddr) -> Self {
        let mut p = Self::new();
        p.phys_addr = pa;
        p.valid.store(1, Ordering::Relaxed);
        p
    }

    /// Create a dummy cached page for tests.
    pub fn new_cached_test(pa: PhysAddr) -> Self {
        let p = Self::new_test(pa);
        p.dirty.store(0, Ordering::Relaxed);
        p
    }

    // ------------------------------------------------------------------------
    // State Machine: exBusy
    // ------------------------------------------------------------------------

    /// Check if the page is exclusively busied.
    pub fn is_exclusive_busied(&self) -> bool {
        self.busy_state.load(Ordering::Relaxed) & BusyState::EXCLUSIVE.bits() != 0
    }

    /// Try to acquire the exclusive lock (exBusy).
    pub fn try_acquire_exclusive(&self) -> bool {
        let mut current = self.busy_state.load(Ordering::Relaxed);
        loop {
            // Cannot acquire if there's any active lock (exclusive or shared).
            if current & (BusyState::EXCLUSIVE.bits() | BusyState::SHARED_MASK.bits()) != 0 {
                return false;
            }

            match self.busy_state.compare_exchange_weak(
                current,
                current | BusyState::EXCLUSIVE.bits(),
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(val) => current = val,
            }
        }
    }

    /// Block (yield inside an async context) until `exBusy` is acquired.
    pub async fn acquire_exclusive_blocking(&self) {
        loop {
            if self.try_acquire_exclusive() {
                return;
            }
            self.wait_for_exclusive_unlock().await;
        }
    }

    /// Wait (async yield) for the `exBusy` lock to be released.
    pub async fn wait_for_exclusive_unlock(&self) {
        use core::task::Poll;
        let token = self as *const _ as usize;

        // Register a waker and yield
        let mut yielded = false;
        core::future::poll_fn(|cx| {
            if !yielded {
                register_waker(token, cx.waker().clone());
                // Mark WANTED so the holder knows to call wake_all
                self.busy_state
                    .fetch_or(BusyState::WANTED.bits(), Ordering::Relaxed);
                yielded = true;
                Poll::Pending
            } else {
                remove_waker(token, cx.waker());
                Poll::Ready(())
            }
        })
        .await;
    }

    /// Release the `exBusy` lock and wake sleepers.
    pub fn release_exclusive(&self) {
        let old = self.busy_state.fetch_and(
            !(BusyState::EXCLUSIVE.bits() | BusyState::WANTED.bits()),
            Ordering::Release,
        );

        debug_assert!(
            (old & BusyState::EXCLUSIVE.bits()) != 0,
            "Released non-exclusive page"
        );

        if old & BusyState::WANTED.bits() != 0 {
            wake_all(self as *const _ as usize);
        }
    }

    /// Downgrade `exBusy` directly to `sBusy` (Atomic lock downgrade).
    pub fn downgrade_exclusive_to_shared(&self) {
        let mut current = self.busy_state.load(Ordering::Relaxed);
        loop {
            debug_assert!((current & BusyState::EXCLUSIVE.bits()) != 0);

            let mut new_state = current & !BusyState::EXCLUSIVE.bits();
            new_state += 1; // 1 reader

            let wake_needed = (new_state & BusyState::WANTED.bits()) != 0;
            if wake_needed {
                new_state &= !BusyState::WANTED.bits();
            }

            match self.busy_state.compare_exchange_weak(
                current,
                new_state,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    if wake_needed {
                        wake_all(self as *const _ as usize);
                    }
                    return;
                },
                Err(val) => current = val,
            }
        }
    }

    // ------------------------------------------------------------------------
    // State Machine: sBusy
    // ------------------------------------------------------------------------

    pub fn is_shared_busied(&self) -> bool {
        self.busy_state.load(Ordering::Relaxed) & BusyState::SHARED_MASK.bits() != 0
    }

    pub fn try_acquire_steady_state(&self) -> bool {
        let mut current = self.busy_state.load(Ordering::Relaxed);
        loop {
            if current & BusyState::EXCLUSIVE.bits() != 0 {
                return false;
            }

            match self.busy_state.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(val) => current = val,
            }
        }
    }

    pub fn release_steady_state(&self) {
        let prev = self.busy_state.fetch_sub(1, Ordering::Release);
        debug_assert!(
            (prev & BusyState::SHARED_MASK.bits()) > 0,
            "sBusy underflow"
        );
    }

    // ------------------------------------------------------------------------
    // Pager Aux Methods
    // ------------------------------------------------------------------------

    pub fn is_valid(&self) -> bool {
        self.valid.load(Ordering::Relaxed) != 0
    }

    pub fn set_valid(&self) {
        self.valid.store(1, Ordering::Relaxed);
    }

    pub fn clear_valid(&self) {
        self.valid.store(0, Ordering::Relaxed);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed) != 0
    }

    pub fn set_dirty(&self) {
        self.dirty.store(1, Ordering::Relaxed);
    }

    pub fn clear_dirty_all(&self) {
        self.dirty.store(0, Ordering::Relaxed);
    }

    /// Access physical address as a mutable pointer for I/O / bzero.
    pub unsafe fn phys_addr_as_mut_ptr(&self) -> *mut u8 {
        self.phys_addr.into_kernel_vaddr().as_mut_ptr()
    }

    /// Get the raw physical address.
    #[inline]
    pub fn phys(&self) -> PhysAddr {
        self.phys_addr
    }

    /// Access the underlying page as a byte slice.
    #[inline]
    pub fn as_page_slice(&self) -> &[u8; crate::hal_common::PAGE_SIZE] {
        unsafe {
            &*(self.phys_addr.into_kernel_vaddr().as_ptr()
                as *const [u8; crate::hal_common::PAGE_SIZE])
        }
    }

    /// Access the underlying page as a mutable byte slice.
    #[inline]
    pub fn as_page_slice_mut(&mut self) -> &mut [u8; crate::hal_common::PAGE_SIZE] {
        unsafe {
            &mut *(self.phys_addr.into_kernel_vaddr().as_mut_ptr()
                as *mut [u8; crate::hal_common::PAGE_SIZE])
        }
    }

    /// Access the underlying page as a slice of 512 u64 page table entries.
    #[inline]
    pub fn as_ptes(&self) -> &[u64; 512] {
        unsafe { &*(self.phys_addr.into_kernel_vaddr().as_ptr() as *const [u64; 512]) }
    }

    /// Access the underlying page as a mutable slice of 512 u64 page table entries.
    #[inline]
    pub fn as_ptes_mut(&mut self) -> &mut [u64; 512] {
        unsafe { &mut *(self.phys_addr.into_kernel_vaddr().as_mut_ptr() as *mut [u64; 512]) }
    }

    // ------------------------------------------------------------------------
    // Teardown / Hardware Maps (VPRC equivalent)
    // ------------------------------------------------------------------------
    pub const VPRC_BLOCKED: u32 = 1 << 31;
    pub const VPRC_OBJREF: u32 = 1 << 30;
    pub const VPRC_WIRE_MASK: u32 = !(Self::VPRC_BLOCKED | Self::VPRC_OBJREF);

    pub fn teardown_from_object(&self) {
        debug_assert!(self.is_exclusive_busied());
        let old = self
            .refs
            .fetch_xor(Self::VPRC_BLOCKED | Self::VPRC_OBJREF, Ordering::Release);
        debug_assert!((old & Self::VPRC_OBJREF) != 0, "Double teardown");

        if (old & Self::VPRC_WIRE_MASK) == 0 {
            core::sync::atomic::fence(Ordering::Acquire);
            self.free_to_allocator();
        } else {
            self.release_exclusive();
        }
    }

    pub fn try_wire_mapped(&self) -> bool {
        let mut old = self.refs.load(Ordering::Relaxed);
        loop {
            if old & Self::VPRC_BLOCKED != 0 {
                return false;
            }
            match self.refs.compare_exchange_weak(
                old,
                old + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(val) => old = val,
            }
        }
    }

    pub fn unwire_mapped(&self) {
        let old = self.refs.fetch_sub(1, Ordering::Release);
        let count = old & Self::VPRC_WIRE_MASK;
        debug_assert!(count > 0, "PTE underflow");

        if count == 1 && (old & Self::VPRC_OBJREF) == 0 {
            core::sync::atomic::fence(Ordering::Acquire);
            self.free_to_allocator();
        }
    }

    fn free_to_allocator(&self) {
        crate::mm::free_raw_frame(self.phys_addr);
    }
}

// ---------------------------------------------------------------------------
// RAII Busy Lock Guards
// ---------------------------------------------------------------------------

/// RAII guard for a shared busy lock (sBusy) on a `VmPage`.
///
/// Holds a `PhysAddr` that resolves to `&'static VmPage` via FRAME_META.
/// Automatically calls `release_steady_state()` on drop, preventing
/// forgotten lock releases that would deadlock future page faults.
pub struct SharedBusyGuard {
    pa: PhysAddr,
}

impl SharedBusyGuard {
    /// Try to acquire the shared busy lock on the page at `pa`.
    /// Returns `None` if the page is exclusively busied or PA is invalid.
    pub fn try_new(pa: PhysAddr) -> Option<Self> {
        let meta = crate::mm::get_frame_meta(pa)?;
        if meta.try_acquire_steady_state() {
            Some(Self { pa })
        } else {
            None
        }
    }

    /// Get the physical address.
    pub fn phys(&self) -> PhysAddr {
        self.pa
    }

    /// Resolve to the FRAME_META entry.
    pub fn meta(&self) -> &'static VmPage {
        crate::mm::get_frame_meta(self.pa).expect("SharedBusyGuard: invalid PA")
    }
}

impl Drop for SharedBusyGuard {
    fn drop(&mut self) {
        if let Some(meta) = crate::mm::get_frame_meta(self.pa) {
            meta.release_steady_state();
        }
    }
}

/// RAII guard for an exclusive busy lock (exBusy) on a `VmPage`.
///
/// Holds a `PhysAddr` that resolves to `&'static VmPage` via FRAME_META.
/// Automatically calls `release_exclusive()` on drop, preventing
/// forgotten lock releases that would deadlock future page faults.
pub struct ExclusiveBusyGuard {
    pa: PhysAddr,
}

impl ExclusiveBusyGuard {
    /// Try to acquire the exclusive busy lock on the page at `pa`.
    /// Returns `None` if the page has any active busy lock (shared or exclusive)
    /// or PA is invalid.
    pub fn try_new(pa: PhysAddr) -> Option<Self> {
        let meta = crate::mm::get_frame_meta(pa)?;
        if meta.try_acquire_exclusive() {
            Some(Self { pa })
        } else {
            None
        }
    }

    /// Get the physical address.
    pub fn phys(&self) -> PhysAddr {
        self.pa
    }

    /// Resolve to the FRAME_META entry.
    pub fn meta(&self) -> &'static VmPage {
        crate::mm::get_frame_meta(self.pa).expect("ExclusiveBusyGuard: invalid PA")
    }

    /// Downgrade this exclusive lock to a shared lock.
    ///
    /// Consumes `self` (the exclusive guard) and returns a `SharedBusyGuard`.
    pub fn downgrade(self) -> SharedBusyGuard {
        let pa = self.pa;
        let meta = self.meta();
        meta.downgrade_exclusive_to_shared();
        core::mem::forget(self); // prevent Drop from releasing exclusive
        SharedBusyGuard { pa }
    }
}

impl Drop for ExclusiveBusyGuard {
    fn drop(&mut self) {
        if let Some(meta) = crate::mm::get_frame_meta(self.pa) {
            meta.release_exclusive();
        }
    }
}
