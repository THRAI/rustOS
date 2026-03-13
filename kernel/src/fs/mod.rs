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
    fs_cache_flush, fs_close, fs_link, fs_lookup, fs_mkdir, fs_open_flags, fs_read_page,
    fs_readdir, fs_readlink, fs_rename, fs_symlink, fs_truncate, fs_unlink, fs_write_at,
    init as init_delegate,
};
pub use dentry::{
    init as init_dentry_cache, insert as insert_dentry, invalidate as invalidate_dentry,
    lookup as lookup_dentry,
};
pub use devfs::open_device;
pub use ext4::{
    close as ext4_close, dir_close as ext4_dir_close, dir_next as ext4_dir_next,
    dir_open as ext4_dir_open, open as ext4_open, read as ext4_read, readlink as ext4_readlink,
    stat as ext4_stat, truncate as ext4_truncate, unlink as ext4_unlink, write as ext4_write,
    DelegateToken,
};
pub use fd_table::{DeviceKind, FdFlags, FdTable, FileDescription, FileObject, OpenFlags};
pub use lwext4_disk::Disk;
pub use mount::{register_mount, resolve_to_source, same_mount_domain, unregister_mount};
pub use path::resolve;
pub use pipe::Pipe;
pub use vnode::{
    init_vnode_cache, vnode_destroy_object, vnode_object, vnode_object_if_exists,
    vnode_objects_lock, Ext4Vnode, Vnode, VnodeId, VnodeType,
};
