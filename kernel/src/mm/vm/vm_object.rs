//! BSD-style VmObject with shadow chain support.
//!
//! VmObject holds physical pages indexed by page offset. Shadow chains
//! enable COW semantics: fork inserts a new shadow at the chain head,
//! and write faults copy pages into the topmost shadow.
//!
//! The `Drop` implementation uses iterative `Arc::try_unwrap` unwinding
//! to handle arbitrarily deep shadow chains (500+) without stack overflow.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use spin::RwLock;

use hal_common::PhysAddr;

// ---------------------------------------------------------------------------
// PageOwnership
// ---------------------------------------------------------------------------

/// Distinguishes who is responsible for freeing a physical frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageOwnership {
    /// Frame is freed when the owning VmObject is dropped.
    Anonymous,
    /// Page cache manages the frame lifetime; VmObject only holds a reference.
    Cached,
}

// ---------------------------------------------------------------------------
// OwnedPage
// ---------------------------------------------------------------------------

/// A physical page held by a VmObject, tagged with ownership semantics.
#[derive(Debug)]
pub struct OwnedPage {
    /// Physical address of the page (page-aligned).
    pub phys: PhysAddr,
    /// Who is responsible for freeing this frame.
    pub ownership: PageOwnership,
}

impl OwnedPage {
    pub fn new_anonymous(phys: PhysAddr) -> Self {
        Self { ownership: PageOwnership::Anonymous, phys }
    }

    pub fn new_cached(phys: PhysAddr) -> Self {
        Self { ownership: PageOwnership::Cached, phys }
    }
}

// ---------------------------------------------------------------------------
// VmObject
// ---------------------------------------------------------------------------

/// Core VM object: a collection of physical pages indexed by page offset,
/// with an optional backing (parent) object forming a shadow chain.
pub struct VmObject {
    /// Pages owned directly by this object, keyed by page offset (in pages).
    pages: BTreeMap<u64, OwnedPage>,
    /// Parent in the shadow chain (for COW).
    backing: Option<Arc<RwLock<VmObject>>>,
    /// Object size in bytes.
    size: usize,
    /// Number of pages resident in *this* object (not backing).
    resident_count: usize,
}

impl VmObject {
    /// Create a new anonymous VmObject (no backing).
    pub fn new(size: usize) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self {
            pages: BTreeMap::new(),
            backing: None,
            size,
            resident_count: 0,
        }))
    }

    /// Create a shadow object in front of `parent` (for fork COW).
    ///
    /// The new shadow starts empty; page lookups walk through to the parent.
    pub fn new_shadow(parent: Arc<RwLock<VmObject>>, size: usize) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self {
            pages: BTreeMap::new(),
            backing: Some(parent),
            size,
            resident_count: 0,
        }))
    }

    /// Look up a page by offset, walking the shadow chain iteratively.
    ///
    /// Returns the physical address if found in this object or any ancestor.
    pub fn lookup_page(&self, offset: u64) -> Option<PhysAddr> {
        // Check this object first.
        if let Some(page) = self.pages.get(&offset) {
            return Some(page.phys);
        }
        // Walk the backing chain iteratively.
        let mut current = self.backing.as_ref().map(Arc::clone);
        while let Some(arc) = current {
            let obj = arc.read();
            if let Some(page) = obj.pages.get(&offset) {
                return Some(page.phys);
            }
            current = obj.backing.as_ref().map(Arc::clone);
        }
        None
    }

    /// Insert a page into this object (not the backing chain).
    pub fn insert_page(&mut self, offset: u64, page: OwnedPage) {
        if self.pages.insert(offset, page).is_none() {
            self.resident_count += 1;
        }
    }

    /// Remove a page from this object only (does not touch backing).
    pub fn remove_page(&mut self, offset: u64) -> Option<OwnedPage> {
        let removed = self.pages.remove(&offset);
        if removed.is_some() {
            self.resident_count -= 1;
        }
        removed
    }

    /// Count the shadow chain depth (for debug/testing).
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
    pub fn resident_count(&self) -> usize {
        self.resident_count
    }

    /// Get a reference to the backing object (if any).
    pub fn backing(&self) -> Option<&Arc<RwLock<VmObject>>> {
        self.backing.as_ref()
    }

    /// Update the object size in bytes.
    pub fn set_size(&mut self, new_size: usize) {
        self.size = new_size;
    }

    /// Remove and return all pages at offsets >= `from_page`.
    /// Only operates on this object (not the backing chain).
    pub fn truncate_pages(&mut self, from_page: u64) -> alloc::vec::Vec<OwnedPage> {
        let keys: alloc::vec::Vec<u64> = self.pages.range(from_page..).map(|(&k, _)| k).collect();
        let mut removed = alloc::vec::Vec::with_capacity(keys.len());
        for k in keys {
            if let Some(page) = self.pages.remove(&k) {
                self.resident_count -= 1;
                removed.push(page);
            }
        }
        removed
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
        // Free our own anonymous pages.
        let pages = core::mem::take(&mut self.pages);
        for (_offset, page) in pages {
            if matches!(page.ownership, PageOwnership::Anonymous) {
                #[cfg(not(test))]
                crate::mm::allocator::frame_free(page.phys);
            }
        }

        // Iteratively unwind the backing chain.
        let mut current = self.backing.take();
        while let Some(arc) = current {
            match Arc::try_unwrap(arc) {
                Ok(rwlock) => {
                    let mut obj = rwlock.into_inner();
                    let ancestor_pages = core::mem::take(&mut obj.pages);
                    for (_offset, page) in ancestor_pages {
                        if matches!(page.ownership, PageOwnership::Anonymous) {
                            #[cfg(not(test))]
                            crate::mm::allocator::frame_free(page.phys);
                        }
                    }
                    current = obj.backing.take();
                }
                Err(_) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            w.insert_page(0, OwnedPage::new_anonymous(PhysAddr::new(0x1000)));
            w.insert_page(1, OwnedPage::new_anonymous(PhysAddr::new(0x2000)));
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
            w.insert_page(0, OwnedPage::new_anonymous(PhysAddr::new(0x1000)));
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
            w.insert_page(0, OwnedPage::new_anonymous(PhysAddr::new(0xA000)));
        }
        let parent = VmObject::new_shadow(Arc::clone(&grandparent), 4096);
        {
            let mut w = parent.write();
            w.insert_page(1, OwnedPage::new_anonymous(PhysAddr::new(0xB000)));
        }
        let child = VmObject::new_shadow(Arc::clone(&parent), 4096);
        {
            let mut w = child.write();
            w.insert_page(2, OwnedPage::new_anonymous(PhysAddr::new(0xC000)));
        }
        // Child has page 2, parent has page 1, grandparent has page 0
        let r = child.read();
        assert_eq!(r.lookup_page(2).unwrap(), PhysAddr::new(0xC000));
        // lookup_page walks backing chain, so grandparent's page 0 is visible
        assert_eq!(r.lookup_page(0).unwrap(), PhysAddr::new(0xA000));
    }

    #[test]
    fn page_ownership_types() {
        let anon = OwnedPage::new_anonymous(PhysAddr::new(0x1000));
        assert_eq!(anon.ownership, PageOwnership::Anonymous);
        let cached = OwnedPage::new_cached(PhysAddr::new(0x2000));
        assert_eq!(cached.ownership, PageOwnership::Cached);
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
                w.insert_page(i as u64, OwnedPage::new_anonymous(PhysAddr::new((i + 1) * 0x1000)));
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
            w.insert_page(0, OwnedPage::new_anonymous(PhysAddr::new(0x1000)));
            w.insert_page(0, OwnedPage::new_anonymous(PhysAddr::new(0x2000)));
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
            w.insert_page(0, OwnedPage::new_anonymous(PhysAddr::new(0xAAAA_0000)));
        }
        let shadow = VmObject::new_shadow(Arc::clone(&parent), 4096);
        {
            let mut w = shadow.write();
            w.insert_page(0, OwnedPage::new_anonymous(PhysAddr::new(0xBBBB_0000)));
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
                w.insert_page(i as u64, OwnedPage::new_anonymous(PhysAddr::new((i + 1) * 0x1000)));
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
            w.insert_page(0, OwnedPage::new_anonymous(PhysAddr::new(0x1000)));
            w.insert_page(1, OwnedPage::new_anonymous(PhysAddr::new(0x2000)));
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
}
