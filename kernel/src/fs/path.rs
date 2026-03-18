//! Path resolution: iterative walk from root vnode.
//!
//! Splits path on '/', calls delegate lookup for each component,
//! caches results in the dentry cache.

use alloc::{string::String, sync::Arc, vec::Vec};

use crate::{
    fs::{fs_lookup, insert_dentry, lookup_dentry, Ext4Vnode, Vnode, VnodeType},
    hal_common::Errno,
    proc::Task,
};

/// Root inode number for ext4.
const EXT4_ROOT_INO: u32 = 2;

/// Linux AT_FDCWD sentinel: "use process cwd".
const AT_FDCWD: isize = -100;

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

/// Normalize a path to an absolute canonical form.
/// Collapses duplicate '/', '.' and '..' components.
pub fn normalize_absolute_path(path: &str) -> String {
    let mut comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {},
            ".." => {
                let _ = comps.pop();
            },
            _ => comps.push(comp),
        }
    }

    if comps.is_empty() {
        return String::from("/");
    }

    let mut out = String::from("/");
    for (idx, comp) in comps.iter().enumerate() {
        if idx > 0 {
            out.push('/');
        }
        out.push_str(comp);
    }
    out
}

/// Convert a user-provided path to an absolute path with cwd/dirfd semantics.
pub fn absolutize_path(task: &Arc<Task>, dirfd: isize, raw_path: &str) -> Result<String, Errno> {
    if raw_path.is_empty() {
        return Err(Errno::Enoent);
    }
    if raw_path.starts_with('/') {
        return Ok(normalize_absolute_path(raw_path));
    }

    // Relative path from cwd.
    if dirfd == AT_FDCWD {
        let cwd = task.cwd.lock().clone();
        let mut combined = String::new();
        if cwd == "/" {
            combined.push('/');
            combined.push_str(raw_path);
        } else {
            combined.push_str(&cwd);
            combined.push('/');
            combined.push_str(raw_path);
        }
        return Ok(normalize_absolute_path(&combined));
    }

    // Relative path from directory fd.
    if dirfd >= 0 {
        let base = {
            let tab = task.fd_table.lock();
            let desc = tab.get(dirfd as u32).ok_or(Errno::Ebadf)?;
            match &desc.object {
                crate::fs::FileObject::Vnode(v) => {
                    if v.vtype() != crate::fs::VnodeType::Directory {
                        return Err(Errno::Enotdir);
                    }
                    String::from(v.path())
                },
                _ => return Err(Errno::Enotdir),
            }
        };
        let mut combined = String::new();
        if base == "/" {
            combined.push('/');
            combined.push_str(raw_path);
        } else {
            combined.push_str(&base);
            combined.push('/');
            combined.push_str(raw_path);
        }
        return Ok(normalize_absolute_path(&combined));
    }

    Err(Errno::Einval)
}
