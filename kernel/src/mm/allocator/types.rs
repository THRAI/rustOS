use core::marker::PhantomData;
use core::sync::atomic::{AtomicU16, Ordering};
use hal_common::addr::{PhysAddr, PAGE_SIZE};

/// Compact page role, fitting in a single u8.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageRole {
    Free = 0,
    PteL2 = 1,
    PteL1 = 2,
    PteL0 = 3,
    UserAnon = 4,
    UserExec = 5,
    UserStack = 6,
    FileCache = 7,
    KernelStack = 8,
    PipeBuffer = 9,
    SigTrampoline = 10,
}

impl From<u8> for PageRole {
    fn from(val: u8) -> Self {
        match val {
            0 => PageRole::Free,
            1 => PageRole::PteL2,
            2 => PageRole::PteL1,
            3 => PageRole::PteL0,
            4 => PageRole::UserAnon,
            5 => PageRole::UserExec,
            6 => PageRole::UserStack,
            7 => PageRole::FileCache,
            8 => PageRole::KernelStack,
            9 => PageRole::PipeBuffer,
            10 => PageRole::SigTrampoline,
            _ => PageRole::Free, // fallback
        }
    }

}

struct Page<Role> {
    raw: PhysAddr,
    role: PhantomData<Role>,
}

/// SMP-safe metadata for a single physical frame.
#[repr(C)]
pub struct FrameMeta {
    /// Packed: role (u8) + flags (u8)
    /// We use an AtomicU16 here so we can update role safely.
    pub role_flags: AtomicU16,
    /// Reference count for COW / shared mappings
    pub refcount: AtomicU16,
}

impl FrameMeta {
    pub const fn new() -> Self {
        Self {
            role_flags: AtomicU16::new(0),
            refcount: AtomicU16::new(0),
        }
    }

    pub fn role(&self) -> PageRole {
        let val = self.role_flags.load(Ordering::Relaxed);
        PageRole::from((val & 0xFF) as u8)
    }

    pub fn set_role(&self, role: PageRole) {
        // We only overwrite the lower 8 bits (role) and keep the upper 8 bits (flags).
        // Since flags aren't heavily used right now, a simple fetch_update works.
        self.role_flags
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |val| {
                Some((val & 0xFF00) | (role as u8 as u16))
            })
            .unwrap();
    }

    pub fn inc_ref(&self) -> u16 {
        self.refcount.fetch_add(1, Ordering::Relaxed)
    }

    pub fn dec_ref(&self) -> u16 {
        self.refcount.fetch_sub(1, Ordering::Release)
    }

    pub fn get_ref(&self) -> u16 {
        self.refcount.load(Ordering::Acquire)
    }
}

// Marker types (zero-size, compile-time only)
#[derive(Debug)]
pub struct UserAnon;
#[derive(Debug)]
pub struct PteL0;
#[derive(Debug)]
pub struct PteL1;
#[derive(Debug)]
pub struct PteL2;
#[derive(Debug)]
#[allow(unused)]
pub struct KernelStack;
#[derive(Debug)]
#[allow(unused)]
pub struct UserStack;
#[derive(Debug)]
pub struct FileCache;
#[derive(Debug)]
#[allow(unused)]
pub struct UserExec;
#[derive(Debug)]
#[allow(unused)]
pub struct PipeBuffer;
#[derive(Debug)]
#[allow(unused)]
pub struct SigTrampoline;
#[derive(Debug)]
#[allow(unused)]
pub struct KernelMisc;

/// A frame that knows its purpose. Cannot be misused.
#[repr(transparent)]
#[derive(Debug)]
pub struct TypedFrame<Role> {
    pub phys: PhysAddr,
    pub(crate) _marker: PhantomData<Role>,
}

impl<Role> Drop for TypedFrame<Role> {
    fn drop(&mut self) {
        if let Some(meta) = get_frame_meta(self.phys) {
            // Note: dec_ref returns the *previous* value.
            let old_ref = meta.dec_ref();
            if old_ref == 1 {
                // Was 1, now 0. Return to buddy allocator.
                crate::mm::allocator::free_raw_frame(self.phys);
            }
        } else {
            // In unit tests or early boot before FRAME_META is initialized,
            // we just fall back to freeing it directly if possible.
            // For safety in this refactor, we just call the underlying free.
            crate::mm::allocator::free_raw_frame(self.phys);
        }
    }
}

// Data pages can be cloned (COW, Shared Memory).
// PteLx and other infrastructure pages CANNOT be cloned.

impl Clone for TypedFrame<UserAnon> {
    fn clone(&self) -> Self {
        if let Some(meta) = get_frame_meta(self.phys) {
            meta.inc_ref();
        }
        Self {
            phys: self.phys,
            _marker: PhantomData,
        }
    }
}

impl Clone for TypedFrame<FileCache> {
    fn clone(&self) -> Self {
        if let Some(meta) = get_frame_meta(self.phys) {
            meta.inc_ref();
        }
        Self {
            phys: self.phys,
            _marker: PhantomData,
        }
    }
}

impl<Role> TypedFrame<Role> {
    /// Get the raw physical address.
    #[inline]
    pub fn phys(&self) -> PhysAddr {
        self.phys
    }

    /// Access the underlying page as a byte slice.
    #[inline]
    pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
        unsafe { &*(self.phys.as_usize() as *const [u8; PAGE_SIZE]) }
    }

    /// Access the underlying page as a mutable byte slice.
    #[inline]
    pub fn as_bytes_mut(&mut self) -> &mut [u8; PAGE_SIZE] {
        unsafe { &mut *(self.phys.as_usize() as *mut [u8; PAGE_SIZE]) }
    }
}

// Implement as_ptes for page table frame types
macro_rules! impl_pte_accessors {
    ($role:ty) => {
        impl TypedFrame<$role> {
            /// Access the underlying page as a slice of 512 u64 page table entries.
            #[inline]
            pub fn as_ptes(&self) -> &[u64; 512] {
                unsafe { &*(self.phys.as_usize() as *const [u64; 512]) }
            }

            /// Access the underlying page as a mutable slice of 512 u64 page table entries.
            #[inline]
            pub fn as_ptes_mut(&mut self) -> &mut [u64; 512] {
                unsafe { &mut *(self.phys.as_usize() as *mut [u64; 512]) }
            }
        }
    };
}

impl_pte_accessors!(PteL0);
impl_pte_accessors!(PteL1);
impl_pte_accessors!(PteL2);

// Global metadata array pointer and length
// Initialized during boot
pub(crate) static mut FRAME_META: *mut FrameMeta = core::ptr::null_mut();
pub(crate) static mut FRAME_META_LEN: usize = 0;

pub fn get_frame_meta(phys: PhysAddr) -> Option<&'static FrameMeta> {
    let pfn = phys.as_usize() / PAGE_SIZE;
    unsafe {
        if pfn < FRAME_META_LEN {
            Some(&*FRAME_META.add(pfn))
        } else {
            None
        }
    }
}
