//! Vnode trait and Ext4Vnode implementation.
//!
//! The Vnode trait is the core filesystem abstraction. Ext4Vnode implements
//! it by sending operations to the delegate task via the bounded channel.

use alloc::string::String;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::mm::vm::vm_object::VmObject;
/// Unique vnode identifier (inode number within a filesystem).
pub type VnodeId = u64;
use spin::rwlock::RwLock;

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
    fn grab_obj(&mut self) -> Arc<RwLock<VmObject>>;
}

/// Ext4 vnode: holds inode number, file type, cached size, and path.
pub struct Ext4Vnode {
    pub ino: u32,
    pub vtype: VnodeType,
    pub file_size: AtomicU64,
    pub path: String,
    pub vm_obj: Option<Arc<RwLock<VmObject>>>,
}

impl Ext4Vnode {
    pub fn new(ino: u32, vtype: VnodeType, size: u64) -> Arc<Self> {
        Arc::new(Self {
            ino,
            vtype,
            file_size: AtomicU64::new(size),
            path: String::new(),
            vm_obj: None,
        })
    }

    pub fn new_with_path(ino: u32, vtype: VnodeType, size: u64, path: String) -> Arc<Self> {
        Arc::new(Self {
            ino,
            vtype,
            file_size: AtomicU64::new(size),
            path,
            vm_obj: None,
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
        self.file_size.store(size, Ordering::Relaxed);
    }

    /// Get or create the unified page cache of this vnode.
    fn grab_obj(&mut self) -> Arc<RwLock<VmObject>> {
        if let Some(obj) = &self.vm_obj {
            return Arc::clone(obj)
        }

        let new_obj = VmObject::new_file(self);
        self.vm_obj = Some(Arc::clone(&new_obj));
        new_obj
    }
}
