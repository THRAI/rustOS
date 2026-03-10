//! Vnode trait and Ext4Vnode implementation.
//!
//! The Vnode trait is the core filesystem abstraction. Ext4Vnode implements
//! it by sending operations to the delegate task via the bounded channel.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::mm::vm::vm_object::VmObject;
use crate::hal_common::addr::VirtPageNum;
use crate::hal_common::PAGE_SIZE;
/// Unique vnode identifier (inode number within a filesystem).
pub type VnodeId = u64;
use spin::rwlock::RwLock;
use crate::hal_common::IrqSafeSpinLock;

static VNODE_OBJECTS: IrqSafeSpinLock<Option<BTreeMap<VnodeId, Arc<RwLock<VmObject>>>>> =
    IrqSafeSpinLock::new(None);

const MAX_CACHED_VNODES: usize = 64;

struct VnodeLru {
    entries: VecDeque<VnodeId>,
}

static VNODE_LRU: IrqSafeSpinLock<Option<VnodeLru>> = IrqSafeSpinLock::new(None);

pub fn init_vnode_cache() {
    *VNODE_OBJECTS.lock() = Some(BTreeMap::new());
    *VNODE_LRU.lock() = Some(VnodeLru {
        entries: VecDeque::new(),
    });
}

pub fn vnode_object(vnode: &dyn Vnode) -> Arc<RwLock<VmObject>> {
    let id = vnode.vnode_id();
    {
        let cache = VNODE_OBJECTS.lock();
        let map = cache.as_ref().expect("vnode cache not initialized");
        if let Some(existing) = map.get(&id) {
            let obj = Arc::clone(existing);
            drop(cache);
            // Promote in LRU
            if let Some(lru) = VNODE_LRU.lock().as_mut() {
                lru.entries.retain(|e| *e != id);
                lru.entries.push_back(id);
            }
            return obj;
        }
    }

    let obj = VmObject::new_file(vnode);
    {
        let mut cache = VNODE_OBJECTS.lock();
        let map = cache.as_mut().expect("vnode cache not initialized");
        // Double-check after re-acquiring lock
        if let Some(existing) = map.get(&id) {
            return Arc::clone(existing);
        }
        map.insert(id, Arc::clone(&obj));
    }
    if let Some(lru) = VNODE_LRU.lock().as_mut() {
        lru.entries.push_back(id);
    }
    evict_idle_vnodes();
    obj
}

pub fn vnode_object_if_exists(vnode_id: VnodeId) -> Option<Arc<RwLock<VmObject>>> {
    let cache = VNODE_OBJECTS.lock();
    let map = cache.as_ref()?;
    map.get(&vnode_id).cloned()
}

pub fn vnode_destroy_object(vnode_id: VnodeId) {
    {
        let mut cache = VNODE_OBJECTS.lock();
        if let Some(map) = cache.as_mut() {
            map.remove(&vnode_id);
        }
    }
    if let Some(lru) = VNODE_LRU.lock().as_mut() {
        lru.entries.retain(|e| *e != vnode_id);
    }
}

fn evict_idle_vnodes() {
    let mut cache = VNODE_OBJECTS.lock();
    let map = match cache.as_mut() {
        Some(m) => m,
        None => return,
    };
    if map.len() <= MAX_CACHED_VNODES {
        return;
    }

    let mut lru = VNODE_LRU.lock();
    let lru = match lru.as_mut() {
        Some(l) => l,
        None => return,
    };

    let mut to_remove = alloc::vec::Vec::new();
    for id in lru.entries.iter() {
        if map.len() - to_remove.len() <= MAX_CACHED_VNODES {
            break;
        }
        if let Some(arc) = map.get(id) {
            if Arc::strong_count(arc) == 1 {
                to_remove.push(*id);
            }
        }
    }
    for id in &to_remove {
        map.remove(id);
        lru.entries.retain(|e| e != id);
    }
}

/// File type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VnodeType {
    Regular,
    Directory,
}

/// File metadata.
#[derive(Debug, Clone)]
pub struct VnodeStat {
    pub vnode_id: VnodeId,
    pub vtype: VnodeType,
    pub size: u64,
}

/// Directory entry returned by readdir.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub vnode_id: VnodeId,
    pub vtype: VnodeType,
}

/// Filesystem vnode operations.
/// All methods that do I/O go through the delegate channel.
pub trait Vnode: Send + Sync {
    fn vnode_id(&self) -> VnodeId;
    fn vtype(&self) -> VnodeType;
    fn size(&self) -> u64;
    fn path(&self) -> &str;
    fn set_size(&self, size: u64);
}

/// Ext4 vnode: holds inode number, file type, cached size, and path.
pub struct Ext4Vnode {
    pub ino: u32,
    pub vtype: VnodeType,
    pub file_size: AtomicU64,
    pub path: String,
}

impl Ext4Vnode {
    pub fn new(ino: u32, vtype: VnodeType, size: u64) -> Arc<Self> {
        Arc::new(Self {
            ino,
            vtype,
            file_size: AtomicU64::new(size),
            path: String::new(),
        })
    }

    pub fn new_with_path(ino: u32, vtype: VnodeType, size: u64, path: String) -> Arc<Self> {
        Arc::new(Self {
            ino,
            vtype,
            file_size: AtomicU64::new(size),
            path,
        })
    }
}

impl Vnode for Ext4Vnode {
    fn vnode_id(&self) -> VnodeId {
        self.ino as VnodeId
    }

    fn vtype(&self) -> VnodeType {
        self.vtype
    }

    fn size(&self) -> u64 {
        self.file_size.load(Ordering::Relaxed)
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn set_size(&self, size: u64) {
        let old_size = self.file_size.swap(size, Ordering::Relaxed);
        if let Some(obj) = vnode_object_if_exists(self.ino as VnodeId) {
            let mut w = obj.write();
            if size < old_size {
                let new_pages = (size as usize + PAGE_SIZE - 1) / PAGE_SIZE;
                w.truncate_pages(VirtPageNum(new_pages));
            }
            w.set_size(size as usize);
        }
    }

}
