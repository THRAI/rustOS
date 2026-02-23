//! Path resolution: iterative walk from root vnode.
//!
//! Splits path on '/', calls delegate lookup for each component,
//! caches results in the dentry cache.

use alloc::sync::Arc;
use hal_common::Errno;

use super::dentry;
use super::delegate;
use super::vnode::{Ext4Vnode, Vnode, VnodeId, VnodeType};

/// Root inode number for ext4.
const EXT4_ROOT_INO: u32 = 2;

/// Get the root vnode.
pub fn root_vnode() -> Arc<dyn Vnode> {
    Ext4Vnode::new(EXT4_ROOT_INO, VnodeType::Directory, 0) as Arc<dyn Vnode>
}

/// Resolve a path to a vnode, walking from root.
///
/// Path must be absolute (starts with '/').
/// Each component is looked up via dentry cache first, then delegate.
pub async fn resolve(path: &str) -> Result<Arc<dyn Vnode>, Errno> {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return Ok(root_vnode());
    }

    let mut current: Arc<dyn Vnode> = root_vnode();

    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }

        let parent_id = current.vnode_id();

        // Check dentry cache first
        if let Some(cached) = dentry::lookup(parent_id, component) {
            current = cached;
            continue;
        }

        // Not cached — ask delegate to look up this component
        let parent_ino = parent_id as u32;
        let result = delegate::fs_lookup(parent_ino, component).await;

        match result {
            Ok((child_ino, child_type, child_size)) => {
                let vtype = if child_type == 2 {
                    VnodeType::Directory
                } else {
                    VnodeType::Regular
                };
                let child: Arc<dyn Vnode> = Ext4Vnode::new(child_ino, vtype, child_size);
                dentry::insert(parent_id, component, Arc::clone(&child));
                current = child;
            }
            Err(_) => return Err(Errno::ENOENT),
        }
    }

    Ok(current)
}
