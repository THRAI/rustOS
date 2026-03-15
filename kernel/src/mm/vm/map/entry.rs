use alloc::sync::Arc;
use core::ptr::NonNull;

use bitflags::bitflags;

use super::VmMap;
use crate::{hal_common::VirtAddrRange, mm::vm::VmObject};

bitflags! {
    /// VmMapEntry state flags for COW and concurrency control.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EntryFlags: u32 {
        const COW            = 1 << 0;
        const NEEDS_COPY     = 1 << 1;
        const GROWS_DOWN     = 1 << 2;
        /// User heap area managed by brk(2).
        const HEAP           = 1 << 5;
    }
}

bitflags! {
    /// Page protection / permission flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MapPerm: u8 {
        const R = 1 << 0;
        const W = 1 << 1;
        const X = 1 << 2;
        const U = 1 << 3;
    }
}

/// Declarative macro to combine [`MapPerm`] flags concisely.
///
/// Usage: `map_perm!(R, W, U)` expands to `MapPerm::R | MapPerm::W | MapPerm::U`.
#[macro_export]
macro_rules! map_perm {
    () => { $crate::mm::vm::MapPerm::empty() };
    ($($flag:ident),+) => {
        $($crate::mm::vm::MapPerm::$flag)|+
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmInherit {
    Share,
    Copy,
    None,
}

pub enum BackingStore {
    Object {
        object: Arc<crate::hal_common::LeveledRwLock<VmObject, 3>>,
        offset: u64,
    },
    SubMap(Arc<VmMap>),
    Guard,
}

impl Clone for BackingStore {
    fn clone(&self) -> Self {
        match self {
            Self::Object { object, offset } => Self::Object {
                object: Arc::clone(object),
                offset: *offset,
            },
            Self::SubMap(map) => Self::SubMap(Arc::clone(map)),
            Self::Guard => Self::Guard,
        }
    }
}

pub struct Node {
    pub left: Option<NonNull<VmMapEntry>>,
    pub right: Option<NonNull<VmMapEntry>>,
}

impl Node {
    pub const fn new() -> Self {
        Self {
            left: None,
            right: None,
        }
    }
}

pub struct VmMapEntry {
    /// Half-open virtual address range [start, end).
    pub(crate) range: VirtAddrRange,
    pub(crate) max_free: u64,

    /// Intrusive Splay Node
    pub(crate) splay_node: Node,

    /// [Public Attributes]
    pub store: BackingStore,
    pub flags: EntryFlags,
    pub protection: MapPerm,
    pub max_protection: MapPerm,
    pub inheritance: VmInherit,
}

impl VmMapEntry {
    pub fn new(
        start: u64,
        end: u64,
        store: BackingStore,
        flags: EntryFlags,
        protection: MapPerm,
    ) -> Self {
        Self {
            range: VirtAddrRange::from_raw(start as usize, end as usize),
            max_free: 0,
            splay_node: Node::new(),
            store,
            flags,
            protection,
            max_protection: protection,
            inheritance: VmInherit::Copy,
        }
    }

    /// Start address as u64 (for splay tree key compatibility).
    pub fn start(&self) -> u64 {
        self.range.start().as_usize() as u64
    }

    /// End address as u64 (for splay tree key compatibility).
    pub fn end(&self) -> u64 {
        self.range.end().as_usize() as u64
    }

    /// Size in bytes.
    pub fn size(&self) -> u64 {
        self.range.len() as u64
    }

    /// Get the typed address range.
    pub fn addr_range(&self) -> VirtAddrRange {
        self.range
    }

    pub fn is_mergeable_with(&self, next: &VmMapEntry) -> bool {
        if self.flags != next.flags
            || self.protection != next.protection
            || self.inheritance != next.inheritance
        {
            return false;
        }
        match (&self.store, &next.store) {
            (
                BackingStore::Object {
                    object: o1,
                    offset: off1,
                },
                BackingStore::Object {
                    object: o2,
                    offset: off2,
                },
            ) => Arc::ptr_eq(o1, o2) && (*off1 + self.size() == *off2),
            (BackingStore::Guard, BackingStore::Guard) => true,
            _ => false,
        }
    }

    pub fn clone_for_split(&self, split_addr: u64) -> Self {
        let mut new_entry = Self {
            range: self.range,
            max_free: 0,
            splay_node: Node::new(),
            store: self.store.clone(),
            flags: self.flags,
            protection: self.protection,
            max_protection: self.max_protection,
            inheritance: self.inheritance,
        };

        if let BackingStore::Object { offset, .. } = &mut new_entry.store {
            *offset += split_addr - self.start();
        }
        new_entry
    }

    pub(crate) fn set_bounds(&mut self, new_start: u64, new_end: u64) {
        self.range = VirtAddrRange::from_raw(new_start as usize, new_end as usize);
    }
}
