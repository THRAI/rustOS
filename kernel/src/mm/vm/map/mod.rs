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
        // Unmap every existing VMA from hardware page tables first.
        let ranges: alloc::vec::Vec<(VirtAddr, VirtAddr)> = self
            .iter()
            .map(|e| {
                (
                    VirtAddr::new(e.start as usize),
                    VirtAddr::new(e.end as usize),
                )
            })
            .collect();
        {
            let mut pmap = self.pmap.lock();
            for (start, end) in ranges {
                crate::mm::pmap::pmap_remove(&mut pmap, start, end);
            }
        }

        // Drop all map entries.
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

    /// Remove the entry containing `va`.
    pub fn remove_entry_containing(&mut self, va: u64) -> Option<VmMapEntry> {
        let removed = self.tree.remove(va)?;
        let entry = *removed;

        // Remove hardware page table mappings for this entry's VA range
        {
            let mut pmap = self.pmap.lock();
            crate::mm::pmap::pmap_remove(
                &mut pmap,
                VirtAddr::new(entry.start as usize),
                VirtAddr::new(entry.end as usize),
            );
        }

        self.size = self.size.saturating_sub(entry.size());
        self.nentries = self.nentries.saturating_sub(1);
        self.timestamp.fetch_add(1, Ordering::SeqCst);
        Some(entry)
    }

    pub fn remove_range(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
    ) -> alloc::vec::Vec<VmMapEntry> {
        if start >= end {
            return alloc::vec::Vec::new();
        }

        self.timestamp.fetch_add(1, Ordering::SeqCst);
        let start_addr = start.as_usize() as u64;
        let end_addr = end.as_usize() as u64;

        // Tear down hardware mappings first.
        {
            let mut pmap = self.pmap.lock();
            crate::mm::pmap::pmap_remove(&mut pmap, start, end);
        }

        let mut removed = alloc::vec::Vec::new();

        // Collect all entries that overlap [start_addr, end_addr).
        let mut to_remove = alloc::vec::Vec::new();
        for entry in self.iter() {
            if entry.start < end_addr && entry.end > start_addr {
                to_remove.push(entry.start);
            }
        }

        // Remove/split all overlapping entries.
        for key in to_remove {
            let entry_box = match self.tree.remove(key) {
                Some(e) => e,
                None => continue,
            };
            let entry = *entry_box;
            let entry_start = entry.start();
            let entry_end = entry.end();

            // Account for removing the original entry from the tree.
            self.size = self.size.saturating_sub(entry.size());
            self.nentries = self.nentries.saturating_sub(1);

            let mut split_entries = alloc::vec::Vec::new();

            // Keep left surviving piece: [entry_start, start_addr)
            if entry_start < start_addr {
                let mut left = entry.clone_for_split(entry_start);
                left.set_bounds(entry_start, start_addr);
                split_entries.push(left);
            }

            // Keep right surviving piece: [end_addr, entry_end)
            if entry_end > end_addr {
                let mut right = entry.clone_for_split(end_addr);
                right.set_bounds(end_addr, entry_end);
                split_entries.push(right);
            }

            if split_entries.is_empty() {
                // Fully removed entry.
                removed.push(entry);
            } else {
                for kept in split_entries {
                    self.size = self.size.saturating_add(kept.size());
                    self.nentries += 1;
                    self.tree.insert(alloc::boxed::Box::new(kept));
                }
            }
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

    pub fn protect_range(&mut self, start: VirtAddr, end: VirtAddr, perm: MapPerm) {
        self.timestamp.fetch_add(1, Ordering::SeqCst);
        {
            let mut pmap = self.pmap.lock();
            crate::mm::pmap::pmap_protect(&mut pmap, start, end, perm);
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
