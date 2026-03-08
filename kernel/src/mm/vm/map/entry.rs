use super::VmMap;
use crate::mm::vm::vm_object::VmObject;
use alloc::sync::Arc;
use bitflags::bitflags;
use core::ptr::NonNull;

bitflags! {
    /// VmMapEntry state flags for COW and concurrency control.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EntryFlags: u32 {
        const COW            = 1 << 0;
        const NEEDS_COPY     = 1 << 1;
        const GROWS_DOWN     = 1 << 2;
        const IN_TRANSITION  = 1 << 3;
        const NEEDS_WAKEUP   = 1 << 4;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmInherit {
    Share,
    Copy,
    None,
}

pub enum BackingStore {
    Object {
        object: Arc<spin::RwLock<VmObject>>,
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
    /// [Private Fields] Protected topology boundaries
    pub(crate) start: u64,
    pub(crate) end: u64,
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
            start,
            end,
            max_free: 0,
            splay_node: Node::new(),
            store,
            flags,
            protection,
            max_protection: protection,
            inheritance: VmInherit::Copy,
        }
    }

    pub fn start(&self) -> u64 {
        self.start
    }
    pub fn end(&self) -> u64 {
        self.end
    }
    pub fn size(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_mergeable_with(&self, next: &VmMapEntry) -> bool {
        if self.flags.contains(EntryFlags::IN_TRANSITION)
            || next.flags.contains(EntryFlags::IN_TRANSITION)
        {
            return false;
        }
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
            start: self.start,
            end: self.end,
            max_free: 0,
            splay_node: Node::new(),
            store: self.store.clone(),
            flags: self.flags,
            protection: self.protection,
            max_protection: self.max_protection,
            inheritance: self.inheritance,
        };

        if let BackingStore::Object { offset, .. } = &mut new_entry.store {
            *offset += split_addr - self.start;
        }
        new_entry
    }

    pub(crate) fn set_bounds(&mut self, new_start: u64, new_end: u64) {
        self.start = new_start;
        self.end = new_end;
    }
}

impl Drop for VmMapEntry {
    fn drop(&mut self) {
        // Rust's default drop behavior handles BackingStore dropping automatically,
        // reducing VmObject reference counts and triggering their teardown.
        if self.flags.contains(EntryFlags::IN_TRANSITION) {
            // Usually we shouldn't drop an IN_TRANSITION entry, but panic here may be unsafe in panicking contexts.
            // A well-behaved kernel shouldn't reach here.
            crate::kprintln!(
                "WARNING: Dropping VmMapEntry that is IN_TRANSITION! start={:#x}",
                self.start
            );
        }
    }
}
