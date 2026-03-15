//! BSD-style VmObject with shadow chain support.
//!
//! VmObject holds physical pages indexed by page offset. Shadow chains
//! enable COW semantics: fork inserts a new shadow at the chain head,
//! and write faults copy pages into the topmost shadow.
//!
//! The `Drop` implementation uses iterative `Arc::try_unwrap` unwinding
//! to handle arbitrarily deep shadow chains (500+) without stack overflow.

use alloc::{collections::BTreeMap, sync::Arc};
use core::{
    marker::{Send, Sync},
    sync::atomic::AtomicU32,
};

use crate::{
    fs::Vnode,
    hal_common::{Errno, LeveledRwLock, PageNum, PhysAddr, PAGE_SIZE},
    mm::{
        pmap_copy_page, pmap_zero_page,
        vm::{page_ref::PageRef, ExclusiveBusyGuard, SharedBusyGuard},
        PageRole,
    },
};
/// Pager trait for clustered I/O operations (BSD vm_pager interface).
/// Supports fetching multiple pages in a single operation for efficiency.
pub trait Pager: core::fmt::Debug + Send + Sync {
    /// Fetch pages from backing store.
    fn page_in(
        &self,
        offset: usize,
        pa: PhysAddr,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>;
    /// Write pages to backing store.
    ///
    /// `len` is the number of valid bytes in the page to write (may be less
    /// than `PAGE_SIZE` for the last page of a file).
    fn page_out(
        &self,
        offset: usize,
        pa: PhysAddr,
        len: usize,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>;

    /// Returns true if this is an anonymous pager.
    fn is_anon(&self) -> bool {
        false
    }
}

/// Anonymous Pager.
#[derive(Debug)]
pub struct AnonPager;

impl Pager for AnonPager {
    fn is_anon(&self) -> bool {
        true
    }

    fn page_in(
        &self,
        _offset: usize,
        pa: PhysAddr,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>
    {
        alloc::boxed::Box::pin(async move {
            pmap_zero_page(pa);
            Ok(())
        })
    }

    fn page_out(
        &self,
        _offset: usize,
        _pa: PhysAddr,
        _len: usize,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>
    {
        alloc::boxed::Box::pin(async move {
            // Anon pages are never paged out currently.
            Ok(())
        })
    }
}

/// Vnode Pager for file-backed pages.
#[derive(Debug)]
pub struct VnodePager {
    pub vnode_id: usize,
    pub path: alloc::string::String,
    /// File offset that corresponds to object offset 0.
    pub base_offset: usize,
    /// Number of valid file-backed bytes from `base_offset`.
    /// Bytes beyond this range are treated as zero-fill (ELF BSS tail).
    pub valid_bytes: usize,
}

impl Pager for VnodePager {
    fn page_in(
        &self,
        offset: usize,
        pa: PhysAddr,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>
    {
        let path = self.path.clone();
        let base_offset = self.base_offset;
        let valid_bytes = self.valid_bytes;
        alloc::boxed::Box::pin(async move {
            let rel = offset.saturating_sub(base_offset);
            let file_bytes_this_page = if valid_bytes == usize::MAX {
                PAGE_SIZE
            } else if rel >= valid_bytes {
                0
            } else {
                core::cmp::min(PAGE_SIZE, valid_bytes - rel)
            };

            // Pure zero-fill page (beyond p_filesz or zero-file segment).
            if file_bytes_this_page == 0 {
                pmap_zero_page(pa);
                return Ok(());
            }

            crate::fs::fs_read_page(&path, offset as u64, pa)
                .await
                .map_err(|_| ())?;

            // Clamp page tail to zero when it crosses the file-backed boundary.
            if file_bytes_this_page < PAGE_SIZE {
                let buf =
                    unsafe { core::slice::from_raw_parts_mut(pa.as_usize() as *mut u8, PAGE_SIZE) };
                buf[file_bytes_this_page..].fill(0);
            }
            Ok(())
        })
    }

    fn page_out(
        &self,
        offset: usize,
        pa: PhysAddr,
        len: usize,
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>
    {
        let path = self.path.clone();
        alloc::boxed::Box::pin(async move {
            let write_len = core::cmp::min(len, PAGE_SIZE);
            let data =
                unsafe { core::slice::from_raw_parts(pa.as_usize() as *const u8, write_len) };
            crate::fs::fs_write_at(&path, offset as u64, data)
                .await
                .map(|_| ())
                .map_err(|_| ())
        })
    }
}

// ---------------------------------------------------------------------------
// VmObject
// ---------------------------------------------------------------------------

/// Core VM object: a collection of physical pages indexed by page offset,
/// with an optional backing (parent) object forming a shadow chain.
///
/// `shadow_count` tracks how many shadow objects point to this object as
/// their backing. Used by `collapse()` to determine when page migration
/// is safe (BSD vm_object_collapse semantics).
///
/// ## Lock Ordering Invariant (D-4)
///
/// VmObject locks (level 3) must only be acquired in **child → parent**
/// (upward) direction along the shadow chain.  `collapse()` holds `self`
/// (child) and acquires `self.backing` (parent) — always upward.
///
/// The reverse direction (parent → child) is **structurally impossible**:
/// VmObject has a `backing` pointer to its parent but no `children` field.
/// There is no API that traverses parent → child.  This makes the
/// invariant self-enforcing — you cannot violate it without first adding
/// a downward pointer, which would require updating this documentation
/// and the lock ordering analysis.
///
/// If a downward pointer is ever needed (e.g. for page daemon scanning),
/// it must use a separate lock level or `try_write()` to avoid AB/BA
/// deadlock between concurrent collapse operations on the same chain.
pub struct VmObject {
    /// Pages owned directly by this object, keyed by page offset (in pages).
    /// Each `PageRef` is a move-only handle: dropping it decrements the
    /// FRAME_META refcount and frees the physical frame automatically.
    pages: BTreeMap<VObjIndex, PageRef>,
    pub pager: Option<Arc<dyn Pager>>,

    /// Parent in the shadow chain (for COW).
    backing: Option<Arc<LeveledRwLock<VmObject, 3>>>,
    /// How many shadow objects use this as their backing.
    shadow_count: usize,

    /// Object size in bytes.
    size: usize,
    /// Number of pages resident in *this* object (not backing).
    resident_count: usize,
    /// I/O barrier: number of pages currently being paged in.
    paging_in_progress: AtomicU32,
    /// Dirty tracking generation counters.
    generation: AtomicU32,
    clean_generation: AtomicU32,
}

/// Index into VmObject, in units of pages (not bytes).
///
/// This is a proper newtype — distinct from `PageNum`, `PhysPageNum`,
/// and `VirtPageNum` at the type level. A physical frame number or
/// virtual page number cannot be silently passed where a `VObjIndex`
/// is expected.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VObjIndex(usize);

impl VObjIndex {
    pub const fn new(n: usize) -> Self {
        Self(n)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    /// Convert a `PageNum` to `VObjIndex` — explicit, never implicit.
    pub const fn from_page_num(pn: PageNum) -> Self {
        Self(pn.as_usize())
    }

    /// Convert byte offset to object page index (floor division).
    pub const fn from_bytes_floor(bytes: usize) -> Self {
        Self(bytes / PAGE_SIZE)
    }

    /// Convert byte offset to object page index (ceiling division).
    pub const fn from_bytes_ceil(bytes: usize) -> Self {
        Self((bytes + PAGE_SIZE - 1) / PAGE_SIZE)
    }

    /// Convert object page index to byte offset.
    pub const fn to_bytes(self) -> usize {
        self.0 * PAGE_SIZE
    }
}

impl core::ops::Add<usize> for VObjIndex {
    type Output = Self;
    fn add(self, rhs: usize) -> Self {
        Self(self.0 + rhs)
    }
}

impl core::ops::AddAssign<usize> for VObjIndex {
    fn add_assign(&mut self, rhs: usize) {
        self.0 += rhs;
    }
}

impl core::ops::Sub<usize> for VObjIndex {
    type Output = Self;
    fn sub(self, rhs: usize) -> Self {
        assert!(self.0 >= rhs, "VObjIndex underflow");
        Self(self.0 - rhs)
    }
}

impl core::ops::Sub<VObjIndex> for VObjIndex {
    type Output = usize;
    fn sub(self, rhs: VObjIndex) -> usize {
        assert!(self.0 >= rhs.0, "VObjIndex underflow");
        self.0 - rhs.0
    }
}

impl core::fmt::Display for VObjIndex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl VmObject {
    /// Create a new anonymous VmObject (no backing).
    pub fn new_anon(size: usize) -> Arc<LeveledRwLock<Self, 3>> {
        Arc::new(LeveledRwLock::new(Self {
            pages: BTreeMap::new(),
            backing: None,
            shadow_count: 0,
            pager: Some(Arc::new(AnonPager)),
            size,
            resident_count: 0,
            paging_in_progress: AtomicU32::new(0),
            generation: AtomicU32::new(0),
            clean_generation: AtomicU32::new(0),
        }))
    }

    /// Create a new file-backed VmObject.
    pub fn new_file(vnode: &(impl Vnode + ?Sized)) -> Arc<LeveledRwLock<Self, 3>> {
        Arc::new(LeveledRwLock::new(Self {
            pages: BTreeMap::new(),
            backing: None,
            shadow_count: 0,
            pager: Some(Arc::new(VnodePager {
                vnode_id: vnode.vnode_id() as usize,
                path: vnode.path().into(),
                base_offset: 0,
                valid_bytes: usize::MAX,
            })),
            size: vnode.size() as usize,
            resident_count: 0,
            paging_in_progress: AtomicU32::new(0),
            generation: AtomicU32::new(0),
            clean_generation: AtomicU32::new(0),
        }))
    }

    /// Create a file-backed VmObject for a specific region of a vnode.
    ///
    /// `base_offset` is the file offset corresponding to object page 0.
    /// `valid_bytes` controls how many bytes from `base_offset` are file-backed;
    /// bytes beyond that range are zero-filled (ELF BSS tail semantics).
    pub fn new_vnode_region(
        vnode_id: usize,
        path: &str,
        size_pages: usize,
        base_offset: usize,
        valid_bytes: usize,
    ) -> Arc<LeveledRwLock<Self, 3>> {
        Arc::new(LeveledRwLock::new(Self {
            pages: BTreeMap::new(),
            backing: None,
            shadow_count: 0,
            pager: Some(Arc::new(VnodePager {
                vnode_id,
                path: path.into(),
                base_offset,
                valid_bytes,
            })),
            size: size_pages * PAGE_SIZE,
            resident_count: 0,
            paging_in_progress: AtomicU32::new(0),
            generation: AtomicU32::new(0),
            clean_generation: AtomicU32::new(0),
        }))
    }

    /// Create a shadow object in front of `parent` (for fork COW).
    ///
    /// The new shadow starts empty; page lookups walk through to the parent.
    /// Increments parent's `shadow_count`.
    pub fn new_shadow(
        parent: Arc<LeveledRwLock<VmObject, 3>>,
        size: usize,
    ) -> Arc<LeveledRwLock<Self, 3>> {
        {
            let mut p = parent.write();
            p.shadow_count += 1;
        }
        Arc::new(LeveledRwLock::new(Self {
            pages: BTreeMap::new(),
            backing: Some(parent),
            shadow_count: 0,
            pager: Some(Arc::new(AnonPager)),
            size,
            resident_count: 0,
            paging_in_progress: AtomicU32::new(0),
            generation: AtomicU32::new(0),
            clean_generation: AtomicU32::new(0),
        }))
    }

    /// Look up a page by offset, walking the shadow chain iteratively.
    ///
    /// Returns the physical address if found in this object or any ancestor.
    pub fn lookup_page(&self, index: VObjIndex) -> Option<PhysAddr> {
        // Check this object first.
        if let Some(pr) = self.pages.get(&index) {
            return Some(pr.phys());
        }
        // Walk the backing chain iteratively.
        let mut current = self.backing.as_ref().map(Arc::clone);
        while let Some(arc) = current {
            let obj = arc.read();
            if let Some(pr) = obj.pages.get(&index) {
                return Some(pr.phys());
            }
            current = obj.backing.as_ref().map(Arc::clone);
        }
        None
    }

    /// Traverse shadow chain to find a page (BSD-style backing traversal).
    pub fn lookup_page_in_chain(&self, pindex: VObjIndex) -> Option<PhysAddr> {
        self.lookup_page(pindex)
    }

    /// Traverses the shadow chain to find a page or allocate one if missing.
    /// This method is static, taking an `Arc<LeveledRwLock<VmObject, 3>>` and dropping the read
    /// lock during sleeps to avoid deadlocking the object tree.
    ///
    /// Returns `(PhysAddr, bool)` — the PA and whether the page is newly allocated
    /// (held with exBusy). Existing pages are returned without any busy lock.
    pub async fn grab_for_fault(
        top_object: Arc<LeveledRwLock<Self, 3>>,
        pindex: VObjIndex,
    ) -> Result<(PhysAddr, bool), Errno> {
        loop {
            // Quick pass: look for page in the shadow chain.
            let mut found_pa: Option<PhysAddr> = None;
            let mut current = Some(Arc::clone(&top_object));

            while let Some(arc) = current {
                let obj = arc.read();
                if let Some(pr) = obj.pages.get(&pindex) {
                    found_pa = Some(pr.phys());
                    break;
                }
                current = obj.backing.as_ref().map(Arc::clone);
            }

            if let Some(pa) = found_pa {
                // Page exists. Check if it's exclusively busied.
                if let Some(meta) = crate::mm::get_frame_meta(pa) {
                    if meta.is_exclusive_busied() {
                        // Wait for it to become available.
                        crate::executor::yield_now().await;
                        meta.wait_for_exclusive_unlock().await;
                        // Loop again because the page might have been removed while we slept.
                        continue;
                    }
                }
                return Ok((pa, false));
            } else {
                // Page not found in shadow chain. We must allocate and insert it into top_object.
                let mut root = top_object.write();
                // Double check in case another thread inserted it while we escalated the lock.
                if let Some(pr) = root.pages.get(&pindex) {
                    let pa = pr.phys();
                    if let Some(meta) = crate::mm::get_frame_meta(pa) {
                        if meta.is_exclusive_busied() {
                            drop(root);
                            crate::executor::yield_now().await;
                            meta.wait_for_exclusive_unlock().await;
                            continue;
                        }
                    }
                    return Ok((pa, false));
                }

                // Allocate a new frame.
                let phys =
                    crate::mm::alloc_raw_frame_sync(PageRole::UserAnon).ok_or(Errno::Enomem)?;

                // Acquire exBusy on the FRAME_META entry (born locked).
                if let Some(meta) = crate::mm::get_frame_meta(phys) {
                    assert!(
                        meta.try_acquire_exclusive(),
                        "grab_for_fault: freshly allocated frame should not be busied"
                    );
                }

                root.insert_page(pindex, PageRef::new(phys));
                return Ok((phys, true));
            }
        }
    }

    /// Fetch a filled page. When kernel needs a page from uvm, it calls this function.
    /// Returns the PhysAddr of the page (with exBusy released).
    pub async fn fetch_page_async(
        obj_arc: Arc<LeveledRwLock<Self, 3>>,
        pindex: VObjIndex,
    ) -> Result<PhysAddr, Errno> {
        let (pa, is_new) = Self::grab_for_fault(Arc::clone(&obj_arc), pindex).await?;

        if is_new {
            // New page — fill it via pager.
            let pager = {
                let obj = obj_arc.read();
                obj.pager.clone()
            };

            if let Some(pager) = pager {
                let offset_bytes = pindex.to_bytes();

                if pager.page_in(offset_bytes, pa).await.is_err() {
                    // Release exBusy before returning error.
                    if let Some(meta) = crate::mm::get_frame_meta(pa) {
                        meta.release_exclusive();
                    }
                    return Err(Errno::Eio);
                }
            } else {
                if let Some(meta) = crate::mm::get_frame_meta(pa) {
                    meta.release_exclusive();
                }
                kprintln!(
                    "[ERROR]VmObject fetch_page_async: no pager for new page at index {}",
                    pindex
                );
                panic!();
            }

            // Release exBusy — page is now valid.
            if let Some(meta) = crate::mm::get_frame_meta(pa) {
                meta.release_exclusive();
            }
        }

        Ok(pa)
    }
    /// Lookup or allocate an anonymous page for the given offset.
    /// Emits `TraceEvent::Alloc { usage: UserAnon }` upon allocation.
    pub fn fault_allocate_anon(&mut self, index: VObjIndex) -> Result<PhysAddr, Errno> {
        match self.pages.entry(index) {
            alloc::collections::btree_map::Entry::Occupied(e) => Ok(e.get().phys()),
            alloc::collections::btree_map::Entry::Vacant(e) => {
                let phys =
                    crate::mm::alloc_raw_frame_sync(PageRole::UserAnon).ok_or(Errno::Enomem)?;
                pmap_zero_page(phys);
                crate::klog!(
                    vm,
                    debug,
                    "STUB: TraceEvent::Alloc {{ usage: UserAnon }} (fault_allocate_anon offset {})",
                    index
                );
                e.insert(PageRef::new(phys));
                self.resident_count += 1;
                Ok(phys)
            },
        }
    }

    /// Implement COW by copying the old_phys page into a newly allocated frame.
    /// Emits `TraceEvent::Alloc { usage: UserAnon }` upon allocation.
    pub fn fault_cow(&mut self, index: VObjIndex, old_phys: PhysAddr) -> Result<PhysAddr, Errno> {
        match self.pages.entry(index) {
            alloc::collections::btree_map::Entry::Occupied(e) => Ok(e.get().phys()),
            alloc::collections::btree_map::Entry::Vacant(e) => {
                let phys =
                    crate::mm::alloc_raw_frame_sync(PageRole::UserAnon).ok_or(Errno::Enomem)?;
                pmap_copy_page(old_phys, phys);
                crate::klog!(
                    vm,
                    debug,
                    "STUB: TraceEvent::Alloc {{ usage: UserAnon }} (fault_cow offset {})",
                    index
                );
                e.insert(PageRef::new(phys));
                self.resident_count += 1;
                Ok(phys)
            },
        }
    }

    /// Look up a page in this object (not the backing chain).
    pub fn get_page(&self, index: VObjIndex) -> Option<&PageRef> {
        self.pages.get(&index)
    }

    /// Insert a page into this object.
    ///
    /// If a page already exists at this index, the old `PageRef` is dropped,
    /// which automatically decrements the FRAME_META refcount and frees
    /// the physical frame if no other references remain.
    pub fn insert_page(&mut self, index: VObjIndex, page: PageRef) {
        if self.pages.insert(index, page).is_none() {
            self.resident_count += 1;
        }
    }

    /// Remove a page from this object only (does not touch backing).
    /// Returns the `PageRef` for the caller to handle teardown.
    /// Decrements `resident_count` if the page was present.
    pub fn remove_page(&mut self, index: VObjIndex) -> Option<PageRef> {
        let removed = self.pages.remove(&index);
        if removed.is_some() {
            self.resident_count = self.resident_count.saturating_sub(1);
        }
        removed
    }

    /// Look up a page in this object (not the backing chain) and acquire a
    /// shared busy lock (sBusy) via RAII guard.
    ///
    /// Returns `None` if the page is not found or the busy lock is contended.
    /// Does NOT walk the shadow chain -- the caller handles chain traversal
    /// for COW scenarios.
    pub fn lookup_page_guarded(&self, index: VObjIndex) -> Option<SharedBusyGuard> {
        let pr = self.pages.get(&index)?;
        SharedBusyGuard::try_new(pr.phys())
    }

    /// Look up a page in this object (not the backing chain) and acquire an
    /// exclusive busy lock (exBusy) via RAII guard.
    ///
    /// Returns `None` if the page is not found or the busy lock is contended.
    pub fn grab_page_guarded(&self, index: VObjIndex) -> Option<ExclusiveBusyGuard> {
        let pr = self.pages.get(&index)?;
        ExclusiveBusyGuard::try_new(pr.phys())
    }

    /// Count the shadow chain depth (for debug/testing).
    #[allow(unused)]
    pub fn shadow_depth(&self) -> usize {
        let mut depth = 0;
        let mut current = self.backing.as_ref().map(Arc::clone);
        while let Some(arc) = current {
            depth += 1;
            let obj = arc.read();
            current = obj.backing.as_ref().map(Arc::clone);
        }
        depth
    }

    /// Get the object size in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Get the number of pages resident in this object.
    #[allow(unused)]
    pub fn resident_count(&self) -> usize {
        self.resident_count
    }

    /// Get a reference to the backing object (if any).
    pub fn backing(&self) -> Option<&Arc<LeveledRwLock<VmObject, 3>>> {
        self.backing.as_ref()
    }

    /// Return a clone of the backing object Arc (for shadow chain traversal).
    pub fn backing_object(&self) -> Option<Arc<LeveledRwLock<VmObject, 3>>> {
        self.backing.as_ref().map(Arc::clone)
    }

    /// Get the number of shadows pointing to this object.
    pub fn shadow_count(&self) -> usize {
        self.shadow_count
    }

    /// Check if this object (not backing) has a page at the given offset.
    pub fn has_page(&self, index: VObjIndex) -> bool {
        self.pages.contains_key(&index)
    }

    /// BSD vm_object_collapse: migrate pages from backing into self.
    ///
    /// Precondition: caller holds `&mut self` (write lock on this object),
    /// and `backing.shadow_count == 1` (we are the sole shadow).
    ///
    /// **Lock ordering (D-4):** This method acquires `backing.write()` while
    /// holding `self.write()` — two level-3 locks. This is safe because the
    /// direction is always child → parent (upward). The reverse is
    /// structurally impossible (no `children` field exists). See the
    /// VmObject struct doc comment for the full invariant.
    ///
    /// Pages in backing that conflict with pages already in self (COW copies)
    /// are freed. Non-conflicting pages are renamed (moved) into self.
    /// After migration, self adopts backing's backing (chain shortening).
    /// Note: we only migrate pages that are not busied (exBusy or sBusy).
    pub fn collapse(&mut self) {
        let backing_arc = match self.backing.take() {
            Some(arc) => arc,
            None => return,
        };

        let mut backing = backing_arc.write();

        // Only collapse if we are the sole shadow.
        if backing.shadow_count != 1 {
            // Put it back — not safe to collapse.
            drop(backing);
            self.backing = Some(backing_arc);
            return;
        }

        // Migrate pages from backing into self.
        let backing_pages = core::mem::take(&mut backing.pages);
        for (offset, page_ref) in backing_pages {
            // Refuse to migrate if backing page is busied (exBusy or sBusy).
            // Checking sBusy fixes D-2 (collapse ignoring sBusy).
            let meta = page_ref.meta();
            if meta.is_exclusive_busied() || meta.is_shared_busied() {
                // Return it to backing — not safe to migrate.
                backing.pages.insert(offset, page_ref);
                continue;
            }

            if let alloc::collections::btree_map::Entry::Vacant(e) = self.pages.entry(offset) {
                // No conflict: rename page from backing to self.
                backing.resident_count = backing.resident_count.saturating_sub(1);
                e.insert(page_ref);
                self.resident_count += 1;
            } else {
                // Conflict: self already has a COW copy at this offset.
                // This backing page has become a phantom: no shadow chain can
                // reach it anymore once collapse succeeds, so release its frame.
                // PageRef::drop handles deallocation automatically.
                backing.resident_count = backing.resident_count.saturating_sub(1);
                drop(page_ref);
            }
        }

        // Adopt backing's backing (skip over the now-empty object).
        // Decrement shadow_count on backing (we're detaching from it).
        backing.shadow_count -= 1;

        // Take grandparent from backing. This severs backing→grandparent,
        // so backing's Drop won't decrement grandparent's shadow_count.
        // We're transferring that shadow relationship to self, so
        // grandparent's shadow_count stays unchanged (no increment needed).
        let grandparent = backing.backing.take();
        drop(backing);
        // backing_arc may be dropped here — if Arc refcount hits 0,
        // VmObject::drop runs but pages are already drained and backing is None.

        self.backing = grandparent;
    }

    // -----------------------------------------------------------------------
    // Dirty tracking (UBC — Unified Buffer Cache)
    // -----------------------------------------------------------------------

    /// Increment modification generation. Called after any page is dirtied.
    /// Uses AtomicU32 — no write lock required (works under read lock).
    pub fn bump_generation(&self) {
        self.generation
            .fetch_add(1, core::sync::atomic::Ordering::Release);
    }

    /// Has any page been modified since last writeback?
    pub fn is_dirty(&self) -> bool {
        self.generation.load(core::sync::atomic::Ordering::Acquire)
            != self
                .clean_generation
                .load(core::sync::atomic::Ordering::Acquire)
    }

    /// Snapshot current generation as clean. Called after all dirty pages flushed.
    pub fn mark_clean(&self) {
        self.clean_generation.store(
            self.generation.load(core::sync::atomic::Ordering::Acquire),
            core::sync::atomic::Ordering::Release,
        );
    }

    /// Collect dirty pages for writeback. Returns `(page-index, phys-addr)` pairs.
    /// Caller must hold at least a read lock on this VmObject.
    pub fn collect_dirty_pages(&self) -> alloc::vec::Vec<(VObjIndex, PhysAddr)> {
        self.pages
            .iter()
            .filter(|(_, pr)| pr.meta().is_dirty())
            .map(|(idx, pr)| (*idx, pr.phys()))
            .collect()
    }

    /// Update the object size in bytes.
    pub fn set_size(&mut self, new_size: usize) {
        self.size = new_size;
    }

    /// Remove and return all pages at offsets >= `from_page`.
    /// Only operates on this object (not the backing chain).
    /// Removed `PageRef`s are dropped, which frees the physical frames.
    pub fn truncate_pages(&mut self, from_index: VObjIndex) {
        let keys: alloc::vec::Vec<VObjIndex> =
            self.pages.range(from_index..).map(|(&k, _)| k).collect();
        for k in keys {
            if self.pages.remove(&k).is_some() {
                self.resident_count -= 1;
                // PageRef::drop handles frame deallocation automatically.
            }
        }
    }

    /// Iterate over all pages directly owned by this object (not backing).
    pub fn pages_iter(&self) -> impl Iterator<Item = &PageRef> {
        self.pages.values()
    }

    /// Iterate over (index, page) pairs for fork deep-copy.
    pub fn pages_with_index(&self) -> impl Iterator<Item = (VObjIndex, &PageRef)> {
        self.pages.iter().map(|(idx, pr)| (*idx, pr))
    }
}

/// Iterative Drop: unwind the shadow chain without recursion.
///
/// When this VmObject is dropped, we walk the backing chain and try to
/// exclusively own each ancestor via `Arc::try_unwrap`. If successful,
/// we free its anonymous pages and continue. If another reference exists,
/// we stop (that ancestor is still shared).
impl Drop for VmObject {
    fn drop(&mut self) {
        // Our owned pages (PageRef entries) are freed automatically when
        // self.pages is dropped. Each PageRef::drop decrements the FRAME_META
        // refcount and returns the physical frame to the buddy allocator if
        // the count hits zero. We just need to handle the shadow chain
        // unwinding.

        // Decrement backing's shadow_count, then release the lock
        // before taking the Arc (which may trigger further drops).
        if let Some(ref backing_arc) = self.backing {
            let mut backing = backing_arc.write();
            backing.shadow_count = backing.shadow_count.saturating_sub(1);
            drop(backing); // release write lock before Arc::try_unwrap
        }

        // Iteratively unwind the backing chain.
        let mut current = self.backing.take();
        while let Some(arc) = current {
            match Arc::try_unwrap(arc) {
                Ok(rwlock) => {
                    let mut obj = rwlock.into_inner();
                    // Take backing to continue unwinding cleanly without recursion.
                    // obj's own `pages` drop automatically here when `obj` is dropped.
                    // Decrement next ancestor's shadow_count.
                    if let Some(ref next_arc) = obj.backing {
                        let mut next = next_arc.write();
                        next.shadow_count = next.shadow_count.saturating_sub(1);
                        drop(next); // release before taking Arc
                    }
                    current = obj.backing.take();
                },
                Err(_) => break,
            }
        }
    }
}

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;
    use crate::mm::vm::page_ref::PageRef;

    #[test]
    fn new_object_is_empty() {
        let obj = VmObject::new(4096);
        let r = obj.read();
        assert_eq!(r.size(), 4096);
        assert_eq!(r.resident_count(), 0);
        assert!(r.lookup_page(0).is_none());
    }

    #[test]
    fn insert_and_lookup_page() {
        let obj = VmObject::new(8192);
        {
            let mut w = obj.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0x1000)));
            w.insert_page(1, PageRef::new(PhysAddr::new(0x2000)));
        }
        let r = obj.read();
        assert_eq!(r.resident_count(), 2);
        assert_eq!(r.lookup_page(0).unwrap(), PhysAddr::new(0x1000));
        assert_eq!(r.lookup_page(1).unwrap(), PhysAddr::new(0x2000));
        assert!(r.lookup_page(2).is_none());
    }

    #[test]
    fn shadow_chain_creation() {
        let parent = VmObject::new(4096);
        {
            let mut w = parent.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0x1000)));
        }
        let shadow = VmObject::new_shadow(Arc::clone(&parent), 4096);
        // Shadow walks backing chain, so parent's page is visible
        {
            let r = shadow.read();
            assert_eq!(r.resident_count(), 0);
            assert_eq!(r.lookup_page(0).unwrap(), PhysAddr::new(0x1000));
        }
        // Parent page still accessible
        {
            let r = parent.read();
            assert_eq!(r.lookup_page(0).unwrap(), PhysAddr::new(0x1000));
        }
    }

    #[test]
    fn shadow_chain_lookup_iterative() {
        let grandparent = VmObject::new(4096);
        {
            let mut w = grandparent.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0xA000)));
        }
        let parent = VmObject::new_shadow(Arc::clone(&grandparent), 4096);
        {
            let mut w = parent.write();
            w.insert_page(1, PageRef::new(PhysAddr::new(0xB000)));
        }
        let child = VmObject::new_shadow(Arc::clone(&parent), 4096);
        {
            let mut w = child.write();
            w.insert_page(2, PageRef::new(PhysAddr::new(0xC000)));
        }
        // Child has page 2, parent has page 1, grandparent has page 0
        let r = child.read();
        assert_eq!(r.lookup_page(2).unwrap(), PhysAddr::new(0xC000));
        // lookup_page walks backing chain, so grandparent's page 0 is visible
        assert_eq!(r.lookup_page(0).unwrap(), PhysAddr::new(0xA000));
    }

    #[test]
    fn iterative_drop_deep_chain_500() {
        // Build a 500-deep shadow chain. If Drop were recursive, this
        // would overflow the stack. Iterative Drop handles it.
        let mut current = VmObject::new(4096);
        for i in 0..500 {
            let shadow = VmObject::new_shadow(Arc::clone(&current), 4096);
            {
                let mut w = shadow.write();
                w.insert_page(i as u64, PageRef::new(PhysAddr::new((i + 1) * 0x1000)));
            }
            current = shadow;
        }
        // Drop the entire chain — should not stack overflow
        drop(current);
    }

    #[test]
    fn iterative_drop_shared_stops_at_refcount() {
        let parent = VmObject::new(4096);
        let shadow1 = VmObject::new_shadow(Arc::clone(&parent), 4096);
        let shadow2 = VmObject::new_shadow(Arc::clone(&parent), 4096);
        // Drop shadow1 — parent still has refcount > 1 (shadow2 holds it)
        drop(shadow1);
        // Parent should still be alive
        assert_eq!(Arc::strong_count(&parent), 2); // shadow2 + parent
        drop(shadow2);
        assert_eq!(Arc::strong_count(&parent), 1);
    }

    #[test]
    fn insert_page_overwrites_existing() {
        let obj = VmObject::new(4096);
        {
            let mut w = obj.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0x1000)));
            w.insert_page(0, PageRef::new(PhysAddr::new(0x2000)));
        }
        let r = obj.read();
        assert_eq!(r.lookup_page(0).unwrap(), PhysAddr::new(0x2000));
    }

    #[test]
    fn iterative_drop_1000_deep() {
        // Stress test: 1000-deep chain
        let mut current = VmObject::new(4096);
        for _ in 0..1000 {
            current = VmObject::new_shadow(Arc::clone(&current), 4096);
        }
        drop(current);
    }

    #[test]
    fn shadow_override_hides_parent_page() {
        let parent = VmObject::new(4096);
        {
            let mut w = parent.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0xAAAA_0000)));
        }
        let shadow = VmObject::new_shadow(Arc::clone(&parent), 4096);
        {
            let mut w = shadow.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0xBBBB_0000)));
        }
        let r = shadow.read();
        assert_eq!(r.lookup_page(0).unwrap(), PhysAddr::new(0xBBBB_0000));
        // Parent's page is still there
        let rp = parent.read();
        assert_eq!(rp.lookup_page(0).unwrap(), PhysAddr::new(0xAAAA_0000));
    }

    #[test]
    fn shadow_depth_reports_chain_length() {
        let root = VmObject::new(4096);
        assert_eq!(root.read().shadow_depth(), 0);
        let s1 = VmObject::new_shadow(Arc::clone(&root), 4096);
        assert_eq!(s1.read().shadow_depth(), 1);
        let s2 = VmObject::new_shadow(Arc::clone(&s1), 4096);
        assert_eq!(s2.read().shadow_depth(), 2);
        let s3 = VmObject::new_shadow(Arc::clone(&s2), 4096);
        assert_eq!(s3.read().shadow_depth(), 3);
    }

    #[test]
    fn iterative_drop_600_deep() {
        // 600-deep chain — verifies iterative Drop beyond the 500 threshold
        let mut current = VmObject::new(4096);
        for i in 0..600 {
            let shadow = VmObject::new_shadow(Arc::clone(&current), 4096);
            {
                let mut w = shadow.write();
                w.insert_page(i as u64, PageRef::new(PhysAddr::new((i + 1) * 0x1000)));
            }
            current = shadow;
        }
        drop(current);
    }

    #[test]
    fn remove_page_decrements_resident_count() {
        let obj = VmObject::new(4096);
        {
            let mut w = obj.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0x1000)));
            w.insert_page(1, PageRef::new(PhysAddr::new(0x2000)));
            assert_eq!(w.resident_count(), 2);
            let removed = w.remove_page(0);
            assert!(removed.is_some());
            assert_eq!(w.resident_count(), 1);
        }
    }

    #[test]
    fn remove_nonexistent_page_returns_none() {
        let obj = VmObject::new(4096);
        let mut w = obj.write();
        assert!(w.remove_page(42).is_none());
        assert_eq!(w.resident_count(), 0);
    }

    #[test]
    fn backing_returns_parent_ref() {
        let parent = VmObject::new(4096);
        let shadow = VmObject::new_shadow(Arc::clone(&parent), 4096);
        let r = shadow.read();
        assert!(r.backing().is_some());
        let root = VmObject::new(4096);
        let rr = root.read();
        assert!(rr.backing().is_none());
    }

    // -----------------------------------------------------------------------
    // shadow_count tracking
    // -----------------------------------------------------------------------

    #[test]
    fn shadow_count_incremented_on_new_shadow() {
        let parent = VmObject::new(4096);
        assert_eq!(parent.read().shadow_count(), 0);
        let s1 = VmObject::new_shadow(Arc::clone(&parent), 4096);
        assert_eq!(parent.read().shadow_count(), 1);
        let s2 = VmObject::new_shadow(Arc::clone(&parent), 4096);
        assert_eq!(parent.read().shadow_count(), 2);
        drop(s1);
        assert_eq!(parent.read().shadow_count(), 1);
        drop(s2);
        assert_eq!(parent.read().shadow_count(), 0);
    }

    // -----------------------------------------------------------------------
    // Collapse
    // -----------------------------------------------------------------------

    #[test]
    fn collapse_migrates_pages_from_backing() {
        let backing = VmObject::new(8192);
        {
            let mut w = backing.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0xA000)));
            w.insert_page(1, PageRef::new(PhysAddr::new(0xB000)));
        }
        // Single shadow — shadow_count == 1, collapse is safe.
        let shadow = VmObject::new_shadow(Arc::clone(&backing), 8192);
        assert_eq!(backing.read().shadow_count(), 1);

        // Collapse: pages should migrate from backing into shadow.
        {
            let mut w = shadow.write();
            w.collapse();
            assert_eq!(w.resident_count(), 2);
            assert_eq!(w.lookup_page(0).unwrap(), PhysAddr::new(0xA000));
            assert_eq!(w.lookup_page(1).unwrap(), PhysAddr::new(0xB000));
            assert!(w.backing().is_none()); // chain shortened
        }
    }

    #[test]
    fn collapse_frees_phantom_on_conflict() {
        // Backing has page at offset 0 (the "phantom").
        let backing = VmObject::new(8192);
        {
            let mut w = backing.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0xDEAD)));
            w.insert_page(1, PageRef::new(PhysAddr::new(0xBEEF)));
        }
        let shadow = VmObject::new_shadow(Arc::clone(&backing), 8192);
        // Shadow has its own page at offset 0 (COW copy).
        {
            let mut w = shadow.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0xC000)));
        }

        // Collapse: offset 0 conflicts (shadow wins), offset 1 migrates.
        {
            let mut w = shadow.write();
            w.collapse();
            // Shadow keeps its own page at offset 0.
            assert_eq!(w.lookup_page(0).unwrap(), PhysAddr::new(0xC000));
            // Offset 1 migrated from backing.
            assert_eq!(w.lookup_page(1).unwrap(), PhysAddr::new(0xBEEF));
            assert_eq!(w.resident_count(), 2);
            assert!(w.backing().is_none());
        }
        // Backing's pages were drained — only the shadow's Arc remains.
        assert_eq!(backing.read().resident_count(), 0);
    }

    #[test]
    fn collapse_refuses_when_shadow_count_gt_1() {
        let backing = VmObject::new(4096);
        {
            let mut w = backing.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0x1000)));
        }
        let s1 = VmObject::new_shadow(Arc::clone(&backing), 4096);
        let _s2 = VmObject::new_shadow(Arc::clone(&backing), 4096);
        assert_eq!(backing.read().shadow_count(), 2);

        // Collapse should be refused — backing has 2 shadows.
        {
            let mut w = s1.write();
            w.collapse();
            // Page should still be in backing, not migrated.
            assert_eq!(w.resident_count(), 0);
            assert!(w.backing().is_some());
        }
    }

    #[test]
    fn collapse_adopts_grandparent() {
        // grandparent -> parent -> child
        let grandparent = VmObject::new(4096);
        {
            let mut w = grandparent.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0xAAAA)));
        }
        let parent = VmObject::new_shadow(Arc::clone(&grandparent), 4096);
        {
            let mut w = parent.write();
            w.insert_page(1, PageRef::new(PhysAddr::new(0xBBBB)));
        }
        let child = VmObject::new_shadow(Arc::clone(&parent), 4096);
        assert_eq!(parent.read().shadow_count(), 1);
        assert_eq!(grandparent.read().shadow_count(), 1);

        // Collapse child <- parent: child absorbs parent's pages,
        // and now points directly to grandparent.
        {
            let mut w = child.write();
            w.collapse();
            assert_eq!(w.resident_count(), 1); // page 1 from parent
            assert_eq!(w.lookup_page(1).unwrap(), PhysAddr::new(0xBBBB));
            // Page 0 is in grandparent, visible via chain walk.
            assert_eq!(w.lookup_page(0).unwrap(), PhysAddr::new(0xAAAA));
            assert_eq!(w.shadow_depth(), 1); // child -> grandparent
        }
        // Grandparent's shadow_count: parent decremented (detached),
        // child incremented (adopted).
        assert_eq!(grandparent.read().shadow_count(), 1);
    }

    // -----------------------------------------------------------------------
    // Phantom page scenario (full end-to-end)
    // -----------------------------------------------------------------------

    /// Simulates the exact phantom page scenario:
    ///
    /// 1. Process P has a heap with pages F0, F1 in a backing object.
    /// 2. Fork: parent and child each get a shadow -> backing.
    /// 3. Parent shrinks heap (drops F1 from its shadow — but F1 is in backing).
    /// 4. Parent exits (its shadow is dropped, backing shadow_count -> 1).
    /// 5. Child COW-faults on F1's offset — collapse should rename F1 into
    ///    child's shadow (zero-copy), not copy it.
    #[test]
    fn phantom_page_eliminated_by_collapse() {
        // Step 1: backing with F0 and F1.
        let backing = VmObject::new(8192);
        {
            let mut w = backing.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0xF0F0)));
            w.insert_page(1, PageRef::new(PhysAddr::new(0xF1F1)));
        }

        // Step 2: fork — parent shadow and child shadow.
        let parent_shadow = VmObject::new_shadow(Arc::clone(&backing), 8192);
        let child_shadow = VmObject::new_shadow(Arc::clone(&backing), 8192);
        assert_eq!(backing.read().shadow_count(), 2);

        // Step 3: parent "shrinks" — conceptually drops interest in offset 1.
        // (In real kernel: pmap_remove + truncate. Here we just note that
        // parent_shadow has no page at offset 1 — it's a phantom in backing.)

        // Step 4: parent exits.
        drop(parent_shadow);
        assert_eq!(backing.read().shadow_count(), 1);

        // Step 5: child triggers collapse (would happen during COW fault).
        {
            let mut w = child_shadow.write();
            w.collapse();

            // F0 and F1 should both be migrated into child (no conflict).
            assert_eq!(w.resident_count(), 2);
            assert_eq!(w.lookup_page(0).unwrap(), PhysAddr::new(0xF0F0));
            assert_eq!(w.lookup_page(1).unwrap(), PhysAddr::new(0xF1F1));

            // Chain is now flat — no backing.
            assert!(w.backing().is_none());
        }

        // Backing should be empty (pages migrated, not leaked).
        assert_eq!(backing.read().resident_count(), 0);
    }

    /// Variant: child already COW-copied F0 before collapse.
    /// F0 in backing becomes a phantom — collapse should free it.
    #[test]
    fn phantom_page_freed_when_child_has_cow_copy() {
        let backing = VmObject::new(8192);
        {
            let mut w = backing.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0xF0F0)));
            w.insert_page(1, PageRef::new(PhysAddr::new(0xF1F1)));
        }

        let parent_shadow = VmObject::new_shadow(Arc::clone(&backing), 8192);
        let child_shadow = VmObject::new_shadow(Arc::clone(&backing), 8192);

        // Child already did a COW copy of offset 0.
        {
            let mut w = child_shadow.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0xC09F)));
        }

        // Parent exits.
        drop(parent_shadow);
        assert_eq!(backing.read().shadow_count(), 1);

        // Collapse: offset 0 conflicts (child keeps 0xC000, backing's 0xF0F0
        // is freed). Offset 1 migrates (0xF1F1 renamed into child).
        {
            let mut w = child_shadow.write();
            w.collapse();
            assert_eq!(w.lookup_page(0).unwrap(), PhysAddr::new(0xC09F));
            assert_eq!(w.lookup_page(1).unwrap(), PhysAddr::new(0xF1F1));
            assert_eq!(w.resident_count(), 2);
            assert!(w.backing().is_none());
        }
    }

    // -----------------------------------------------------------------------
    // Fork bomb stress test
    // -----------------------------------------------------------------------

    /// Simulates a fork bomb: one backing object with N shadows created
    /// and destroyed in waves. Verifies shadow_count stays consistent
    /// and collapse works correctly after mass destruction.
    #[test]
    fn fork_bomb_shadow_count_consistency() {
        let backing = VmObject::new(4096);
        {
            let mut w = backing.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0xBA5E)));
        }

        // Wave 1: create 100 shadows (simulates fork bomb).
        let mut shadows: Vec<Arc<LeveledRwLock<VmObject, 3>>> = Vec::new();
        for _ in 0..100 {
            shadows.push(VmObject::new_shadow(Arc::clone(&backing), 4096));
        }
        assert_eq!(backing.read().shadow_count(), 100);

        // All shadows can see the backing page.
        for s in &shadows {
            assert_eq!(s.read().lookup_page(0).unwrap(), PhysAddr::new(0xBA5E));
        }

        // Kill 99 of them (processes exit).
        let survivor = shadows.pop().unwrap();
        drop(shadows);
        assert_eq!(backing.read().shadow_count(), 1);

        // Survivor collapses — absorbs the page.
        {
            let mut w = survivor.write();
            w.collapse();
            assert_eq!(w.lookup_page(0).unwrap(), PhysAddr::new(0xBA5E));
            assert_eq!(w.resident_count(), 1);
            assert!(w.backing().is_none());
        }
        assert_eq!(backing.read().resident_count(), 0);
    }

    /// Fork bomb with deep chains: each generation forks from the previous.
    /// Simulates shell script `while true; do $0 & done` creating a deep
    /// shadow chain. Verifies iterative drop + collapse don't corrupt state.
    #[test]
    fn fork_bomb_deep_chain_200() {
        let root = VmObject::new(4096);
        {
            let mut w = root.write();
            w.insert_page(0, PageRef::new(PhysAddr::new(0x2007)));
        }

        // Build a 200-deep chain: each "child" forks from previous "parent".
        // Keep all intermediate refs alive (simulates all processes running).
        let mut all: Vec<Arc<LeveledRwLock<VmObject, 3>>> = vec![Arc::clone(&root)];
        for _ in 0..200 {
            let parent = all.last().unwrap();
            let child = VmObject::new_shadow(Arc::clone(parent), 4096);
            all.push(child);
        }

        // All 201 objects alive. Deepest can see root's page.
        assert_eq!(
            all.last().unwrap().read().lookup_page(0).unwrap(),
            PhysAddr::new(0x2007)
        );
        assert_eq!(all.last().unwrap().read().shadow_depth(), 200);

        // Kill all except root and the deepest.
        let deepest = all.pop().unwrap();
        let _root_ref = all.remove(0);
        // Drop the 199 intermediates in reverse order.
        drop(all);

        // Deepest still sees the page (chain walk through root).
        assert_eq!(
            deepest.read().lookup_page(0).unwrap(),
            PhysAddr::new(0x2007)
        );

        // Shadow counts should be consistent after mass drop.
        // The chain may have partially collapsed during drops.
        // The key invariant: no panic, no corruption.
        drop(deepest);
        // Root should have shadow_count 0 now.
        assert_eq!(_root_ref.read().shadow_count(), 0);
    }

    /// Fork bomb: wide fan (100 shadows) each with their own COW page,
    /// then mass exit. Verifies no page leaks or double-frees.
    #[test]
    fn fork_bomb_wide_fan_with_cow_pages() {
        let backing = VmObject::new(4096 * 4);
        {
            let mut w = backing.write();
            for i in 0..4u64 {
                w.insert_page(
                    i,
                    PageRef::new(PhysAddr::new((0xA000 + i as usize) * 0x1000)),
                );
            }
        }

        // 100 shadows, each does a COW write to a different page.
        let mut shadows: Vec<Arc<LeveledRwLock<VmObject, 3>>> = Vec::new();
        for i in 0..100 {
            let s = VmObject::new_shadow(Arc::clone(&backing), 4096 * 4);
            {
                let mut w = s.write();
                // Each shadow writes to offset (i % 4).
                let offset = (i % 4) as u64;
                w.insert_page(
                    offset,
                    PageRef::new(PhysAddr::new(0xC000_0000 + i * 0x1000)),
                );
            }
            shadows.push(s);
        }
        assert_eq!(backing.read().shadow_count(), 100);

        // Verify each shadow sees its own COW page.
        for (i, s) in shadows.iter().enumerate() {
            let offset = (i % 4) as u64;
            let r = s.read();
            assert_eq!(
                r.lookup_page(offset).unwrap(),
                PhysAddr::new(0xC000_0000 + i * 0x1000)
            );
        }

        // Kill all but one.
        let survivor = shadows.pop().unwrap();
        drop(shadows);
        assert_eq!(backing.read().shadow_count(), 1);

        // Collapse survivor.
        {
            let mut w = survivor.write();
            w.collapse();
            // Survivor had COW page at offset 3 (99 % 4 = 3).
            // Offsets 0, 1, 2 should be migrated from backing.
            // Offset 3: conflict — survivor's COW page wins.
            assert_eq!(w.resident_count(), 4);
            assert!(w.backing().is_none());
        }
    }
}
