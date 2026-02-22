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
                // Frame deallocation will go here once the frame allocator
                // is implemented (Phase 2 Plan 2). For now, the PhysAddr
                // is simply dropped.
                let _ = page.phys;
            }
        }

        // Iteratively unwind the backing chain.
        let mut current = self.backing.take();
        while let Some(arc) = current {
            match Arc::try_unwrap(arc) {
                Ok(rwlock) => {
                    let mut obj = rwlock.into_inner();
                    // Free anonymous pages in this ancestor.
                    let ancestor_pages = core::mem::take(&mut obj.pages);
                    for (_offset, page) in ancestor_pages {
                        if matches!(page.ownership, PageOwnership::Anonymous) {
                            let _ = page.phys;
                        }
                    }
                    current = obj.backing.take();
                }
                Err(_) => break, // Other references exist; stop unwinding.
            }
        }
    }
}
