use alloc::sync::Arc;
use core::ptr::NonNull;

use bitflags::bitflags;

use crate::{
    hal_common::{LeveledRwLock, VirtAddr, VirtAddrRange},
    mm::vm::{VObjIndex, VmObject},
};

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

/// Copy-on-write state for a VMA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CowState {
    None,
    CopyOnWrite,
}

/// Region kind + backing store. Carried from builder into entry.
/// Determines inheritance behavior for fork.
#[derive(Clone)]
pub enum VmMapping {
    /// Anonymous private (mmap MAP_ANON|MAP_PRIVATE, exec stack).
    AnonPrivate {
        object: Arc<LeveledRwLock<VmObject, 3>>,
        offset: u64,
    },
    /// File-backed private (mmap MAP_PRIVATE on file, exec PT_LOAD).
    FilePrivate {
        object: Arc<LeveledRwLock<VmObject, 3>>,
        offset: u64,
    },
    /// File-backed shared (mmap MAP_SHARED).
    FileShared {
        object: Arc<LeveledRwLock<VmObject, 3>>,
        offset: u64,
    },
    /// Heap region managed by brk(2).
    Heap {
        object: Arc<LeveledRwLock<VmObject, 3>>,
        offset: u64,
    },
    /// Shared page (sigcode trampoline, future POSIX shm).
    Shared {
        object: Arc<LeveledRwLock<VmObject, 3>>,
        offset: u64,
    },
    /// Guard page. No backing; faults always fatal.
    Guard,
}

impl VmMapping {
    pub fn inheritance(&self) -> VmInherit {
        match self {
            Self::AnonPrivate { .. } | Self::FilePrivate { .. } | Self::Heap { .. } => {
                VmInherit::Copy
            },
            Self::FileShared { .. } | Self::Shared { .. } => VmInherit::Share,
            Self::Guard => VmInherit::None,
        }
    }

    pub fn object(&self) -> Option<&Arc<LeveledRwLock<VmObject, 3>>> {
        match self {
            Self::AnonPrivate { object, .. }
            | Self::FilePrivate { object, .. }
            | Self::FileShared { object, .. }
            | Self::Heap { object, .. }
            | Self::Shared { object, .. } => Some(object),
            Self::Guard => None,
        }
    }

    pub fn offset(&self) -> u64 {
        match self {
            Self::AnonPrivate { offset, .. }
            | Self::FilePrivate { offset, .. }
            | Self::FileShared { offset, .. }
            | Self::Heap { offset, .. }
            | Self::Shared { offset, .. } => *offset,
            Self::Guard => 0,
        }
    }

    /// Create a mapping of the same variant but with a different backing object.
    pub fn with_object(&self, new_object: Arc<LeveledRwLock<VmObject, 3>>) -> Self {
        match self {
            Self::AnonPrivate { offset, .. } => Self::AnonPrivate {
                object: new_object,
                offset: *offset,
            },
            Self::FilePrivate { offset, .. } => Self::FilePrivate {
                object: new_object,
                offset: *offset,
            },
            Self::FileShared { offset, .. } => Self::FileShared {
                object: new_object,
                offset: *offset,
            },
            Self::Heap { offset, .. } => Self::Heap {
                object: new_object,
                offset: *offset,
            },
            Self::Shared { offset, .. } => Self::Shared {
                object: new_object,
                offset: *offset,
            },
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
    pub(crate) max_free: usize,

    /// Intrusive Splay Node
    pub(crate) splay_node: Node,

    /// [Public Attributes]
    pub mapping: VmMapping,
    pub protection: MapPerm,
    pub max_protection: MapPerm,
    pub cow_state: CowState,
}

impl VmMapEntry {
    pub fn new(start: VirtAddr, end: VirtAddr, mapping: VmMapping, protection: MapPerm) -> Self {
        Self {
            range: VirtAddrRange::from_raw(start.as_usize(), end.as_usize()),
            max_free: 0,
            splay_node: Node::new(),
            mapping,
            protection,
            max_protection: protection,
            cow_state: CowState::None,
        }
    }

    /// Start address.
    pub fn start(&self) -> VirtAddr {
        self.range.start()
    }

    /// End address.
    pub fn end(&self) -> VirtAddr {
        self.range.end()
    }

    /// Size in bytes.
    pub fn size(&self) -> usize {
        self.range.len()
    }

    /// Get the typed address range.
    pub fn addr_range(&self) -> VirtAddrRange {
        self.range
    }

    pub fn is_mergeable_with(&self, next: &VmMapEntry) -> bool {
        if self.cow_state != next.cow_state || self.protection != next.protection {
            return false;
        }
        if core::mem::discriminant(&self.mapping) != core::mem::discriminant(&next.mapping) {
            return false;
        }
        match (&self.mapping, &next.mapping) {
            (VmMapping::Guard, VmMapping::Guard) => true,
            _ => match (self.mapping.object(), next.mapping.object()) {
                (Some(o1), Some(o2)) => {
                    Arc::ptr_eq(o1, o2)
                        && (self.mapping.offset() + self.size() as u64 == next.mapping.offset())
                },
                _ => false,
            },
        }
    }

    pub fn clone_for_split(&self, split_addr: VirtAddr) -> Self {
        let mut new_entry = Self {
            range: self.range,
            max_free: 0,
            splay_node: Node::new(),
            mapping: self.mapping.clone(),
            protection: self.protection,
            max_protection: self.max_protection,
            cow_state: self.cow_state,
        };

        let delta = (split_addr.as_usize() - self.start().as_usize()) as u64;
        match &mut new_entry.mapping {
            VmMapping::AnonPrivate { offset, .. }
            | VmMapping::FilePrivate { offset, .. }
            | VmMapping::FileShared { offset, .. }
            | VmMapping::Heap { offset, .. }
            | VmMapping::Shared { offset, .. } => {
                *offset += delta;
            },
            VmMapping::Guard => {},
        }
        new_entry
    }

    pub(crate) fn set_bounds(&mut self, new_start: VirtAddr, new_end: VirtAddr) {
        self.range = VirtAddrRange::from_raw(new_start.as_usize(), new_end.as_usize());
    }

    /// Extract the backing VmObject and compute the page offset for a fault VA.
    ///
    /// Returns `None` for Guard mappings (no backing).
    pub fn extract_object_offset(
        &self,
        fault_va_aligned: VirtAddr,
    ) -> Option<(Arc<LeveledRwLock<VmObject, 3>>, VObjIndex)> {
        let object = self.mapping.object()?;
        let offset = self.mapping.offset();
        let offset_bytes = offset + (fault_va_aligned.as_usize() - self.start().as_usize()) as u64;
        Some((
            object.clone(),
            VObjIndex::from_bytes_floor(offset_bytes as usize),
        ))
    }
}
