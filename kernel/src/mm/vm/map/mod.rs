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
    pub size: usize,

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
        let ranges: alloc::vec::Vec<(VirtAddr, VirtAddr)> =
            self.iter().map(|e| (e.start(), e.end())).collect();
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

    /// Remove an entry from the tree by key, updating size/nentries bookkeeping.
    fn remove_tree_entry(&mut self, key: VirtAddr) -> Option<VmMapEntry> {
        let entry = *self.tree.remove(key)?;
        self.size = self.size.saturating_sub(entry.size());
        self.nentries = self.nentries.saturating_sub(1);
        Some(entry)
    }

    /// Reinsert entries into the tree, updating size/nentries bookkeeping.
    fn reinsert_entries(&mut self, entries: impl Iterator<Item = VmMapEntry>) {
        for entry in entries {
            self.size += entry.size();
            self.nentries += 1;
            self.tree.insert(alloc::boxed::Box::new(entry));
        }
    }

    pub fn insert_entry(&mut self, entry: VmMapEntry) -> Result<(), Errno> {
        if entry.start() >= entry.end() {
            return Err(Errno::Einval);
        }

        if entry.start().as_usize() % PAGE_SIZE != 0 || entry.end().as_usize() % PAGE_SIZE != 0 {
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
    pub fn remove_entry_containing(&mut self, va: VirtAddr) -> Option<VmMapEntry> {
        let entry = self.remove_tree_entry(va)?;

        // Remove hardware page table mappings for this entry's VA range
        {
            let mut pmap = self.pmap.lock();
            pmap_remove(&mut pmap, entry.start(), entry.end());
        }

        self.timestamp.fetch_add(1, Ordering::SeqCst);
        Some(entry)
    }

    pub fn remove_range(&mut self, start: VirtAddr, end: VirtAddr) -> alloc::vec::Vec<VmMapEntry> {
        if start >= end {
            return alloc::vec::Vec::new();
        }

        // Pmap: tear down entire range upfront (preserves dirty bit attribution)
        {
            let mut pmap = self.pmap.lock();
            pmap_remove(&mut pmap, start, end);
        }

        let keys = overlapping_keys(self, start, end);
        let mut removed = alloc::vec::Vec::new();
        for key in keys {
            let entry = match self.remove_tree_entry(key) {
                Some(e) => e,
                None => continue,
            };
            let split = split_entry_at(entry, start, end);
            self.reinsert_entries([split.left, split.right].into_iter().flatten());
            removed.push(split.middle);
        }

        self.timestamp.fetch_add(1, Ordering::SeqCst);
        removed
    }

    pub fn lookup(&mut self, va: VirtAddr) -> Option<&VmMapEntry> {
        self.tree.lookup(va)
    }

    pub fn lookup_readonly(&self, va: VirtAddr) -> Option<&VmMapEntry> {
        self.tree.lookup_readonly(va)
    }

    pub fn lookup_mut(&mut self, va: VirtAddr) -> Option<&mut VmMapEntry> {
        self.tree.lookup_mut(va)
    }

    pub fn is_range_free(&self, start: VirtAddr, end: VirtAddr) -> bool {
        for vma in self.iter() {
            if core::cmp::max(vma.start(), start) < core::cmp::min(vma.end(), end) {
                return false;
            }
        }
        true
    }

    pub fn find_free_area_topdown(&self, len: usize) -> Option<VirtAddr> {
        let max_va: usize = 0x0003_FFFF_0000; // Approx user max VA
        let mut current_top = max_va;

        let mut occupied = alloc::vec::Vec::new();
        for vma in self.iter() {
            occupied.push((vma.start().as_usize(), vma.end().as_usize()));
        }
        occupied.sort_unstable_by_key(|&(s, _)| s);

        for (start, end) in occupied.into_iter().rev() {
            if current_top >= end && current_top - end >= len {
                return Some(VirtAddr::new(current_top - len));
            }
            if start < current_top {
                current_top = start;
            }
        }
        if current_top >= len + 0x10000 {
            return Some(VirtAddr::new(current_top - len));
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

        // Validate: full coverage + max_protection (pure, no mutation)
        validate_protect_coverage(self, start, end, perm)?;

        // Split + re-protect affected middles
        let keys = overlapping_keys(self, start, end);
        for key in keys {
            let entry = match self.remove_tree_entry(key) {
                Some(e) => e,
                None => continue,
            };
            let mut split = split_entry_at(entry, start, end);
            split.middle.protection = perm;
            self.reinsert_entries(
                [split.left, Some(split.middle), split.right]
                    .into_iter()
                    .flatten(),
            );
        }

        // Pmap: update hardware PTEs (after VMA update per FreeBSD ordering)
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
            start,
            end,
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
        let s = start;
        let e = end;
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
        va: VirtAddr,
        new_object: Arc<LeveledRwLock<VmObject, 3>>,
    ) -> Result<(), Errno> {
        let vma = self.lookup_mut(va).ok_or(Errno::Esrch)?;
        vma.mapping = vma.mapping.with_object(new_object);
        self.timestamp.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// Set the COW lifecycle state on an entry. Bumps timestamp.
    pub fn set_cow_state(&mut self, va: VirtAddr, state: CowState) -> Result<(), Errno> {
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
        old_brk_aligned: VirtAddr,
        new_brk_aligned: VirtAddr,
    ) -> Result<(), Errno> {
        if new_brk_aligned <= old_brk_aligned {
            return Err(Errno::Einval);
        }

        if old_brk_aligned > VirtAddr::new(0) {
            let mut extended = false;
            {
                if let Some(vma) = self.lookup_mut(old_brk_aligned - 1) {
                    if vma.end() == old_brk_aligned
                        && matches!(vma.mapping, VmMapping::Heap { .. })
                    {
                        let heap_start = vma.start();
                        vma.set_bounds(heap_start, new_brk_aligned);
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
                    .saturating_add((new_brk_aligned - old_brk_aligned) as usize);
                self.timestamp.fetch_add(1, Ordering::SeqCst);
                return Ok(());
            }
        }

        if !self.is_range_free(old_brk_aligned, new_brk_aligned) {
            return Err(Errno::Einval);
        }

        let grow_len = new_brk_aligned - old_brk_aligned;
        let mapping = VmMapping::Heap {
            object: VmObject::new_anon(grow_len),
            offset: 0,
        };
        self.map(
            old_brk_aligned,
            new_brk_aligned,
            MapPerm::R | MapPerm::W | MapPerm::U,
            mapping,
        )
    }

    pub fn shrink_heap(
        &mut self,
        old_brk_aligned: VirtAddr,
        new_brk_aligned: VirtAddr,
    ) -> Result<alloc::vec::Vec<VmMapEntry>, Errno> {
        if new_brk_aligned >= old_brk_aligned {
            return Err(Errno::Einval);
        }

        if old_brk_aligned == VirtAddr::new(0) {
            return Ok(alloc::vec::Vec::new());
        }


        let mut unmap_range = None;
        let mut remove_heap_va = None;

        {
            if let Some(vma) = self.lookup_mut(old_brk_aligned - 1) {
                if vma.end()  == old_brk_aligned
                    && matches!(vma.mapping, VmMapping::Heap { .. })
                {
                    let heap_start = vma.start();
                    let new_heap_end = core::cmp::max(new_brk_aligned, heap_start);

                    if new_heap_end <= heap_start {
                        remove_heap_va = Some(old_brk_aligned - 1);
                    } else {
                        vma.set_bounds(heap_start, new_heap_end);
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
                    new_heap_end,
                    old_heap_end,
                );
                self.size = self
                    .size
                    .saturating_sub(old_heap_end - new_heap_end);
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
    ///
    /// Pipeline:
    /// 1. **Classify** — each parent VMA → `ForkDescriptor` (pure, no mutation)
    /// 2. **Apply** — fold descriptors into (parent rebind + child insert + pmap)
    ///    under a single `ShootdownBatch` across all writable pages.
    pub fn cow_fork_into(&mut self, child: &mut VmMap) -> Result<(), Errno> {
        // Phase 1: classify (pure — snapshot decisions before mutating anything)
        let descriptors: alloc::vec::Vec<ForkDescriptor> =
            self.iter().filter_map(ForkDescriptor::from_vma).collect();

        let child_pmap_arc = child.pmap.clone();
        let mut child_pmap = child_pmap_arc.lock();
        let parent_pmap_arc = self.pmap.clone();
        let mut parent_pmap = parent_pmap_arc.lock();

        // Phase 2: apply VMA + pmap mutations under one ShootdownBatch
        let mut batch = parent_pmap.shootdown_batch();
        descriptors.into_iter().try_for_each(|desc| {
            desc.apply(self, child, &mut parent_pmap, &mut child_pmap, &mut batch)
        })
        // batch drops here → single quiesce release + single adaptive flush
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

// ---------------------------------------------------------------------------
// COW fork descriptor — pure classification of per-VMA fork action
// ---------------------------------------------------------------------------

/// What fork does with one VMA. Produced by pure classification, consumed
/// by `apply()` which binds VMA + pmap mutations together.
enum ForkDescriptor {
    /// Read-only region: share the VmObject, copy pmap entries as-is.
    Share {
        start: VirtAddr,
        end: VirtAddr,
        prot: MapPerm,
        child_mapping: VmMapping,
    },
    /// Writable region: shadow both sides, downgrade parent pmap to RO.
    Cow {
        start: VirtAddr,
        end: VirtAddr,
        prot: MapPerm,
        parent_shadow: VmMapping,
        child_shadow: VmMapping,
    },
}

impl ForkDescriptor {
    /// Pure classification: inspect a parent VMA and decide the fork action.
    /// Returns `None` for Guard entries (nothing to inherit).
    fn from_vma(vma: &VmMapEntry) -> Option<Self> {
        let obj = vma.mapping.object()?;
        if vma.protection.contains(MapPerm::W) {
            let (parent_shadow, child_shadow) = build_cow_shadows(obj, &vma.mapping);
            Some(Self::Cow {
                start: vma.start(),
                end: vma.end(),
                prot: vma.protection,
                parent_shadow,
                child_shadow,
            })
        } else {
            Some(Self::Share {
                start: vma.start(),
                end: vma.end(),
                prot: vma.protection,
                child_mapping: vma.mapping.with_object(Arc::clone(obj)),
            })
        }
    }

    /// Apply this descriptor: rebind parent VMA + insert child VMA + sync pmap.
    /// All three mutations are bound in one call. The `batch` is shared across
    /// all descriptors so writable page downgrades use a single ShootdownBatch.
    fn apply(
        self,
        parent: &mut VmMap,
        child: &mut VmMap,
        parent_pmap: &mut crate::mm::Pmap,
        child_pmap: &mut crate::mm::Pmap,
        batch: &mut crate::mm::pmap::shootdown::ShootdownBatch,
    ) -> Result<(), Errno> {
        match self {
            Self::Share {
                start,
                end,
                prot,
                child_mapping,
            } => {
                // VMA: insert child entry (parent unchanged)
                child.insert_entry(VmMapEntry::new(start, end, child_mapping, prot))?;
                // Pmap: copy parent pages into child as-is
                for (va, pa, _) in parent_pmap.range(start, end) {
                    child_pmap.entry_or_insert(va, pa, prot).ok();
                }
            },
            Self::Cow {
                start,
                end,
                prot,
                parent_shadow,
                child_shadow,
            } => {
                // VMA: rebind parent store to shadow, insert child with its shadow
                if let Some(vma) = parent.lookup_mut(start) {
                    vma.mapping = parent_shadow;
                }
                child.insert_entry(VmMapEntry::new(start, end, child_shadow, prot))?;
                // Pmap: downgrade parent W→RO, copy to child as RO
                let ro = prot & !MapPerm::W;
                parent_pmap.for_each_in_range_mut(start, end, |mut entry| {
                    child_pmap.entry_or_insert(entry.va(), entry.pa(), ro).ok();
                    batch.add(entry.set_perm(ro));
                });
            },
        }
        Ok(())
    }
}

/// Build COW shadow VmObjects for both parent and child, preserving
/// file-backed pager on both shadow heads for demand-fault continuity.
fn build_cow_shadows(
    parent_obj: &Arc<LeveledRwLock<VmObject, 3>>,
    mapping: &VmMapping,
) -> (VmMapping, VmMapping) {
    let size_bytes = parent_obj.read().size();
    let parent_shadow = VmObject::new_shadow(Arc::clone(parent_obj), size_bytes);
    let child_shadow = VmObject::new_shadow(Arc::clone(parent_obj), size_bytes);

    // Propagate non-anonymous pager so demand faults still reach the vnode.
    {
        let parent_read = parent_obj.read();
        if let Some(ref pager) = parent_read.pager {
            if !pager.is_anon() {
                parent_shadow.write().pager = Some(Arc::clone(pager));
                child_shadow.write().pager = Some(Arc::clone(pager));
            }
        }
    }

    (
        mapping.with_object(parent_shadow),
        mapping.with_object(child_shadow),
    )
}

// ---------------------------------------------------------------------------
// Range operation helpers — shared by remove_range and protect_range
// ---------------------------------------------------------------------------

/// Result of splitting a VmMapEntry at range boundaries [clip_start, clip_end).
struct SplitResult {
    /// [entry_start, clip_start) — unchanged survivor (None if fully inside)
    left: Option<VmMapEntry>,
    /// [max(entry_start, clip_start), min(entry_end, clip_end)) — affected piece
    middle: VmMapEntry,
    /// [clip_end, entry_end) — unchanged survivor (None if fully inside)
    right: Option<VmMapEntry>,
}

/// Pure geometry: split an entry at [clip_start, clip_end) boundaries.
fn split_entry_at(entry: VmMapEntry, clip_start: VirtAddr, clip_end: VirtAddr) -> SplitResult {
    let left = (entry.start() < clip_start).then(|| {
        let mut l = entry.clone_for_split(entry.start());
        l.set_bounds(entry.start(), clip_start);
        l
    });
    let right = (entry.end() > clip_end).then(|| {
        let mut r = entry.clone_for_split(clip_end);
        r.set_bounds(clip_end, entry.end());
        r
    });
    let mid_start = core::cmp::max(entry.start(), clip_start);
    let mid_end = core::cmp::min(entry.end(), clip_end);
    let mut middle = entry.clone_for_split(mid_start);
    middle.set_bounds(mid_start, mid_end);
    SplitResult {
        left,
        middle,
        right,
    }
}

/// Collect keys of all entries overlapping [start, end).
fn overlapping_keys(vm: &VmMap, start: VirtAddr, end: VirtAddr) -> alloc::vec::Vec<VirtAddr> {
    vm.iter()
        .filter(|e| e.start() < end && e.end() > start)
        .map(|e| e.start())
        .collect()
}

/// Validate that [start, end) is fully covered by VMAs and perm ≤ max_protection.
fn validate_protect_coverage(vm: &VmMap, start: VirtAddr, end: VirtAddr, perm: MapPerm) -> Result<(), Errno> {
    let mut entries: alloc::vec::Vec<_> = vm
        .iter()
        .filter(|e| e.start() < end && e.end() > start)
        .map(|e| (e.start(), e.end(), e.max_protection))
        .collect();

    if entries.is_empty() {
        return Err(Errno::Enomem);
    }
    entries.sort_unstable_by_key(|&(s, _, _)| s);

    let mut covered = start;
    for (s, e, max_prot) in entries {
        if s > covered {
            return Err(Errno::Enomem);
        }
        if !max_prot.contains(perm) {
            return Err(Errno::Einval);
        }
        covered = core::cmp::max(covered, e);
        if covered >= end {
            return Ok(());
        }
    }
    if covered < end {
        Err(Errno::Enomem)
    } else {
        Ok(())
    }
}
