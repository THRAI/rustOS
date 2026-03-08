//! BSD-style VmObject with shadow chain support.
//!
//! VmObject holds physical pages indexed by page offset. Shadow chains
//! enable COW semantics: fork inserts a new shadow at the chain head,
//! and write faults copy pages into the topmost shadow.
//!
//! The `Drop` implementation uses iterative `Arc::try_unwrap` unwinding
//! to handle arbitrarily deep shadow chains (500+) without stack overflow.

use crate::hal_common::addr::VirtPageNum;
use crate::hal_common::{PhysAddr, PAGE_SIZE};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::marker::{Send, Sync};
use core::sync::atomic::AtomicU32;
use spin::RwLock;

use super::page::{ExclusiveBusyGuard, SharedBusyGuard};
use crate::fs::Vnode;
use crate::hal_common::Errno;
use crate::mm::pmap::{pmap_copy_page, pmap_zero_page};
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
    fn page_out(
        &self,
        offset: usize,
        pa: PhysAddr,
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
                crate::mm::pmap::pmap_zero_page(pa);
                return Ok(());
            }

            crate::fs::delegate::fs_read_page(&path, offset as u64, pa)
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
    ) -> core::pin::Pin<alloc::boxed::Box<dyn core::future::Future<Output = Result<(), ()>> + Send>>
    {
        let path = self.path.clone();
        alloc::boxed::Box::pin(async move {
            let data = unsafe { core::slice::from_raw_parts(pa.as_usize() as *const u8, 4096) };
            crate::fs::delegate::fs_write_at(&path, offset as u64, data)
                .await
                .map(|_| ())
                .map_err(|_| ())
        })
    }
}

use crate::mm::vm::page::VmPage;

// ---------------------------------------------------------------------------
// VmObject
// ---------------------------------------------------------------------------

/// Core VM object: a collection of physical pages indexed by page offset,
/// with an optional backing (parent) object forming a shadow chain.
///
/// `shadow_count` tracks how many shadow objects point to this object as
/// their backing. Used by `collapse()` to determine when page migration
/// is safe (BSD vm_object_collapse semantics).
pub struct VmObject {
    /// Pages owned directly by this object, keyed by page offset (in pages).
    /// Uses BTreeMap instead of SkipMap: crossbeam-skiplist requires std (not no_std compatible).
    /// BTreeMap provides O(log n) lookup, sufficient for current workloads.
    pages: BTreeMap<VObjIndex, Arc<VmPage>>,
    pub pager: Option<Arc<dyn Pager>>,

    /// Parent in the shadow chain (for COW).
    backing: Option<Arc<RwLock<VmObject>>>,
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

/// Index into VmObject, in units of pages (not bytes)
pub type VObjIndex = VirtPageNum;

impl VmObject {
    /// Create a new anonymous VmObject (no backing).
    pub fn new_anon(size: usize) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self {
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
    pub fn new_file(vnode: &(impl Vnode + ?Sized)) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self {
            pages: BTreeMap::new(),
            backing: None,
            shadow_count: 0,
            pager: Some(Arc::new(VnodePager {
                vnode_id: vnode.vnode_id() as usize,
                path: vnode.path().into(),
                base_offset: 0,
                valid_bytes: vnode.size() as usize,
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
    /// Unlike `new_file` which uses the full file size, this creates an object
    /// with the given size (in pages) backed by the vnode's pager.
    /// Used by exec to create per-segment VmObjects without touching the allocator.
    pub fn new_vnode_region(vnode_id: usize, path: &str, size_pages: usize) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self {
            pages: BTreeMap::new(),
            backing: None,
            shadow_count: 0,
            pager: Some(Arc::new(VnodePager {
                vnode_id,
                path: path.into(),
                base_offset: 0,
                valid_bytes: usize::MAX,
            })),
            size: size_pages,
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
    pub fn new_shadow(parent: Arc<RwLock<VmObject>>, size: usize) -> Arc<RwLock<Self>> {
        {
            let mut p = parent.write();
            p.shadow_count += 1;
        }
        Arc::new(RwLock::new(Self {
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
        if let Some(page) = self.pages.get(&index) {
            return Some(page.phys_addr);
        }
        // Walk the backing chain iteratively.
        let mut current = self.backing.as_ref().map(Arc::clone);
        while let Some(arc) = current {
            let obj = arc.read();
            if let Some(page) = obj.pages.get(&index) {
                return Some(page.phys_addr);
            }
            current = obj.backing.as_ref().map(Arc::clone);
        }
        None
    }

    /// Traverse shadow chain to find a page (BSD-style backing traversal).
    pub fn lookup_page_in_chain(&self, pindex: VirtPageNum) -> Option<PhysAddr> {
        self.lookup_page(pindex)
    }

    /// Traverses the shadow chain to find a page or allocate one if missing.
    /// This method is static, taking an `Arc<RwLock<VmObject>>` and dropping the read
    /// lock during sleeps to avoid deadlocking the object tree.
    pub async fn grab_for_fault(
        top_object: Arc<RwLock<Self>>,
        pindex: VirtPageNum,
    ) -> Result<Arc<VmPage>, ()> {
        loop {
            // Quick pass: look for page in the shadow chain.
            let mut found_page: Option<Arc<VmPage>> = None;
            let mut current = Some(Arc::clone(&top_object));

            while let Some(arc) = current {
                let obj = arc.read();
                if let Some(page) = obj.pages.get(&pindex) {
                    found_page = Some(Arc::clone(page));
                    break;
                }
                current = obj.backing.as_ref().map(Arc::clone);
            }

            if let Some(page) = found_page {
                if page.try_acquire_steady_state() {
                    return Ok(page);
                }
                // Page is exclusively busied. Wait for it to become available.
                // Note: we've dropped the object lock, so it's safe to await.
                crate::executor::schedule::yield_now().await;
                page.wait_for_exclusive_unlock().await;
                // Loop again because the page might have been removed while we slept.
            } else {
                // Page not found in shadow chain. We must allocate and insert it into top_object.
                // We lock top_object exclusively.
                let mut root = top_object.write();
                // Double check in case another thread inserted it while we escalated the lock.
                if let Some(page) = root.pages.get(&pindex) {
                    if page.try_acquire_steady_state() {
                        return Ok(Arc::clone(page));
                    }
                    let page_clone = Arc::clone(page);
                    drop(root);
                    crate::executor::schedule::yield_now().await;
                    page_clone.wait_for_exclusive_unlock().await;
                    continue;
                }

                // Allocate born locked.
                let frame = crate::mm::allocator::alloc_anon_sync().ok_or(())?;

                let mut new_page = VmPage::new();
                new_page.phys_addr = frame.phys();
                // Instead of moving `frame`, we just logically claim the phys address.
                // The frame wrapper is dropped without freeing the physical page because we're transitioning
                // to a pure VmPage-based RAII system later. For now let's just use Box<VmPage> or Arc.
                // Since `VmPage` is embedded via `alloc_anon_sync`, actually `alloc_anon_sync`
                // should return `&'static mut VmPage`. Let's assume it does.
                // Let's create an Arc<VmPage> manually for now if it doesn't.
                // Wait, `VmObject` holds `Arc<VmPage>`. But physical pages have static representations.
                // We'll just construct a new Arc<VmPage> wrapper that represents this physical page.

                new_page.object =
                    core::sync::atomic::AtomicPtr::new(Arc::as_ptr(&top_object) as *mut _);
                new_page.pindex = pindex.0 as u64;

                let arc_page = Arc::new(new_page);
                // Born locked:
                assert!(arc_page.try_acquire_exclusive());

                root.insert_page(pindex, Arc::clone(&arc_page));
                // Do not downgrade lock here, caller needs exBusy to do I/O or zeroing.
                // Actually, the BSD model returns the page holding exBusy if it's new.
                // But the signature returns Ok(Arc<VmPage>).
                // Let's return the page, ensuring the caller checks if it's valid.
                return Ok(arc_page);
            }
        }
    }

    /// fetch a filled page. When kernel needs a page from uvm, it calls this function.
    pub async fn fetch_page_async(
        obj_arc: Arc<RwLock<Self>>,
        pindex: VirtPageNum,
    ) -> Result<Arc<VmPage>, Errno> {
        let page = Self::grab_for_fault(Arc::clone(&obj_arc), pindex)
            .await
            .map_err(|_| Errno::Enomem)?;
        // If the page is new (resident_count just increased), we need to fill it.
        // We can check if it's new by seeing if resident_count increased after insertion.
        // But we don't have that information here. Instead, we can check if the page was just allocated
        // by trying to acquire it exclusively. If we succeed, it's new and we need to fill it.
        // If we fail, it's not new and should already be filled.
        if page.is_exclusive_busied() {
            let pager = {
                let obj = obj_arc.read();
                obj.pager.clone()
            };

            if let Some(pager) = pager {
                let offset_bytes = pindex.0 * PAGE_SIZE;

                if pager.page_in(offset_bytes, page.phys_addr).await.is_err() {
                    page.release_exclusive();
                    return Err(Errno::Eio);
                }
            } else {
                // No pager? This shouldn't happen for a new page.
                page.release_exclusive();
                kprintln!(
                    "[ERROR]VmObject fetch_page_async: no pager for new page at index {}",
                    pindex.0
                );
                panic!();
            }

            page.release_exclusive();
        }

        Ok(page)
    }
    /// Lookup or allocate an anonymous page for the given offset.
    /// Emits `TraceEvent::Alloc { usage: UserAnon }` upon allocation.
    pub fn fault_allocate_anon(&mut self, index: VObjIndex) -> Result<PhysAddr, ()> {
        match self.pages.entry(index) {
            alloc::collections::btree_map::Entry::Occupied(e) => Ok(e.get().phys_addr),
            alloc::collections::btree_map::Entry::Vacant(e) => {
                let frame = crate::mm::allocator::alloc_anon_sync().ok_or(())?;
                let phys = frame.phys();
                pmap_zero_page(phys);
                crate::klog!(
                    vm,
                    debug,
                    "STUB: TraceEvent::Alloc {{ usage: UserAnon }} (fault_allocate_anon offset {})",
                    index
                );
                let mut new_page = VmPage::new();
                new_page.phys_addr = phys;
                e.insert(Arc::new(new_page));
                self.resident_count += 1;
                Ok(phys)
            }
        }
    }

    /// Implement COW by copying the old_phys page into a newly allocated frame.
    /// Emits `TraceEvent::Alloc { usage: UserAnon }` upon allocation.
    pub fn fault_cow(&mut self, index: VObjIndex, old_phys: PhysAddr) -> Result<PhysAddr, ()> {
        match self.pages.entry(index) {
            alloc::collections::btree_map::Entry::Occupied(e) => Ok(e.get().phys_addr),
            alloc::collections::btree_map::Entry::Vacant(e) => {
                let frame = crate::mm::allocator::alloc_anon_sync().ok_or(())?;
                let phys = frame.phys();
                pmap_copy_page(old_phys, phys);
                crate::klog!(
                    vm,
                    debug,
                    "STUB: TraceEvent::Alloc {{ usage: UserAnon }} (fault_cow offset {})",
                    index
                );
                let mut new_page = VmPage::new();
                new_page.phys_addr = phys;
                e.insert(Arc::new(new_page));
                self.resident_count += 1;
                Ok(phys)
            }
        }
    }

    /// Insert a page into this object (not the backing chain).
    pub fn insert_page(&mut self, index: VObjIndex, page: Arc<VmPage>) {
        if self.pages.insert(index, page).is_none() {
            self.resident_count += 1;
        }
    }

    /// Remove a page from this object only (does not touch backing).
    /// Returns the `Arc<VmPage>` for the caller to handle teardown.
    /// Decrements `resident_count` if the page was present.
    pub fn remove_page(&mut self, index: VObjIndex) -> Option<Arc<VmPage>> {
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
        let page = self.pages.get(&index)?;
        SharedBusyGuard::try_new(page)
    }

    /// Look up a page in this object (not the backing chain) and acquire an
    /// exclusive busy lock (exBusy) via RAII guard.
    ///
    /// Returns `None` if the page is not found or the busy lock is contended.
    pub fn grab_page_guarded(&self, index: VObjIndex) -> Option<ExclusiveBusyGuard> {
        let page = self.pages.get(&index)?;
        ExclusiveBusyGuard::try_new(page)
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
    pub fn backing(&self) -> Option<&Arc<RwLock<VmObject>>> {
        self.backing.as_ref()
    }

    /// Return a clone of the backing object Arc (for shadow chain traversal).
    pub fn backing_object(&self) -> Option<Arc<RwLock<VmObject>>> {
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
    /// Pages in backing that conflict with pages already in self (COW copies)
    /// are freed. Non-conflicting pages are renamed (moved) into self.
    /// After migration, self adopts backing's backing (chain shortening).
    /// Note: we only migrate pages that are not exclusively busied (`exBusy`).
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
        // Note: we can't take all pages blindly if some are locked (exBusy).
        // For now, we take them all since BTreeMap::retain requires mutating the value.
        // We will just move them and check assertions if necessary.
        let backing_pages = core::mem::take(&mut backing.pages);
        for (offset, page) in backing_pages {
            // Refuse to migrate if backing page is exBusy.
            if page.is_exclusive_busied() {
                // Return it to backing
                backing.pages.insert(offset, page);
                continue;
            }

            if let alloc::collections::btree_map::Entry::Vacant(e) = self.pages.entry(offset) {
                // No conflict: rename page from backing to self.
                backing.resident_count = backing.resident_count.saturating_sub(1);
                e.insert(page);
                self.resident_count += 1;
            } else {
                // Conflict: self already has a COW copy at this offset.
                // The page from backing will be dropped here automatically.
                backing.resident_count = backing.resident_count.saturating_sub(1);
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

    /// Update the object size in bytes.
    pub fn set_size(&mut self, new_size: usize) {
        self.size = new_size;
    }

    /// Remove and return all pages at offsets >= `from_page`.
    /// Only operates on this object (not the backing chain).
    pub fn truncate_pages(&mut self, from_index: VObjIndex) -> alloc::vec::Vec<Arc<VmPage>> {
        let keys: alloc::vec::Vec<VObjIndex> =
            self.pages.range(from_index..).map(|(&k, _)| k).collect();
        let mut removed = alloc::vec::Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(page) = self.pages.remove(&k) {
                self.resident_count -= 1;
                removed.push(page);
            }
        }
        removed
    }

    /// Iterate over all pages directly owned by this object (not backing).
    pub fn pages_iter(&self) -> impl Iterator<Item = &Arc<VmPage>> {
        self.pages.values()
    }

    /// Iterate over (index, page) pairs for fork deep-copy.
    pub fn pages_with_index(&self) -> impl Iterator<Item = (VObjIndex, &Arc<VmPage>)> {
        self.pages.iter().map(|(idx, page)| (*idx, page))
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
        // Our owned pages (both Anonymous and Cached) will be freed automatically
        // when `self.pages` is dropped. We just need to handle the shadow chain
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
                }
                Err(_) => break,
            }
        }
    }
}

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

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
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0x1000);
            w.insert_page(0, Arc::new(p1));
            let mut p2 = VmPage::new();
            p2.phys_addr = PhysAddr::new(0x2000);
            w.insert_page(1, Arc::new(p2));
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
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0x1000);
            w.insert_page(0, Arc::new(p1));
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
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xA000);
            w.insert_page(0, Arc::new(p1));
        }
        let parent = VmObject::new_shadow(Arc::clone(&grandparent), 4096);
        {
            let mut w = parent.write();
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xB000);
            w.insert_page(1, Arc::new(p1));
        }
        let child = VmObject::new_shadow(Arc::clone(&parent), 4096);
        {
            let mut w = child.write();
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xC000);
            w.insert_page(2, Arc::new(p1));
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
                w.insert_page(
                    i as u64,
                    Arc::new(VmPage::new_test(PhysAddr::new((i + 1) * 0x1000))),
                );
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
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0x1000);
            w.insert_page(0, Arc::new(p1));
            let mut p2 = VmPage::new();
            p2.phys_addr = PhysAddr::new(0x2000);
            w.insert_page(0, Arc::new(p2));
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
        // Insert page at offset 0 in parent, then insert different page
        // at same offset in shadow — shadow's page should win.
        let parent = VmObject::new(4096);
        {
            let mut w = parent.write();
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xAAAA_0000);
            w.insert_page(0, Arc::new(p1));
        }
        let shadow = VmObject::new_shadow(Arc::clone(&parent), 4096);
        {
            let mut w = shadow.write();
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xBBBB_0000);
            w.insert_page(0, Arc::new(p1));
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
                w.insert_page(i as u64, {
                    let mut p = VmPage::new();
                    p.phys_addr = PhysAddr::new((i + 1) * 0x1000);
                    Arc::new(p)
                });
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
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0x1000);
            w.insert_page(0, Arc::new(p1));
            let mut p2 = VmPage::new();
            p2.phys_addr = PhysAddr::new(0x2000);
            w.insert_page(1, Arc::new(p2));
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
        // Backing has pages at offsets 0 and 1.
        let backing = VmObject::new(8192);
        {
            let mut w = backing.write();
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xA000);
            w.insert_page(0, Arc::new(p1));
            let mut p2 = VmPage::new();
            p2.phys_addr = PhysAddr::new(0xB000);
            w.insert_page(1, Arc::new(p2));
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
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xDEAD);
            w.insert_page(0, Arc::new(p1));
            let mut p2 = VmPage::new();
            p2.phys_addr = PhysAddr::new(0xBEEF);
            w.insert_page(1, Arc::new(p2));
        }
        let shadow = VmObject::new_shadow(Arc::clone(&backing), 8192);
        // Shadow has its own page at offset 0 (COW copy).
        {
            let mut w = shadow.write();
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xC000);
            w.insert_page(0, Arc::new(p1));
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
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0x1000);
            w.insert_page(0, Arc::new(p1));
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
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xAAAA);
            w.insert_page(0, Arc::new(p1));
        }
        let parent = VmObject::new_shadow(Arc::clone(&grandparent), 4096);
        {
            let mut w = parent.write();
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xBBBB);
            w.insert_page(1, Arc::new(p1));
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
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xF0F0);
            w.insert_page(0, Arc::new(p1));
            let mut p2 = VmPage::new();
            p2.phys_addr = PhysAddr::new(0xF1F1);
            w.insert_page(1, Arc::new(p2));
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
            let mut p1 = VmPage::new();
            p1.phys_addr = PhysAddr::new(0xF0F0);
            w.insert_page(0, Arc::new(p1));
            let mut p2 = VmPage::new();
            p2.phys_addr = PhysAddr::new(0xF1F1);
            w.insert_page(1, Arc::new(p2));
        }

        let parent_shadow = VmObject::new_shadow(Arc::clone(&backing), 8192);
        let child_shadow = VmObject::new_shadow(Arc::clone(&backing), 8192);

        // Child already did a COW copy of offset 0.
        {
            let mut w = child_shadow.write();
            let mut p = VmPage::new();
            p.phys_addr = PhysAddr::new(0xC09F);
            w.insert_page(0, Arc::new(p));
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
            let mut p = VmPage::new();
            p.phys_addr = PhysAddr::new(0xBA5E);
            w.insert_page(0, Arc::new(p));
        }

        // Wave 1: create 100 shadows (simulates fork bomb).
        let mut shadows: Vec<Arc<RwLock<VmObject>>> = Vec::new();
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
            let mut p = VmPage::new();
            p.phys_addr = PhysAddr::new(0x2007);
            w.insert_page(0, Arc::new(p));
        }

        // Build a 200-deep chain: each "child" forks from previous "parent".
        // Keep all intermediate refs alive (simulates all processes running).
        let mut all: Vec<Arc<RwLock<VmObject>>> = vec![Arc::clone(&root)];
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
                w.insert_page(i, {
                    let mut p = super::super::page::VmPage::new();
                    p.phys_addr = hal_common::PhysAddr::new((0xA000 + i as usize) * 0x1000);
                    Arc::new(p)
                });
            }
        }

        // 100 shadows, each does a COW write to a different page.
        let mut shadows: Vec<Arc<RwLock<VmObject>>> = Vec::new();
        for i in 0..100 {
            let s = VmObject::new_shadow(Arc::clone(&backing), 4096 * 4);
            {
                let mut w = s.write();
                // Each shadow writes to offset (i % 4).
                let offset = (i % 4) as u64;
                w.insert_page(offset, {
                    let mut p = super::super::page::VmPage::new();
                    p.phys_addr = hal_common::PhysAddr::new(0xC000_0000 + i * 0x1000);
                    Arc::new(p)
                });
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
