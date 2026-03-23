//! Minimal filesystem stubs for early bring-up builds.
//!
//! These keep the kernel compiling while the architecture boot path is being
//! validated before the ext4 delegate stack is enabled.

use crate::hal_common::{Errno, PhysAddr};

/// File handle placeholder used by bring-up builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsFileHandle(pub u16);

/// A directory entry returned by ReadDir.
#[derive(Clone, Copy)]
pub struct DirEntryRaw {
    pub name: [u8; 255],
    pub name_len: u8,
    pub inode_type: u8,
    pub inode: u32,
}

/// ReadDir result: up to 32 entries per batch.
pub const READDIR_BATCH: usize = 32;

#[inline]
fn fs_disabled<T>() -> Result<T, Errno> {
    Err(Errno::Enosys)
}

pub async fn fs_open(_path: &str) -> Result<FsFileHandle, Errno> {
    fs_disabled()
}

pub async fn fs_open_flags(_path: &str, _flags: u32) -> Result<FsFileHandle, Errno> {
    fs_disabled()
}

pub async fn fs_read(_handle: FsFileHandle, _buf: &mut [u8]) -> Result<usize, Errno> {
    fs_disabled()
}

pub async fn fs_write(_handle: FsFileHandle, _buf: &[u8]) -> Result<usize, Errno> {
    fs_disabled()
}

pub async fn fs_close(_handle: FsFileHandle) -> Result<(), Errno> {
    fs_disabled()
}

pub async fn fs_lookup(_parent_ino: u32, _name: &str) -> Result<(u32, u8, u64), Errno> {
    fs_disabled()
}

pub async fn fs_stat(_ino: u32) -> Result<(u64, u8), Errno> {
    fs_disabled()
}

pub async fn fs_read_page(_path: &str, _offset: u64, _pa: PhysAddr) -> Result<(), Errno> {
    fs_disabled()
}

pub async fn fs_write_at(_path: &str, _offset: u64, _data: &[u8]) -> Result<usize, Errno> {
    fs_disabled()
}

pub async fn fs_truncate(_path: &str, _size: u64) -> Result<(), Errno> {
    fs_disabled()
}

pub async fn fs_mkdir(_path: &str) -> Result<(), Errno> {
    fs_disabled()
}

pub async fn fs_unlink(_path: &str, _is_dir: bool) -> Result<(), Errno> {
    fs_disabled()
}

pub async fn fs_link(_old_path: &str, _new_path: &str) -> Result<(), Errno> {
    fs_disabled()
}

pub async fn fs_rename(_old_path: &str, _new_path: &str) -> Result<(), Errno> {
    fs_disabled()
}

pub async fn fs_symlink(_target: &str, _path: &str) -> Result<(), Errno> {
    fs_disabled()
}

pub async fn fs_readlink(_path: &str) -> Result<(usize, [u8; 256]), Errno> {
    fs_disabled()
}

pub async fn fs_cache_flush(_path: &str) -> Result<(), Errno> {
    fs_disabled()
}

pub async fn fs_readdir(
    _path: &str,
    _start_idx: usize,
) -> Result<([DirEntryRaw; READDIR_BATCH], usize), Errno> {
    fs_disabled()
}

pub fn init() {
    crate::klog!(fs, info, "filesystem delegate disabled because full-fs feature is off");
}
