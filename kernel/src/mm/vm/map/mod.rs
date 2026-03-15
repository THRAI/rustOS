use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::hal_common::{Errno, VirtAddr, PAGE_SIZE};

pub mod entry;
pub mod splay;

pub(crate) use splay::{SplayTree, SplayTreeIter};

use crate::{
    hal_common::SpinMutex,
    mm::{
        pmap_protect, pmap_remove,
        vm::{BackingStore, EntryFlags, MapPerm, VmMapEntry},
        Pmap,
    },
};

pub struct VmMap {
    /// Intrusive Splay tree of VmMapEntries
    pub tree: SplayTree,

    /// Hardware page map
    pub pmap: Arc<SpinMutex<Pmap, 2>>,

    /// Global total size of memory mapped
    pub size: u64,

    /// Entry count
    pub nentries: usize,

    /// ABA defense timestamp, incremented at every map mutation.
    ///
    /// ## Async Fault Revalidation Protocol
    ///
    /// The async fault handler drops the VmMap lock during I/O. On re-acquire,
    /// it compares the saved timestamp against the current value. If changed,
    /// it re-lookups the VMA and verifies backing object identity via
    /// `Arc::ptr_eq`. This single mechanism replaces three separate designs:
    ///
    /// - **VMA removal detection** (replaces `IN_TRANSITION`): timestamp
    ///   increments on every remove_range/remove_entry_containing. Fault
    ///   handler re-lookups VMA — if gone, discards the frame and bails.
    ///
    /// - **Permission change detection**: timestamp increments on
    ///   protect_range. Fault handler uses fresh `vma.protection`.
    ///
    /// - **Address space replacement detection** (replaces `busy_count`):
    ///   after execve, the entire VmMap is replaced. Fault handler
    ///   re-acquires the (new) map, sees timestamp mismatch,
    ///   re-lookups VMA. Object identity check (`Arc::ptr_eq`) catches the
    ///   case where the same VA exists in the new map with a different
    ///   backing object.
    ///
    /// If thread profiling later shows that wasted I/O from concurrent
    /// munmap-during-fault is a bottleneck, a `busy_count` prevention
    /// layer can be added on top without changing the detection logic.
    pub timestamp: AtomicU32,
}

impl VmMap {
    pub fn new(pmap: Arc<SpinMutex<Pmap, 2>>) -> Self {
        Self {
            tree: SplayTree::new(),
            pmap,
            size: 0,
            nentries: 0,
            timestamp: AtomicU32::new(0),
        }
    }

    /// Access the underlying physical map
    pub fn pmap_lock(&self) -> crate::hal_common::SpinMutexGuard<'_, Pmap, 2> {
        self.pmap.lock()
    }

    /// Clear all mappings in the address space
    pub fn clear(&mut self) {
        // Unmap every existing VMA from hardware page tables first.
        let ranges: alloc::vec::Vec<(VirtAddr, VirtAddr)> = self
            .iter()
            .map(|e| {
                (
                    VirtAddr::new(e.start() as usize),
                    VirtAddr::new(e.end() as usize),
                )
            })
            .collect();
        {
            let mut pmap = self.pmap.lock();
            for (start, end) in ranges {
                pmap_remove(&mut pmap, start, end);
            }
        }

        // Drop all map entries.
        self.tree = SplayTree::new();
        self.size = 0;
        self.nentries = 0;
        self.timestamp
            .fetch_add(1, core::sync::atomic::Ordering::Release);
    }

    pub fn insert_entry(&mut self, entry: VmMapEntry) -> Result<(), Errno> {
        if entry.start() >= entry.end() {
            return Err(Errno::Einval);
        }

        let page_size = PAGE_SIZE as u64;
        if entry.start() % page_size != 0 || entry.end() % page_size != 0 {
            return Err(Errno::Einval);
        }

        for existing in self.iter() {
            if existing.start() < entry.end() && existing.end() > entry.start() {
                return Err(Errno::Einval);
            }
        }

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
            pmap_remove(
                &mut pmap,
                VirtAddr::new(entry.start() as usize),
                VirtAddr::new(entry.end() as usize),
            );
        }

        self.size = self.size.saturating_sub(entry.size());
        self.nentries = self.nentries.saturating_sub(1);
        self.timestamp.fetch_add(1, Ordering::SeqCst);
        Some(entry)
    }

    pub fn remove_range(&mut self, start: VirtAddr, end: VirtAddr) -> alloc::vec::Vec<VmMapEntry> {
        if start >= end {
            return alloc::vec::Vec::new();
        }

        self.timestamp.fetch_add(1, Ordering::SeqCst);
        let start_addr = start.as_usize() as u64;
        let end_addr = end.as_usize() as u64;

        // Tear down hardware mappings first.
        {
            let mut pmap = self.pmap.lock();
            pmap_remove(&mut pmap, start, end);
        }

        let mut removed = alloc::vec::Vec::new();

        // Collect all entries that overlap [start_addr, end_addr).
        let mut to_remove = alloc::vec::Vec::new();
        for entry in self.iter() {
            if entry.start() < end_addr && entry.end() > start_addr {
                to_remove.push(entry.start());
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
            if core::cmp::max(vma.start(), start) < core::cmp::min(vma.end(), end) {
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
            occupied.push((vma.start(), vma.end()));
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

    pub fn protect_range(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        perm: MapPerm,
    ) -> Result<(), Errno> {
        if start >= end {
            return Err(Errno::Einval);
        }

        let start_addr = start.as_usize() as u64;
        let end_addr = end.as_usize() as u64;

        let mut overlapping = alloc::vec::Vec::new();
        for entry in self.iter() {
            if entry.start() < end_addr && entry.end() > start_addr {
                overlapping.push((entry.start(), entry.end(), entry.max_protection));
            }
        }

        if overlapping.is_empty() {
            return Err(Errno::Enomem);
        }

        overlapping.sort_unstable_by_key(|&(entry_start, _, _)| entry_start);

        let mut covered_until = start_addr;
        for &(entry_start, entry_end, max_protection) in &overlapping {
            if entry_start > covered_until {
                return Err(Errno::Enomem);
            }
            if !max_protection.contains(perm) {
                return Err(Errno::Einval);
            }
            if entry_end > covered_until {
                covered_until = entry_end;
            }
            if covered_until >= end_addr {
                break;
            }
        }

        if covered_until < end_addr {
            return Err(Errno::Enomem);
        }

        let keys: alloc::vec::Vec<u64> = overlapping
            .into_iter()
            .map(|(entry_start, _, _)| entry_start)
            .collect();

        for key in keys {
            let entry_box = match self.tree.remove(key) {
                Some(entry) => entry,
                None => continue,
            };
            let entry = *entry_box;

            self.size = self.size.saturating_sub(entry.size());
            self.nentries = self.nentries.saturating_sub(1);

            let entry_start = entry.start();
            let entry_end = entry.end();

            if entry_start < start_addr {
                let mut left = entry.clone_for_split(entry_start);
                left.set_bounds(entry_start, start_addr);
                self.size = self.size.saturating_add(left.size());
                self.nentries += 1;
                self.tree.insert(alloc::boxed::Box::new(left));
            }

            let protected_start = core::cmp::max(entry_start, start_addr);
            let protected_end = core::cmp::min(entry_end, end_addr);
            if protected_start < protected_end {
                let mut middle = entry.clone_for_split(protected_start);
                middle.set_bounds(protected_start, protected_end);
                middle.protection = perm;
                self.size = self.size.saturating_add(middle.size());
                self.nentries += 1;
                self.tree.insert(alloc::boxed::Box::new(middle));
            }

            if entry_end > end_addr {
                let mut right = entry.clone_for_split(end_addr);
                right.set_bounds(end_addr, entry_end);
                self.size = self.size.saturating_add(right.size());
                self.nentries += 1;
                self.tree.insert(alloc::boxed::Box::new(right));
            }
        }

        self.timestamp.fetch_add(1, Ordering::SeqCst);
        {
            let mut pmap = self.pmap.lock();
            pmap_protect(&mut pmap, start, end, perm);
        }
        Ok(())
    }

    pub fn grow_heap(
        &mut self,
        old_brk_aligned: usize,
        new_brk_aligned: usize,
    ) -> Result<(), Errno> {
        if new_brk_aligned <= old_brk_aligned {
            return Err(Errno::Einval);
        }

        if old_brk_aligned > 0 {
            let mut extended = false;
            {
                if let Some(vma) = self.lookup_mut((old_brk_aligned - 1) as u64) {
                    if vma.end() == old_brk_aligned as u64 && vma.flags.contains(EntryFlags::HEAP) {
                        let heap_start = vma.start() as usize;
                        vma.set_bounds(heap_start as u64, new_brk_aligned as u64);
                        if let BackingStore::Object { object, .. } = &vma.store {
                            object.write().set_size(new_brk_aligned - heap_start);
                        }
                        extended = true;
                    }
                }
            }

            if extended {
                self.size = self
                    .size
                    .saturating_add((new_brk_aligned - old_brk_aligned) as u64);
                self.timestamp.fetch_add(1, Ordering::SeqCst);
                return Ok(());
            }
        }

        if !self.is_range_free(old_brk_aligned as u64, new_brk_aligned as u64) {
            return Err(Errno::Einval);
        }

        let grow_len = new_brk_aligned - old_brk_aligned;
        let obj = crate::mm::vm::VmObject::new_anon(grow_len);
        let vma = VmMapEntry::new(
            old_brk_aligned as u64,
            new_brk_aligned as u64,
            BackingStore::Object {
                object: obj,
                offset: 0,
            },
            EntryFlags::HEAP,
            MapPerm::R | MapPerm::W | MapPerm::U,
        );
        self.insert_entry(vma)
    }

    pub fn shrink_heap(
        &mut self,
        old_brk_aligned: usize,
        new_brk_aligned: usize,
    ) -> Result<alloc::vec::Vec<VmMapEntry>, Errno> {
        if new_brk_aligned >= old_brk_aligned {
            return Err(Errno::Einval);
        }

        if old_brk_aligned == 0 {
            return Ok(alloc::vec::Vec::new());
        }

        let mut unmap_range = None;
        let mut remove_heap_va = None;

        {
            if let Some(vma) = self.lookup_mut((old_brk_aligned - 1) as u64) {
                if vma.end() == old_brk_aligned as u64 && vma.flags.contains(EntryFlags::HEAP) {
                    let heap_start = vma.start() as usize;
                    let new_heap_end = core::cmp::max(new_brk_aligned, heap_start);

                    if new_heap_end <= heap_start {
                        remove_heap_va = Some((old_brk_aligned - 1) as u64);
                    } else {
                        vma.set_bounds(heap_start as u64, new_heap_end as u64);
                        if let BackingStore::Object { object, .. } = &vma.store {
                            let new_pages =
                                (new_heap_end - heap_start) / crate::hal_common::PAGE_SIZE;
                            let mut obj = object.write();
                            obj.set_size(new_heap_end - heap_start);
                            obj.truncate_pages(crate::mm::vm::VObjIndex::new(new_pages));
                        }
                        unmap_range = Some((new_heap_end, old_brk_aligned));
                    }
                }
            }
        }

        if let Some(va) = remove_heap_va {
            let mut removed_entries = alloc::vec::Vec::new();
            if let Some(removed) = self.remove_entry_containing(va) {
                removed_entries.push(removed);
            }
            return Ok(removed_entries);
        }

        if let Some((new_heap_end, old_heap_end)) = unmap_range {
            if new_heap_end < old_heap_end {
                let mut pmap = self.pmap.lock();
                pmap_remove(
                    &mut pmap,
                    VirtAddr::new(new_heap_end),
                    VirtAddr::new(old_heap_end),
                );
                self.size = self
                    .size
                    .saturating_sub((old_heap_end - new_heap_end) as u64);
                self.timestamp.fetch_add(1, Ordering::SeqCst);
            }
        }

        Ok(alloc::vec::Vec::new())
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
