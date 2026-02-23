//! Vnode trait and Ext4Vnode implementation.
//!
//! The Vnode trait is the core filesystem abstraction. Ext4Vnode implements
//! it by sending operations to the delegate task via the bounded channel.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use hal_common::Errno;

/// Unique vnode identifier (inode number within a filesystem).
pub type VnodeId = u64;

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
}

/// Ext4 vnode: holds inode number, file type, and cached size.
pub struct Ext4Vnode {
    pub ino: u32,
    pub vtype: VnodeType,
    pub file_size: AtomicU64,
}

impl Ext4Vnode {
    pub fn new(ino: u32, vtype: VnodeType, size: u64) -> Arc<Self> {
        Arc::new(Self {
            ino,
            vtype,
            file_size: AtomicU64::new(size),
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
}
