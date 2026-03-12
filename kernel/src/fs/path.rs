//! Path resolution: iterative walk from root vnode.
//!
//! Splits path on '/', calls delegate lookup for each component,
//! caches results in the dentry cache.

use alloc::{string::String, sync::Arc};

use crate::{
    fs::{fs_lookup, insert_dentry, lookup_dentry, Ext4Vnode, Vnode, VnodeType},
    hal_common::Errno,
};

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
    let mut built_path = String::new();

    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }

        let parent_id = current.vnode_id();

        // Build the full path as we walk
        built_path.push('/');
        built_path.push_str(component);

        // Check dentry cache first
        if let Some(cached) = lookup_dentry(parent_id, component) {
            current = cached;
            continue;
        }

        // Not cached — ask delegate to look up this component
        let parent_ino = parent_id as u32;
        let result = fs_lookup(parent_ino, &built_path).await;

        match result {
            Ok((child_ino, child_type, child_size)) => {
                let vtype = if child_type == 2 {
                    VnodeType::Directory
                } else {
                    VnodeType::Regular
                };
                let child: Arc<dyn Vnode> =
                    Ext4Vnode::new_with_path(child_ino, vtype, child_size, built_path.clone());
                insert_dentry(parent_id, component, Arc::clone(&child));
                current = child;
            },
            Err(_) => return Err(Errno::Enoent),
        }
    }

    Ok(current)
}
