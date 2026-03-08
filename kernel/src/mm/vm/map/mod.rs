use crate::hal_common::VirtAddr;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

pub mod entry;
pub mod splay;

use crate::hal_common::spin_mutex::SpinMutex;
use crate::mm::pmap::Pmap;
use crate::mm::vm::map::entry::{MapPerm, VmMapEntry};
use crate::mm::vm::map::splay::{SplayTree, SplayTreeIter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmError {
    Overlap,
    NotFound,
    InvalidRange,
}

pub struct VmMap {
    /// Intrusive Splay tree of VmMapEntries
    pub tree: SplayTree,

    /// Hardware page map
    pub pmap: Arc<SpinMutex<Pmap>>,

    /// Global total size of memory mapped
    pub size: u64,

    /// Entry count
    pub nentries: usize,

    /// ABA defense timestamp, incremented heavily
    pub timestamp: AtomicU32,

    /// Global map busy count to synchronize teardown vs faults/syscalls
    pub busy_count: AtomicU32,

    /// Should we wake any waiter when busy_count reaches 0
    pub needs_busy_wakeup: core::sync::atomic::AtomicBool,
}

impl VmMap {
    pub fn new(pmap: Arc<SpinMutex<Pmap>>) -> Self {
        Self {
            tree: SplayTree::new(),
            pmap,
            size: 0,
            nentries: 0,
            timestamp: AtomicU32::new(0),
            busy_count: AtomicU32::new(0),
            needs_busy_wakeup: core::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Access the underlying physical map
    pub fn pmap_lock(&self) -> crate::hal_common::spin_mutex::SpinMutexGuard<'_, Pmap> {
        self.pmap.lock()
    }

    /// Clear all mappings in the address space
    pub fn clear(&mut self) {
        // We will need a proper way to drop VmMapEntries in SplayTree.
        // For now, we can clear the tree by assigning a new one.
        self.tree = SplayTree::new();
        self.size = 0;
        self.nentries = 0;
        self.timestamp
            .fetch_add(1, core::sync::atomic::Ordering::Release);
    }
    /// Wait for map busy_count to drop to 0, blocking concurrent tasks
    pub async fn wait_for_unbusy(&self) {
        // We will implement an async wait using WaitQueue later.
        // For now, busy polling or yield
        while self.busy_count.load(Ordering::Acquire) > 0 {
            // spin or yield
        }
    }

    pub fn insert_entry(&mut self, entry: VmMapEntry) -> Result<(), VmError> {
        let entry_size = entry.size();
        self.tree.insert(alloc::boxed::Box::new(entry));
        self.size += entry_size;
        self.nentries += 1;
        self.timestamp.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    pub fn remove_range(&mut self, start: VirtAddr, end: VirtAddr) -> alloc::vec::Vec<VmMapEntry> {
        self.timestamp.fetch_add(1, Ordering::SeqCst);
        let removed = alloc::vec::Vec::new();
        // Splay tree removal over range:
        // We iterate and slice up boxes.
        // Since iterator is not yet fully implemented, we loop with a pointer
        // ... Implementation to follow.
        {
            let mut pmap = self.pmap.lock();
            crate::mm::pmap::pmap_remove(&mut pmap, start, end);
        }
        removed
    }

    pub fn lookup(&mut self, va: u64) -> Option<&VmMapEntry> {
        self.tree.lookup(va)
    }

    pub fn lookup_readonly(&self, va: u64) -> Option<&VmMapEntry> {
        self.tree.lookup_readonly(va)
    }

    pub fn lookup_mut(&mut self, va: u64) -> Option<&mut VmMapEntry> {
        self.tree.lookup_mut(va)
    }

    pub fn is_range_free(&self, start: u64, end: u64) -> bool {
        for vma in self.iter() {
            if core::cmp::max(vma.start, start) < core::cmp::min(vma.end, end) {
                return false;
            }
        }
        true
    }

    pub fn find_free_area_topdown(&self, len: usize) -> Option<VirtAddr> {
        let max_va = 0x0003_FFFF_0000_u64; // Approx user max VA
        let mut current_top = max_va;

        let mut occupied = alloc::vec::Vec::new();
        for vma in self.iter() {
            occupied.push((vma.start, vma.end));
        }
        occupied.sort_unstable_by_key(|&(s, _)| s);

        for (start, end) in occupied.into_iter().rev() {
            if current_top >= end && current_top - end >= len as u64 {
                return Some(VirtAddr::new((current_top - len as u64) as usize));
            }
            if start < current_top {
                current_top = start;
            }
        }
        if current_top >= len as u64 + 0x10000 {
            return Some(VirtAddr::new((current_top - len as u64) as usize));
        }
        None
    }

    pub fn protect_range(&mut self, _start: VirtAddr, _end: VirtAddr, _perm: MapPerm) {
        self.timestamp.fetch_add(1, Ordering::SeqCst);
        {
            // FIXME MapPerm differs, we need to map to the legacy format or update Pmap
            // let mut pmap = self.pmap.lock();
            // crate::mm::pmap::pmap_protect(&mut pmap, start, end, perm);
        }
    }

    pub fn iter(&self) -> SplayTreeIter<'_> {
        self.tree.iter()
    }
}

impl Drop for VmMap {
    fn drop(&mut self) {
        // 1. Teardown HW pmap
        // Pmap destruction handles its own frames in Pmap::drop(),
        // but we might need an explicit clear or it's handled automatically.
        // 2. Clear tree
        // SplayTree's drop iteratively drops all entries
    }
}
