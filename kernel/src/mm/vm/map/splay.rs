use core::ptr::NonNull;

use crate::mm::vm::VmMapEntry;

pub struct SplayTree {
    root: Option<NonNull<VmMapEntry>>,
}

// SAFETY: VmMap wraps SplayTree and protects access with LeveledRwLock<VmMap, 1>.
// The RwLock guarantees that &SplayTree (shared reads) and &mut SplayTree
// (exclusive writes) are never handed out concurrently, so it is safe to
// implement Sync here.  The NonNull<VmMapEntry> inside is never aliased
// across threads without the lock held.
unsafe impl Send for SplayTree {}
unsafe impl Sync for SplayTree {}

impl SplayTree {
    pub const fn new() -> Self {
        Self { root: None }
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Internal helper: Update max_free for a node
    unsafe fn update_max_free(mut node: NonNull<VmMapEntry>) {
        // SAFETY: Caller guarantees `node` is a valid, exclusively accessible VmMapEntry.
        unsafe {
            let n = node.as_ref();
            let mut max = 0;
            let mut gap_left = 0;
            let mut gap_right = 0;

            if let Some(left) = n.splay_node.left {
                let l = left.as_ref();
                max = core::cmp::max(max, l.max_free);
                gap_left = n.start().saturating_sub(l.end());
            } else {
                // Gap between 0 (or start of map) and n.start() handled generically outside?
                // Actually, gap between node and its children. If no left child, gap is 0 towards children.
            }

            if let Some(right) = n.splay_node.right {
                let r = right.as_ref();
                max = core::cmp::max(max, r.max_free);
                gap_right = r.start().saturating_sub(n.end());
            }

            max = core::cmp::max(max, gap_left);
            max = core::cmp::max(max, gap_right);

            node.as_mut().max_free = max;
        }
    }

    /// Splay the tree around the target address `addr`.
    /// Brings the node containing or closest to `addr` to the root.
    unsafe fn splay(&mut self, addr: u64) {
        // SAFETY: Caller guarantees all NonNull nodes in the tree are valid
        // and exclusively accessible through the SplayTree.
        unsafe {
            if self.root.is_none() {
                return;
            }

            // Standard Top-Down Splay
            let mut n = self.root.take().unwrap();
            let mut dummy = VmMapEntry::new(
                0,
                0,
                crate::mm::vm::BackingStore::Guard,
                crate::mm::vm::EntryFlags::empty(),
                crate::mm::vm::MapPerm::empty(),
            );
            let mut left_tail = NonNull::from(&mut dummy);
            let mut right_tail = NonNull::from(&mut dummy);

            loop {
                let n_ref = n.as_ref();
                if addr < n_ref.start() {
                    if let Some(mut left) = n_ref.splay_node.left {
                        if addr < left.as_ref().start() {
                            // Zig-Zig
                            n.as_mut().splay_node.left = left.as_ref().splay_node.right;
                            Self::update_max_free(n);
                            left.as_mut().splay_node.right = Some(n);
                            n = left;
                            if n.as_ref().splay_node.left.is_none() {
                                break;
                            }
                            left = n.as_ref().splay_node.left.unwrap();
                        }
                        // Zig
                        right_tail.as_mut().splay_node.left = Some(n);
                        right_tail = n;
                        n = left;
                    } else {
                        break;
                    }
                } else if addr >= n_ref.end() {
                    if let Some(mut right) = n_ref.splay_node.right {
                        if addr >= right.as_ref().end() {
                            // Zag-Zag
                            n.as_mut().splay_node.right = right.as_ref().splay_node.left;
                            Self::update_max_free(n);
                            right.as_mut().splay_node.left = Some(n);
                            n = right;
                            if n.as_ref().splay_node.right.is_none() {
                                break;
                            }
                            right = n.as_ref().splay_node.right.unwrap();
                        }
                        // Zag
                        left_tail.as_mut().splay_node.right = Some(n);
                        left_tail = n;
                        n = right;
                    } else {
                        break;
                    }
                } else {
                    // Hit
                    break;
                }
            }

            // Assemble
            left_tail.as_mut().splay_node.right = n.as_ref().splay_node.left;
            right_tail.as_mut().splay_node.left = n.as_ref().splay_node.right;

            // The dummy children are the actual subtrees
            n.as_mut().splay_node.left = dummy.splay_node.right;
            n.as_mut().splay_node.right = dummy.splay_node.left;

            // Update max_free for the subtrees if they exist
            if let Some(l) = n.as_ref().splay_node.left {
                Self::update_max_free(l);
            }
            if let Some(r) = n.as_ref().splay_node.right {
                Self::update_max_free(r);
            }
            Self::update_max_free(n);

            self.root = Some(n);
        }
    }

    pub fn insert(&mut self, entry_box: alloc::boxed::Box<VmMapEntry>) {
        let addr = entry_box.start();
        let mut new_node = NonNull::from(alloc::boxed::Box::leak(entry_box));

        unsafe {
            if self.root.is_none() {
                Self::update_max_free(new_node);
                self.root = Some(new_node);
                return;
            }

            self.splay(addr);
            let mut root = self.root.unwrap();
            let root_ref = root.as_ref();

            if addr < root_ref.start() {
                new_node.as_mut().splay_node.left = root_ref.splay_node.left;
                new_node.as_mut().splay_node.right = Some(root);
                root.as_mut().splay_node.left = None;
            } else {
                new_node.as_mut().splay_node.right = root_ref.splay_node.right;
                new_node.as_mut().splay_node.left = Some(root);
                root.as_mut().splay_node.right = None;
            }

            Self::update_max_free(root);
            Self::update_max_free(new_node);
            self.root = Some(new_node);
        }
    }

    pub fn lookup(&mut self, addr: u64) -> Option<&VmMapEntry> {
        unsafe {
            self.splay(addr);
            if let Some(root) = self.root {
                let r = root.as_ref();
                if addr >= r.start() && addr < r.end() {
                    return Some(r);
                }
            }
        }
        None
    }

    pub fn lookup_readonly(&self, addr: u64) -> Option<&VmMapEntry> {
        let mut curr = self.root;
        while let Some(n) = curr {
            let n_ref = unsafe { n.as_ref() };
            if addr >= n_ref.start() && addr < n_ref.end() {
                return Some(n_ref);
            } else if addr < n_ref.start() {
                curr = n_ref.splay_node.left;
            } else {
                curr = n_ref.splay_node.right;
            }
        }
        None
    }

    pub fn lookup_mut(&mut self, addr: u64) -> Option<&mut VmMapEntry> {
        unsafe {
            self.splay(addr);
            if let Some(mut root) = self.root {
                let r = root.as_mut();
                if addr >= r.start() && addr < r.end() {
                    return Some(r);
                }
            }
        }
        None
    }

    pub fn remove(&mut self, addr: u64) -> Option<alloc::boxed::Box<VmMapEntry>> {
        unsafe {
            self.splay(addr);
            if let Some(mut root) = self.root {
                let r = root.as_mut();
                if addr >= r.start() && addr < r.end() {
                    // Match found, remove it.
                    let left = r.splay_node.left;
                    let right = r.splay_node.right;

                    if let Some(l) = left {
                        // Splay maximum element in left subtree
                        self.root = Some(l);
                        // We use a safe large value to splay max
                        self.splay(u64::MAX);
                        let mut new_root = self.root.unwrap();
                        new_root.as_mut().splay_node.right = right;
                        Self::update_max_free(new_root);
                        self.root = Some(new_root);
                    } else {
                        self.root = right;
                    }

                    // Re-box the removed node so caller can drop it or use it
                    r.splay_node.left = None;
                    r.splay_node.right = None;
                    return Some(alloc::boxed::Box::from_raw(root.as_ptr()));
                }
            }
        }
        None
    }

    pub fn for_each<F: FnMut(&VmMapEntry)>(&self, mut f: F) {
        let mut stack = alloc::vec::Vec::new();
        let mut curr = self.root;
        while curr.is_some() || !stack.is_empty() {
            while let Some(n) = curr {
                stack.push(n);
                curr = unsafe { n.as_ref().splay_node.left };
            }
            if let Some(n) = stack.pop() {
                unsafe {
                    f(n.as_ref());
                    curr = n.as_ref().splay_node.right;
                }
            }
        }
    }

    pub fn iter(&self) -> SplayTreeIter<'_> {
        let mut iter = SplayTreeIter {
            stack: alloc::vec::Vec::new(),
            _marker: core::marker::PhantomData,
        };
        let mut curr = self.root;
        while let Some(n) = curr {
            iter.stack.push(n);
            curr = unsafe { n.as_ref().splay_node.left };
        }
        iter
    }
}

pub struct SplayTreeIter<'a> {
    stack: alloc::vec::Vec<NonNull<VmMapEntry>>,
    _marker: core::marker::PhantomData<&'a VmMapEntry>,
}

impl<'a> Iterator for SplayTreeIter<'a> {
    type Item = &'a VmMapEntry;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        unsafe {
            let n_ref = node.as_ref();
            let mut curr = n_ref.splay_node.right;
            while let Some(n) = curr {
                self.stack.push(n);
                curr = n.as_ref().splay_node.left;
            }
            Some(n_ref)
        }
    }
}

impl Drop for SplayTree {
    fn drop(&mut self) {
        // Iterative drop to avoid stack overflow
        let mut current = self.root;
        while let Some(mut node) = current {
            unsafe {
                if let Some(mut left) = node.as_ref().splay_node.left {
                    node.as_mut().splay_node.left = left.as_ref().splay_node.right;
                    left.as_mut().splay_node.right = Some(node);
                    current = Some(left);
                } else {
                    let right = node.as_ref().splay_node.right;
                    let _ = alloc::boxed::Box::from_raw(node.as_ptr()); // drop the current node
                    current = right;
                }
            }
        }
    }
}
