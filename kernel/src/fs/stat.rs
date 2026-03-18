//! Linux ABI stat and dirent structures — pure data transforms.
//!
//! No IO, no locks, no kernel side effects.  The syscall layer calls
//! these helpers to fill ABI structs from vnode/pipe/device metadata.

use crate::fs::{DeviceKind, FileObject, VnodeType};

// ---------------------------------------------------------------------------
// stat
// ---------------------------------------------------------------------------

/// Linux `struct stat` for rv64 (128 bytes).
#[repr(C)]
pub struct LinuxStat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_mode: u32,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub __pad1: u64,
    pub st_size: i64,
    pub st_blksize: i32,
    pub __pad2: i32,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: i64,
    pub st_mtime: i64,
    pub st_mtime_nsec: i64,
    pub st_ctime: i64,
    pub st_ctime_nsec: i64,
    pub __unused: [i32; 2],
}

/// Convert lwext4 inode type byte to Linux `st_mode`.
///
/// Pure mapping — no IO.
#[inline]
pub fn stat_mode_from_type(file_type: u8) -> u32 {
    match file_type {
        2 => 0o040755, // directory
        7 => 0o120777, // symlink
        _ => 0o100644, // regular
    }
}

/// Build a zeroed `LinuxStat` with default block size.
#[inline]
pub fn stat_zeroed() -> LinuxStat {
    LinuxStat {
        st_dev: 0,
        st_ino: 0,
        st_mode: 0,
        st_nlink: 1,
        st_uid: 0,
        st_gid: 0,
        st_rdev: 0,
        __pad1: 0,
        st_size: 0,
        st_blksize: 4096,
        __pad2: 0,
        st_blocks: 0,
        st_atime: 0,
        st_atime_nsec: 0,
        st_mtime: 0,
        st_mtime_nsec: 0,
        st_ctime: 0,
        st_ctime_nsec: 0,
        __unused: [0; 2],
    }
}

/// Fill a `LinuxStat` from a `FileObject`.
///
/// Pure function — reads metadata from the vnode/pipe/device but
/// performs no IO.  Caller is responsible for copying the result
/// to user memory.
pub fn fill_stat_from_file_object(obj: &FileObject) -> LinuxStat {
    let mut st = stat_zeroed();
    match obj {
        FileObject::Vnode(v) => {
            let size = v.size();
            st.st_size = size as i64;
            st.st_blocks = size.div_ceil(512) as i64;
            st.st_ino = v.vnode_id();
            let ftype = match v.vtype() {
                VnodeType::Regular => 1u8,
                VnodeType::Directory => 2u8,
            };
            st.st_mode = stat_mode_from_type(ftype);
        },
        FileObject::PipeRead(_) | FileObject::PipeWrite(_) => {
            st.st_mode = 0o010600; // S_IFIFO | rw
        },
        FileObject::Device(dk) => {
            st.st_mode = 0o020666; // S_IFCHR | rw
            st.st_rdev = match dk {
                DeviceKind::Null => 0x0103,
                DeviceKind::Zero => 0x0105,
                DeviceKind::ConsoleRead | DeviceKind::ConsoleWrite => 0x0501,
            };
        },
        FileObject::Socket(_) => {
            st.st_mode = 0o140777; // S_IFSOCK | rwx
        },
    }
    st
}

/// Fill a `LinuxStat` from raw inode metadata (used by fstatat path).
///
/// Pure function — no IO.
pub fn fill_stat_from_lookup(ino: u32, ftype: u8, size: u64) -> LinuxStat {
    let mut st = stat_zeroed();
    st.st_ino = ino as u64;
    st.st_mode = stat_mode_from_type(ftype);
    st.st_size = size as i64;
    st.st_blocks = size.div_ceil(512) as i64;
    st
}

// ---------------------------------------------------------------------------
// dirent
// ---------------------------------------------------------------------------

/// Linux `struct linux_dirent64` header (variable-length, d_name follows).
#[repr(C)]
pub struct LinuxDirent64 {
    pub d_ino: u64,
    pub d_off: i64,
    pub d_reclen: u16,
    pub d_type: u8,
    // d_name follows (variable length, null-terminated)
}

/// Convert lwext4 inode type to Linux `d_type`.
///
/// Pure mapping — no IO.
#[inline]
pub fn dirent_type_from_ext4(inode_type: u8) -> u8 {
    match inode_type {
        2 => 4,  // DT_DIR
        1 => 8,  // DT_REG
        7 => 10, // DT_LNK
        _ => 8,  // default to DT_REG
    }
}
