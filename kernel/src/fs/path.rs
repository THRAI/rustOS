//! Path resolution: iterative walk from root vnode.
//!
//! Splits path on '/', calls delegate lookup for each component,
//! caches results in the dentry cache.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

use crate::{
    fs::{fs_lookup, fs_readlink, insert_dentry, lookup_dentry, Ext4Vnode, Vnode, VnodeType},
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
/// Symlinks (ext4 type 7) are followed transparently, up to 8 hops.
pub async fn resolve(path: &str) -> Result<Arc<dyn Vnode>, Errno> {
    resolve_depth(path, 0).await
}

fn parent_dir(path: &str) -> &str {
    if let Some(pos) = path.rfind('/') {
        if pos == 0 { "/" } else { &path[..pos] }
    } else {
        "/"
    }
}

async fn resolve_depth(path: &str, depth: u32) -> Result<Arc<dyn Vnode>, Errno> {
    if depth > 8 {
        return Err(Errno::Enoent); // symlink loop depth exceeded
    }

    let tail = path.trim_start_matches('/');
    if tail.is_empty() {
        return Ok(root_vnode());
    }

    let mut current: Arc<dyn Vnode> = root_vnode();
    let mut built_path = String::new();
    let components: Vec<&str> = tail.split('/').filter(|c| !c.is_empty() && *c != ".").collect();

    for (i, component) in components.iter().enumerate() {
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
                if child_type == 7 {
                    // Symlink: read target and follow it.
                    let (len, buf) =
                        fs_readlink(&built_path).await.map_err(|_| Errno::Enoent)?;
                    let raw = core::str::from_utf8(&buf[..len])
                        .map_err(|_| Errno::Enoent)?
                        .trim_end_matches('\0');
                    // Build full new path: symlink_target + remaining components.
                    let mut new_path = if raw.starts_with('/') {
                        String::from(raw)
                    } else {
                        let parent = parent_dir(&built_path);
                        let mut s = String::from(parent);
                        if !s.ends_with('/') {
                            s.push('/');
                        }
                        s.push_str(raw);
                        s
                    };
                    for rem in &components[i + 1..] {
                        new_path.push('/');
                        new_path.push_str(rem);
                    }
                    return Box::pin(resolve_depth(&new_path, depth + 1)).await;
                }
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
