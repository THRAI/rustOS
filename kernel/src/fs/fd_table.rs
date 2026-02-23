//! Per-process file descriptor table.
//!
//! Each process has an FdTable mapping fd numbers to Arc<OpenFile>.
//! OpenFile holds a vnode reference, current offset, and flags.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use super::vnode::{Vnode, VnodeId};

/// Open file flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags {
    pub read: bool,
    pub write: bool,
}

impl OpenFlags {
    pub const RDONLY: Self = Self { read: true, write: false };
    pub const WRONLY: Self = Self { read: false, write: true };
    pub const RDWR: Self = Self { read: true, write: true };
}

/// An open file: vnode + offset + flags.
/// Shared between parent and child after fork (POSIX semantics: offset is shared).
pub struct OpenFile {
    pub vnode: Arc<dyn Vnode>,
    pub offset: AtomicU64,
    pub flags: OpenFlags,
}

impl OpenFile {
    pub fn new(vnode: Arc<dyn Vnode>, flags: OpenFlags) -> Arc<Self> {
        Arc::new(Self {
            vnode,
            offset: AtomicU64::new(0),
            flags,
        })
    }
}

/// Per-process file descriptor table.
pub struct FdTable {
    /// fd number -> open file entry.
    entries: BTreeMap<u32, Arc<OpenFile>>,
    /// Next fd number to allocate.
    next_fd: u32,
}

impl FdTable {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            next_fd: 3, // 0,1,2 reserved for stdin/stdout/stderr
        }
    }

    /// Allocate a new fd for the given open file.
    pub fn insert(&mut self, file: Arc<OpenFile>) -> u32 {
        let fd = self.next_fd;
        self.next_fd += 1;
        self.entries.insert(fd, file);
        fd
    }

    /// Look up an open file by fd.
    pub fn get(&self, fd: u32) -> Option<&Arc<OpenFile>> {
        self.entries.get(&fd)
    }

    /// Remove (close) an fd. Returns the OpenFile if it existed.
    pub fn remove(&mut self, fd: u32) -> Option<Arc<OpenFile>> {
        self.entries.remove(&fd)
    }

    /// Fork: clone the fd table (all entries are Arc-shared per POSIX).
    pub fn fork(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            next_fd: self.next_fd,
        }
    }
}
