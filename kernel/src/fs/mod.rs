//! Filesystem subsystem.

#[cfg(not(feature = "full-fs"))]
mod bringup_stub;
#[cfg(feature = "full-fs")]
pub mod delegate;
pub mod dentry;
pub mod devfs;
#[cfg(feature = "full-fs")]
pub mod ext4;
pub mod fd_table;
#[cfg(feature = "full-fs")]
pub mod lwext4_disk;
pub mod mount;
pub mod path;
pub mod pipe;
pub mod stat;
pub mod symlink;
pub mod vnode;

#[cfg(not(feature = "full-fs"))]
pub use bringup_stub::{
    fs_cache_flush, fs_close, fs_link, fs_lookup, fs_mkdir, fs_open, fs_open_flags, fs_read,
    fs_read_page, fs_readdir, fs_readlink, fs_rename, fs_stat, fs_symlink, fs_truncate, fs_unlink,
    fs_write, fs_write_at, init as init_delegate, DirEntryRaw, FsFileHandle, READDIR_BATCH,
};
#[cfg(feature = "full-fs")]
pub use delegate::{
    fs_cache_flush, fs_close, fs_link, fs_lookup, fs_mkdir, fs_open, fs_open_flags, fs_read,
    fs_read_page, fs_readdir, fs_readlink, fs_rename, fs_stat, fs_symlink, fs_truncate, fs_unlink,
    fs_write, fs_write_at, init as init_delegate, DirEntryRaw, FsFileHandle, READDIR_BATCH,
};
pub use dentry::{
    init as init_dentry_cache, insert as insert_dentry, invalidate as invalidate_dentry,
    lookup as lookup_dentry,
};
pub use devfs::open_device;
#[cfg(feature = "full-fs")]
pub use ext4::{
    close as ext4_close, dir_close as ext4_dir_close, dir_next as ext4_dir_next,
    dir_open as ext4_dir_open, open as ext4_open, read as ext4_read, readlink as ext4_readlink,
    stat as ext4_stat, truncate as ext4_truncate, unlink as ext4_unlink, write as ext4_write,
    DelegateToken,
};
pub use fd_table::{DeviceKind, FdFlags, FdTable, FileDescription, FileObject, OpenFlags};
#[cfg(feature = "full-fs")]
pub use lwext4_disk::Disk;
pub use mount::{register_mount, resolve_to_source, same_mount_domain, unregister_mount};
pub use path::{absolutize_path, normalize_absolute_path, resolve};
pub use pipe::{ConsoleReadFuture, Pipe, PipeReadFuture, PipeWriteFuture};
pub use stat::{
    dirent_type_from_ext4, fill_stat_from_file_object, fill_stat_from_lookup,
    fill_statx_from_file_object, fill_statx_from_lookup, stat_mode_from_type, stat_zeroed,
    statx_from_stat, statx_zeroed, LinuxDirent64, LinuxStat, LinuxStatx, LinuxStatxTimestamp,
    STATX_ALL, STATX_BASIC_STATS, STATX_BLOCKS, STATX_BTIME, STATX_CTIME, STATX_GID, STATX_INO,
    STATX_MODE, STATX_MNT_ID, STATX_NLINK, STATX_SIZE, STATX_TYPE, STATX_UID,
};
pub use vnode::{
    init_vnode_cache, vnode_destroy_object, vnode_object, vnode_object_if_exists,
    vnode_objects_lock, Ext4Vnode, Vnode, VnodeId, VnodeType,
};
