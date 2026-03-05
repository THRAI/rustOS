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

use crate::mm::vm::page::VmPage;

// Global metadata array pointer and length
// Initialized during boot
pub(crate) static mut FRAME_META: *mut VmPage = core::ptr::null_mut();
pub(crate) static mut FRAME_META_LEN: usize = 0;

pub fn get_frame_meta(phys: PhysAddr) -> Option<&'static mut VmPage> {
    let pfn = phys.as_usize() / PAGE_SIZE;
    unsafe {
        if pfn < FRAME_META_LEN {
            Some(&mut *FRAME_META.add(pfn))
        } else {
            None
        }
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
