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

/// Linux `struct statx_timestamp`.
#[repr(C)]
pub struct LinuxStatxTimestamp {
    pub tv_sec: i64,
    pub tv_nsec: u32,
    pub __reserved: i32,
}

/// Linux `struct statx` (256 bytes).
#[repr(C)]
pub struct LinuxStatx {
    pub stx_mask: u32,
    pub stx_blksize: u32,
    pub stx_attributes: u64,
    pub stx_nlink: u32,
    pub stx_uid: u32,
    pub stx_gid: u32,
    pub stx_mode: u16,
    pub __spare0: [u16; 1],
    pub stx_ino: u64,
    pub stx_size: u64,
    pub stx_blocks: u64,
    pub stx_attributes_mask: u64,
    pub stx_atime: LinuxStatxTimestamp,
    pub stx_btime: LinuxStatxTimestamp,
    pub stx_ctime: LinuxStatxTimestamp,
    pub stx_mtime: LinuxStatxTimestamp,
    pub stx_rdev_major: u32,
    pub stx_rdev_minor: u32,
    pub stx_dev_major: u32,
    pub stx_dev_minor: u32,
    pub stx_mnt_id: u64,
    pub stx_dio_mem_align: u32,
    pub stx_dio_offset_align: u32,
    pub __spare3: [u64; 12],
}

pub const STATX_TYPE: u32 = 0x0000_0001;
pub const STATX_MODE: u32 = 0x0000_0002;
pub const STATX_NLINK: u32 = 0x0000_0004;
pub const STATX_UID: u32 = 0x0000_0008;
pub const STATX_GID: u32 = 0x0000_0010;
pub const STATX_ATIME: u32 = 0x0000_0020;
pub const STATX_MTIME: u32 = 0x0000_0040;
pub const STATX_CTIME: u32 = 0x0000_0080;
pub const STATX_INO: u32 = 0x0000_0100;
pub const STATX_SIZE: u32 = 0x0000_0200;
pub const STATX_BLOCKS: u32 = 0x0000_0400;
pub const STATX_BTIME: u32 = 0x0000_0800;
pub const STATX_MNT_ID: u32 = 0x0000_1000;
pub const STATX_BASIC_STATS: u32 = 0x0000_07ff;
pub const STATX_ALL: u32 = 0x0000_0fff;

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

/// Build a zeroed `LinuxStatx` with conservative defaults.
#[inline]
pub fn statx_zeroed() -> LinuxStatx {
    LinuxStatx {
        stx_mask: 0,
        stx_blksize: 4096,
        stx_attributes: 0,
        stx_nlink: 1,
        stx_uid: 0,
        stx_gid: 0,
        stx_mode: 0,
        __spare0: [0; 1],
        stx_ino: 0,
        stx_size: 0,
        stx_blocks: 0,
        stx_attributes_mask: 0,
        stx_atime: LinuxStatxTimestamp {
            tv_sec: 0,
            tv_nsec: 0,
            __reserved: 0,
        },
        stx_btime: LinuxStatxTimestamp {
            tv_sec: 0,
            tv_nsec: 0,
            __reserved: 0,
        },
        stx_ctime: LinuxStatxTimestamp {
            tv_sec: 0,
            tv_nsec: 0,
            __reserved: 0,
        },
        stx_mtime: LinuxStatxTimestamp {
            tv_sec: 0,
            tv_nsec: 0,
            __reserved: 0,
        },
        stx_rdev_major: 0,
        stx_rdev_minor: 0,
        stx_dev_major: 0,
        stx_dev_minor: 0,
        stx_mnt_id: 0,
        stx_dio_mem_align: 0,
        stx_dio_offset_align: 0,
        __spare3: [0; 12],
    }
}

/// Convert a minimal `stat` view to the richer `statx` ABI.
#[inline]
pub fn statx_from_stat(st: &LinuxStat, mask: u32) -> LinuxStatx {
    let mut out = statx_zeroed();
    let supported = STATX_BASIC_STATS | STATX_BTIME | STATX_MNT_ID;
    let mut visible = mask & supported;
    if visible == 0 {
        visible = STATX_BASIC_STATS;
    }
    visible |= STATX_TYPE | STATX_MODE | STATX_INO | STATX_SIZE | STATX_BLOCKS;

    out.stx_mask = visible;
    out.stx_blksize = st.st_blksize as u32;
    out.stx_nlink = st.st_nlink;
    out.stx_uid = st.st_uid;
    out.stx_gid = st.st_gid;
    out.stx_mode = st.st_mode as u16;
    out.stx_ino = st.st_ino;
    out.stx_size = st.st_size.max(0) as u64;
    out.stx_blocks = st.st_blocks.max(0) as u64;
    out.stx_atime.tv_sec = st.st_atime;
    out.stx_atime.tv_nsec = st.st_atime_nsec.max(0) as u32;
    out.stx_btime.tv_sec = st.st_ctime;
    out.stx_btime.tv_nsec = st.st_ctime_nsec.max(0) as u32;
    out.stx_ctime.tv_sec = st.st_ctime;
    out.stx_ctime.tv_nsec = st.st_ctime_nsec.max(0) as u32;
    out.stx_mtime.tv_sec = st.st_mtime;
    out.stx_mtime.tv_nsec = st.st_mtime_nsec.max(0) as u32;
    out
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
                DeviceKind::Rtc => 0x00fe,
            };
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

/// Fill a `LinuxStatx` from a `FileObject`.
#[inline]
pub fn fill_statx_from_file_object(obj: &FileObject, mask: u32) -> LinuxStatx {
    let st = fill_stat_from_file_object(obj);
    statx_from_stat(&st, mask)
}

/// Fill a `LinuxStatx` from raw inode metadata (used by statx path).
#[inline]
pub fn fill_statx_from_lookup(ino: u32, ftype: u8, size: u64, mask: u32) -> LinuxStatx {
    let st = fill_stat_from_lookup(ino, ftype, size);
    statx_from_stat(&st, mask)
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
