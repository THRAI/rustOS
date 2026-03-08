//! Minimal mount table for syscall-level mount/umount2 compatibility.
//!
//! This keeps mount state in VFS layer without switching the real backend.
//! It is intentionally small: enough for oscomp/basic mount lifecycle.

use alloc::string::String;
use alloc::vec::Vec;
use crate::hal_common::{Errno, SpinMutex};

const MAX_MOUNTS: usize = 64;

#[derive(Clone)]
pub struct MountEntry {
    pub source: String,
    pub target: String,
    pub fstype: String,
    pub flags: usize,
}

struct MountTable {
    entries: Vec<MountEntry>,
}

impl MountTable {
    const fn new() -> Self {
        Self { entries: Vec::new() }
    }

    fn add(&mut self, entry: MountEntry) -> Result<(), Errno> {
        if self.entries.len() >= MAX_MOUNTS {
            return Err(Errno::Enomem);
        }
        if self.entries.iter().any(|m| m.target == entry.target) {
            // Linux would report EBUSY; fallback to EEXIST in current errno set.
            return Err(Errno::Eexist);
        }
        self.entries.push(entry);
        Ok(())
    }

    fn remove_by_target(&mut self, target: &str) -> Result<(), Errno> {
        if let Some(idx) = self.entries.iter().position(|m| m.target == target) {
            self.entries.remove(idx);
            Ok(())
        } else {
            // Linux commonly reports EINVAL for non-mountpoint.
            Err(Errno::Einval)
        }
    }
}

static MOUNT_TABLE: SpinMutex<MountTable> = SpinMutex::new(MountTable::new());

pub fn register_mount(source: &str, target: &str, fstype: &str, flags: usize) -> Result<(), Errno> {
    if source.is_empty() || target.is_empty() || fstype.is_empty() {
        return Err(Errno::Einval);
    }

    let mut table = MOUNT_TABLE.lock();
    table.add(MountEntry {
        source: String::from(source),
        target: String::from(target),
        fstype: String::from(fstype),
        flags,
    })
}

pub fn unregister_mount(target: &str) -> Result<(), Errno> {
    if target.is_empty() {
        return Err(Errno::Einval);
    }

    let mut table = MOUNT_TABLE.lock();
    table.remove_by_target(target)
}

pub fn is_mounted(target: &str) -> bool {
    let table = MOUNT_TABLE.lock();
    table.entries.iter().any(|m| m.target == target)
}
