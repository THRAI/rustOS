//! lwext4-backed ext4 filesystem driver.
//!
//! All public I/O functions require `&mut DelegateToken`, enforcing at
//! compile time that only the single delegate task can call into lwext4.
//! Metadata queries (`stat`, `exists`) use the native C APIs
//! (`ext4_raw_inode_fill`, `ext4_inode_exist`) instead of file-open hacks.

use crate::fs::Disk;
use crate::hal_common::Errno;
use lwext4_rust::bindings::{self, ext4_dir, ext4_direntry, ext4_inode, EOK};
use lwext4_rust::{Ext4BlockWrapper, Ext4File, InodeTypes};

/// Map a raw lwext4 C error code (positive integer) to an `Errno`.
fn lwext4_err(rc: i32) -> Errno {
    match rc.unsigned_abs() {
        1 => Errno::Eperm,
        2 => Errno::Enoent,
        5 => Errno::Eio,
        9 => Errno::Ebadf,
        12 => Errno::Enomem,
        17 => Errno::Eexist,
        19 => Errno::Enodev,
        20 => Errno::Enotdir,
        21 => Errno::Eisdir,
        22 => Errno::Einval,
        24 => Errno::Emfile,
        34 => Errno::Erange,
        39 => Errno::Enotempty,
        _ => Errno::Eio,
    }
}

// SAFETY: All lwext4 access is serialized through the delegate task.
unsafe impl Send for Disk {}
unsafe impl Sync for Disk {}

struct SendSyncBW(Ext4BlockWrapper<Disk>);
unsafe impl Send for SendSyncBW {}
unsafe impl Sync for SendSyncBW {}

static EXT4_BW: crate::hal_common::Once<crate::hal_common::SpinMutex<SendSyncBW>> =
    crate::hal_common::Once::new();

// ── DelegateToken (Phase 2) ─────────────────────────────────────────

/// Proof that the caller is running inside the delegate task.
/// Only one instance exists, created in `delegate_task()`.
pub struct DelegateToken(());

impl DelegateToken {
    /// # Safety
    /// Must only be called once, inside the delegate task.
    pub(crate) unsafe fn new() -> Self {
        Self(())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Convert a `&str` path to a stack-allocated null-terminated buffer.
/// Returns `(buf, len_with_nul)`. Panics if path >= 256 bytes.
fn path_to_cstr(path: &str) -> [u8; 257] {
    let bytes = path.as_bytes();
    assert!(bytes.len() < 257, "path too long for ext4 cstr buffer");
    let mut buf = [0u8; 257];
    buf[..bytes.len()].copy_from_slice(bytes);
    // buf[bytes.len()] is already 0 (null terminator)
    buf
}

// ── VFS Flags (Phase 3) ─────────────────────────────────────────────

/// Standard POSIX-style open flags used by the VFS layer.
/// Eliminates magic numbers like `0x0010_0000`.
pub mod flags {
    pub const O_RDONLY: u32 = 0;
    pub const O_WRONLY: u32 = 1;
    pub const O_RDWR: u32 = 2;
    pub const O_CREAT: u32 = 0x0000_0040;
    pub const O_TRUNC: u32 = 0x0000_0200;
    pub const O_APPEND: u32 = 0x0000_0400;
    pub const O_DIRECTORY: u32 = 0x0010_0000;
}

// ── Mount ───────────────────────────────────────────────────────────

/// Mount the ext4 filesystem from VirtIO-blk.
pub fn mount() -> Result<(), Errno> {
    let disk = Disk::new();
    let bw = Ext4BlockWrapper::<Disk>::new(disk, "/", "ext4_fs").map_err(|e| {
        klog!(fs, error, "ext4 mount failed: {}", e);
        Errno::Eio
    })?;
    EXT4_BW.call_once(|| crate::hal_common::SpinMutex::new(SendSyncBW(bw)));
    crate::kprintln!("lwext4 mounted at /");
    Ok(())
}

// ── File I/O (require DelegateToken) ────────────────────────────────

/// Open a file. Returns an Ext4File handle.
pub fn open(_tok: &mut DelegateToken, path: &str, open_flags: u32) -> Result<Ext4File, Errno> {
    let ftype = if open_flags & flags::O_DIRECTORY != 0 {
        InodeTypes::EXT4_DE_DIR
    } else {
        InodeTypes::EXT4_DE_REG_FILE
    };
    let mut file = Ext4File::new(path, ftype);
    file.file_open(path, open_flags).map_err(lwext4_err)?;
    Ok(file)
}

/// Read from an open file into buf. Returns bytes read.
pub fn read(_tok: &mut DelegateToken, file: &mut Ext4File, buf: &mut [u8]) -> Result<usize, Errno> {
    file.file_read(buf).map_err(lwext4_err)
}

/// Write to an open file. Returns bytes written.
pub fn write(_tok: &mut DelegateToken, file: &mut Ext4File, buf: &[u8]) -> Result<usize, Errno> {
    file.file_write(buf).map_err(lwext4_err)
}

/// Truncate a file to `size` bytes.
pub fn truncate(_tok: &mut DelegateToken, path: &str, size: u64) -> Result<(), Errno> {
    let mut file = Ext4File::new(path, InodeTypes::EXT4_DE_REG_FILE);
    file.file_open(path, flags::O_RDWR).map_err(lwext4_err)?;
    file.file_truncate(size).map_err(lwext4_err)?;
    file.file_close().map_err(lwext4_err)?;
    Ok(())
}

/// Close an open file.
pub fn close(_tok: &mut DelegateToken, file: &mut Ext4File) -> Result<(), Errno> {
    file.file_close().map_err(lwext4_err)?;
    Ok(())
}

// ── Native stat via ext4_raw_inode_fill (Phase 1) ───────────────────

/// Stat a path: returns `(inode, file_size, file_type_u8)`.
/// file_type: 1=regular, 2=directory, 7=symlink.
///
/// Uses the native `ext4_raw_inode_fill` C API — no file_open needed.
pub fn stat(_tok: &mut DelegateToken, path: &str) -> Result<(u32, u64, u8), Errno> {
    let cpath = path_to_cstr(path);
    let mut ino: u32 = 0;
    let mut inode: ext4_inode = unsafe { core::mem::zeroed() };

    let rc = unsafe {
        bindings::ext4_raw_inode_fill(
            cpath.as_ptr() as *const core::ffi::c_char,
            &mut ino,
            &mut inode,
        )
    };

    if rc != EOK as i32 {
        return Err(lwext4_err(rc));
    }

    let mode = inode.mode;
    let ftype = match mode & 0xF000 {
        0x4000 => 2u8, // S_IFDIR
        0xA000 => 7u8, // S_IFLNK
        _ => 1u8,      // S_IFREG and fallback
    };
    let size = (inode.size_hi as u64) << 32 | (inode.size_lo as u64);
    Ok((ino, size, ftype))
}

/// Check if an inode exists at the given path.
pub fn exists(_tok: &mut DelegateToken, path: &str) -> bool {
    let cpath = path_to_cstr(path);
    let ptr = cpath.as_ptr() as *const core::ffi::c_char;
    let rc = unsafe { bindings::ext4_inode_exist(ptr, 1) }; // EXT4_DE_REG_FILE
    if rc == EOK as i32 {
        return true;
    }
    let rc = unsafe { bindings::ext4_inode_exist(ptr, 2) }; // EXT4_DE_DIR
    rc == EOK as i32
}

// ── Directory iteration (native C API) ──────────────────────────────

/// Open a directory for iteration. Returns an opaque `ext4_dir` handle.
pub fn dir_open(_tok: &mut DelegateToken, path: &str) -> Result<ext4_dir, Errno> {
    let cpath = path_to_cstr(path);
    let mut dir: ext4_dir = unsafe { core::mem::zeroed() };
    let rc =
        unsafe { bindings::ext4_dir_open(&mut dir, cpath.as_ptr() as *const core::ffi::c_char) };
    if rc != EOK as i32 {
        return Err(lwext4_err(rc));
    }
    Ok(dir)
}

/// Read the next directory entry. Returns `None` when exhausted.
/// Yields `(name_bytes, name_len, inode_type, inode_number)`.
pub fn dir_next(_tok: &mut DelegateToken, dir: &mut ext4_dir) -> Option<([u8; 255], u8, u8, u32)> {
    let de_ptr = unsafe { bindings::ext4_dir_entry_next(dir) };
    if de_ptr.is_null() {
        return None;
    }
    let de: &ext4_direntry = unsafe { &*de_ptr };
    if de.name_length == 0 {
        return None;
    }
    Some((de.name, de.name_length, de.inode_type, de.inode))
}

/// Close a directory handle.
pub fn dir_close(_tok: &mut DelegateToken, dir: &mut ext4_dir) -> Result<(), Errno> {
    let rc = unsafe { bindings::ext4_dir_close(dir) };
    if rc != EOK as i32 {
        return Err(lwext4_err(rc));
    }
    Ok(())
}

/// Create a directory.
pub fn mkdir(_tok: &mut DelegateToken, path: &str) -> Result<(), Errno> {
    let mut file = Ext4File::new(path, InodeTypes::EXT4_DE_DIR);
    file.dir_mk(path).map_err(lwext4_err)?;
    Ok(())
}

/// Remove a file.
pub fn unlink(_tok: &mut DelegateToken, path: &str) -> Result<(), Errno> {
    let mut file = Ext4File::new(path, InodeTypes::EXT4_DE_REG_FILE);
    file.file_remove(path).map_err(lwext4_err)?;
    Ok(())
}

/// Create a hard link: `new_path` links to `old_path`.
pub fn link(_tok: &mut DelegateToken, old_path: &str, new_path: &str) -> Result<(), Errno> {
    let file = Ext4File::new(old_path, InodeTypes::EXT4_DE_REG_FILE);
    let _ = file.link_create(new_path).map_err(lwext4_err)?;
    Ok(())
}

/// Rename/move a file or directory.
pub fn rename(_tok: &mut DelegateToken, old_path: &str, new_path: &str) -> Result<(), Errno> {
    let mut file = Ext4File::new(old_path, InodeTypes::EXT4_DE_REG_FILE);
    let _ = file.file_rename(old_path, new_path).map_err(lwext4_err)?;
    Ok(())
}

/// Create a symbolic link: `path` points to `target`.
pub fn symlink(_tok: &mut DelegateToken, target: &str, path: &str) -> Result<(), Errno> {
    let file = Ext4File::new(path, InodeTypes::EXT4_DE_SYMLINK);
    let _ = file.symlink_create(target, path).map_err(lwext4_err)?;
    Ok(())
}

/// Read symbolic link content into `buf`. Returns bytes written.
pub fn readlink(_tok: &mut DelegateToken, path: &str, buf: &mut [u8]) -> Result<usize, Errno> {
    let file = Ext4File::new(path, InodeTypes::EXT4_DE_SYMLINK);
    file.symlink_read(buf).map_err(lwext4_err)
}

/// Flush filesystem cache for the mount point that contains `path`.
pub fn cache_flush(_tok: &mut DelegateToken, path: &str) -> Result<(), Errno> {
    let cpath = path_to_cstr(path);
    let rc = unsafe { bindings::ext4_cache_flush(cpath.as_ptr() as *const core::ffi::c_char) };
    if rc != EOK as i32 {
        return Err(lwext4_err(rc));
    }
    Ok(())
}
