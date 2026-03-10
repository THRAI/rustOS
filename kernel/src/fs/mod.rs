//! Filesystem subsystem.

pub mod delegate;
pub mod dentry;
pub mod devfs;
pub mod ext4;
pub mod fd_table;
pub mod lwext4_disk;
pub mod mount;
pub mod path;
pub mod pipe;
pub mod symlink;
pub mod vnode;

pub use delegate::{
    fs_cache_flush, fs_close, fs_link, fs_lookup, fs_mkdir, fs_open, fs_open_flags, fs_read,
    fs_read_page, fs_readdir, fs_readlink, fs_rename, fs_stat, fs_symlink, fs_truncate, fs_unlink,
    fs_write, fs_write_at, init as init_delegate, DirEntryRaw, FsFileHandle,
};
pub use dentry::{
    init as init_dentry_cache, insert as insert_dentry, invalidate as invalidate_dentry,
    lookup as lookup_dentry,
};
pub use devfs::open_device;
pub use ext4::flags as ext4_flags;
pub use ext4::{
    cache_flush as ext4_cache_flush, close as ext4_close, dir_close as ext4_dir_close,
    dir_next as ext4_dir_next, dir_open as ext4_dir_open, link as ext4_link, mkdir as ext4_mkdir,
    mount as ext4_mount, open as ext4_open, read as ext4_read, readlink as ext4_readlink,
    rename as ext4_rename, stat as ext4_stat, symlink as ext4_symlink, truncate as ext4_truncate,
    unlink as ext4_unlink, write as ext4_write, DelegateToken,
};
pub use fd_table::{DeviceKind, FdFlags, FdTable, FileDescription, FileObject, OpenFlags};
pub use lwext4_disk::Disk;
pub use mount::{register_mount, resolve_to_source, same_mount_domain, unregister_mount};
pub use path::resolve;
pub use pipe::Pipe;
pub use symlink::{
    create as create_symlink, read as read_symlink, remove as remove_symlink,
    resolve as resolve_symlink,
};
pub use vnode::{
    init_vnode_cache, vnode_destroy_object, vnode_object, vnode_object_if_exists, DirEntry,
    Ext4Vnode, Vnode, VnodeId, VnodeStat, VnodeType,
};
