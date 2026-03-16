use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

pub mod entry;
pub mod splay;

pub(crate) use splay::{SplayTree, SplayTreeIter};

use crate::{
    hal_common::{Errno, LeveledRwLock, PhysAddr, SpinMutex, VirtAddr, PAGE_SIZE},
    mm::{
        pmap_enter, pmap_extract, pmap_protect, pmap_remove,
        vm::{CowState, MapPerm, VmMapEntry, VmMapping, VmObject},
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

    // -----------------------------------------------------------------------
    // Builder entry points
    // -----------------------------------------------------------------------

    /// Map a region into the address space.
    /// Validates alignment, overlap. Bumps timestamp.
    /// Does NOT touch pmap (lazy fault model).
    pub fn map(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        perm: MapPerm,
        mapping: VmMapping,
    ) -> Result<(), Errno> {
        let entry = VmMapEntry::new(
            start.as_usize() as u64,
            end.as_usize() as u64,
            mapping,
            perm,
        );
        self.insert_entry(entry)
    }

    /// map() variant for exec: if an entry at exact [start, end) with
    /// the same file offset exists, merge permissions (|=).
    /// Absorbs the old insert_or_merge_file_vma function.
    pub fn map_or_merge(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        perm: MapPerm,
        mapping: VmMapping,
    ) -> Result<(), Errno> {
        let s = start.as_usize() as u64;
        let e = end.as_usize() as u64;
        if let Some(existing) = self.lookup_mut(s) {
            if existing.start() == s
                && existing.end() == e
                && existing.mapping.offset() == mapping.offset()
            {
                existing.protection |= perm;
                return Ok(());
            }
        }
        self.map(start, end, perm, mapping)
    }

    // -----------------------------------------------------------------------
    // Accountant methods
    // -----------------------------------------------------------------------

    /// Replace the VmObject inside a VmMapping variant.
    /// Preserves the variant discriminant. Bumps timestamp.
    pub fn rebind_store(
        &mut self,
        va: u64,
        new_object: Arc<LeveledRwLock<VmObject, 3>>,
    ) -> Result<(), Errno> {
        let vma = self.lookup_mut(va).ok_or(Errno::Esrch)?;
        vma.mapping = vma.mapping.with_object(new_object);
        self.timestamp.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// Set the COW lifecycle state on an entry. Bumps timestamp.
    pub fn set_cow_state(&mut self, va: u64, state: CowState) -> Result<(), Errno> {
        let vma = self.lookup_mut(va).ok_or(Errno::Esrch)?;
        vma.cow_state = state;
        self.timestamp.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Sigcode VMA
    // -----------------------------------------------------------------------

    /// Map the sigcode trampoline page as a Shared VMA and eagerly enter it
    /// into the pmap.  Called during exec and (implicitly via fork VMA copy)
    /// during fork.
    pub fn map_sigcode(&mut self) {
        use crate::proc::signal::{sigcode_object, SIGCODE_VA};

        let obj = sigcode_object().clone();
        self.map(
            VirtAddr::new(SIGCODE_VA),
            VirtAddr::new(SIGCODE_VA + PAGE_SIZE),
            crate::map_perm!(R, X, U),
            VmMapping::Shared {
                object: obj,
                offset: 0,
            },
        )
        .expect("map_sigcode: insert failed");

        // Eagerly enter the page into pmap so signal delivery works
        // without a fault.
        if let Some(pa) = sigcode_object()
            .read()
            .lookup_page(crate::mm::vm::VObjIndex::new(0))
        {
            let mut pmap = self.pmap.lock();
            let _ = pmap_enter(
                &mut pmap,
                VirtAddr::new(SIGCODE_VA),
                pa,
                crate::map_perm!(R, X, U),
                false,
            );
        }
    }

    // -----------------------------------------------------------------------
    // Heap management
    // -----------------------------------------------------------------------

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
                    if vma.end() == old_brk_aligned as u64
                        && matches!(vma.mapping, VmMapping::Heap { .. })
                    {
                        let heap_start = vma.start() as usize;
                        vma.set_bounds(heap_start as u64, new_brk_aligned as u64);
                        if let Some(object) = vma.mapping.object() {
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
        let mapping = VmMapping::Heap {
            object: VmObject::new_anon(grow_len),
            offset: 0,
        };
        self.map(
            VirtAddr::new(old_brk_aligned),
            VirtAddr::new(new_brk_aligned),
            MapPerm::R | MapPerm::W | MapPerm::U,
            mapping,
        )
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
                if vma.end() == old_brk_aligned as u64
                    && matches!(vma.mapping, VmMapping::Heap { .. })
                {
                    let heap_start = vma.start() as usize;
                    let new_heap_end = core::cmp::max(new_brk_aligned, heap_start);

                    if new_heap_end <= heap_start {
                        remove_heap_va = Some((old_brk_aligned - 1) as u64);
                    } else {
                        vma.set_bounds(heap_start as u64, new_heap_end as u64);
                        if let Some(object) = vma.mapping.object() {
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

    // -----------------------------------------------------------------------
    // COW fork
    // -----------------------------------------------------------------------

    /// COW fork: create shadow chains for writable VMAs, share read-only VMAs.
    /// Copies parent page mappings into child pmap.
    ///
    /// `self` is the parent VmMap, `child` is the freshly-created child VmMap.
    /// After this call, writable pages are downgraded to RO in both parent and
    /// child pmaps so the first write triggers a COW fault.
    pub fn cow_fork_into(&mut self, child: &mut VmMap) -> Result<(), Errno> {
        let mut parent_rebinds: alloc::vec::Vec<(u64, VmMapping)> = alloc::vec::Vec::new();
        let mut child_entries: alloc::vec::Vec<VmMapEntry> = alloc::vec::Vec::new();
        let mut pmap_ranges: alloc::vec::Vec<(u64, u64, MapPerm, bool)> = alloc::vec::Vec::new();

        for vma in self.iter() {
            let is_writable = vma.protection.contains(MapPerm::W);

            let (parent_new_mapping, child_new_mapping) = match &vma.mapping {
                VmMapping::Guard => (None, VmMapping::Guard),
                mapping => {
                    let parent_obj = match mapping.object() {
                        Some(obj) => obj,
                        None => {
                            // Should not happen for non-Guard variants
                            continue;
                        },
                    };

                    if is_writable {
                        // Writable mapping: both parent and child must get their own
                        // shadows backed by the original object, otherwise whichever
                        // side writes first will leak modifications to the other side.
                        let size_bytes = (vma.end() - vma.start()) as usize;
                        let parent_obj_shadow =
                            VmObject::new_shadow(Arc::clone(parent_obj), size_bytes);
                        let child_obj_shadow =
                            VmObject::new_shadow(Arc::clone(parent_obj), size_bytes);

                        // Keep file-backed demand-fault behavior on shadow heads.
                        {
                            let parent_read = parent_obj.read();
                            if let Some(ref pager) = parent_read.pager {
                                if !pager.is_anon() {
                                    parent_obj_shadow.write().pager = Some(Arc::clone(pager));
                                    child_obj_shadow.write().pager = Some(Arc::clone(pager));
                                }
                            }
                        }
                        (
                            Some(mapping.with_object(parent_obj_shadow)),
                            mapping.with_object(child_obj_shadow),
                        )
                    } else {
                        // Read-only: share the same VmObject (no shadow needed)
                        (None, mapping.with_object(Arc::clone(parent_obj)))
                    }
                },
            };

            if let Some(new_mapping) = parent_new_mapping {
                parent_rebinds.push((vma.start(), new_mapping));
            };

            let child_vma =
                VmMapEntry::new(vma.start(), vma.end(), child_new_mapping, vma.protection);
            child_entries.push(child_vma);
            pmap_ranges.push((vma.start(), vma.end(), vma.protection, is_writable));
        }

        // Rebind parent VMAs to their new COW shadow heads.
        for (start, new_mapping) in parent_rebinds {
            if let Some(vma) = self.lookup_mut(start) {
                vma.mapping = new_mapping;
            }
        }

        // Install child VMAs.
        for entry in child_entries {
            child.insert_entry(entry)?;
        }

        // Phase 2: set up pmap mappings
        //
        // Lock ordering: child_pmap (L2) → parent_pmap (L2).
        // Safe today because the child is brand-new (no concurrent access).
        let child_pmap_arc = child.pmap.clone();
        let mut child_pmap = child_pmap_arc.lock();
        let parent_pmap_arc = self.pmap.clone();
        let mut parent_pmap = parent_pmap_arc.lock();

        for (start, end, prot, is_writable) in pmap_ranges {
            if is_writable && !prot.contains(MapPerm::W) {
                continue;
            }
            if prot.is_empty() {
                continue;
            }

            let mut va = start as usize;
            while va < end as usize {
                if let Some(pa) = pmap_extract(&parent_pmap, VirtAddr::new(va)) {
                    if is_writable {
                        // Strip W from parent PTE to force COW fault on parent writes too
                        let ro_prot = prot & !MapPerm::W;
                        pmap_protect(
                            &mut parent_pmap,
                            VirtAddr::new(va),
                            VirtAddr(va + PAGE_SIZE),
                            ro_prot,
                        );
                        // Map same physical page read-only in child
                        let _ = pmap_enter(&mut child_pmap, VirtAddr::new(va), pa, ro_prot, false);
                    } else {
                        // Read-only: share with same permissions
                        let _ = pmap_enter(&mut child_pmap, VirtAddr::new(va), pa, prot, false);
                    }
                }
                va += PAGE_SIZE;
            }
        }

        Ok(())
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
