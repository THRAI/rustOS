use crate::hal_common::{PhysAddr, PAGE_SIZE};

#[cfg(target_arch = "loongarch64")]
const LA64_KERNEL_WIN_BASE: usize = 0x9000_0000_0000_0000;
#[cfg(target_arch = "loongarch64")]
const LA64_KERNEL_WIN_LOW_MASK: usize = 0x0000_0000_FFFF_FFFF;

#[inline]
fn canonical_phys_index(addr: usize) -> usize {
    #[cfg(target_arch = "loongarch64")]
    {
        return (addr & LA64_KERNEL_WIN_LOW_MASK) / PAGE_SIZE;
    }

    #[cfg(not(target_arch = "loongarch64"))]
    {
        addr / PAGE_SIZE
    }
}

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
    DriverDma = 11,
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
            11 => PageRole::DriverDma,
            _ => PageRole::Free, // fallback
        }
    }
}

use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::mm::VmPage;

// Global metadata array pointer and length.
// Initialized during boot with Release stores; read with Acquire loads
// for SMP visibility.
pub(crate) static FRAME_META: AtomicPtr<VmPage> = AtomicPtr::new(core::ptr::null_mut());
pub(crate) static FRAME_META_LEN: AtomicUsize = AtomicUsize::new(0);

pub fn get_frame_meta(phys: PhysAddr) -> Option<&'static VmPage> {
    let pfn = canonical_phys_index(phys.page_align_down().as_usize());
    let len = FRAME_META_LEN.load(Ordering::Acquire);
    let ptr = FRAME_META.load(Ordering::Acquire);
    if pfn < len && !ptr.is_null() {
        // SAFETY: `ptr` was set during boot to a valid array of `len` VmPage
        // entries that lives for the lifetime of the kernel.  All VmPage
        // fields are Atomic*, so a shared reference is sound even under
        // concurrent access from multiple CPUs.
        Some(unsafe { &*ptr.add(pfn) })
    } else {
        None
    }
}

impl VmPage {
    /// Atomically increment the reference count (for TypedFrame compatibility).
    pub fn inc_ref(&self) -> u32 {
        self.refs
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed)
    }

    /// Atomically decrement the reference count (for TypedFrame compatibility).
    /// Returns the old reference count.
    pub fn dec_ref(&self) -> u32 {
        self.refs
            .fetch_sub(1, core::sync::atomic::Ordering::Release)
    }

    /// Get the current reference count.
    pub fn get_ref(&self) -> u32 {
        self.refs.load(core::sync::atomic::Ordering::Acquire)
    }

    /// Set the functional role of the page.
    pub fn set_role(&self, role: PageRole) {
        self.role_flags
            .store(role as u8, core::sync::atomic::Ordering::Relaxed);
    }

    /// Get the current functional role of the page.
    pub fn role(&self) -> PageRole {
        PageRole::from(self.role_flags.load(core::sync::atomic::Ordering::Relaxed))
    }
}
