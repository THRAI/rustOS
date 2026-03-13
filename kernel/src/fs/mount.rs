//! Minimal mount namespace table.
//!
//! Provides:
//! 1. mount lifecycle bookkeeping.
//! 2. longest-prefix path translation from namespace path to backend path.
//! 3. a basic "same mount domain" check for link/rename semantics.

use alloc::{string::String, vec::Vec};

use crate::hal_common::{Errno, SpinMutex};

const MAX_MOUNTS: usize = 64;

#[derive(Clone)]
pub struct MountEntry {
    /// Raw source string from mount(2), such as "dev:/dev/vda2".
    pub source: String,
    /// Canonical backend root path used for real ext4 operations.
    pub source_path: String,
    /// Namespace mountpoint path.
    pub target: String,
    pub fstype: String,
    pub flags: usize,
}

struct MountTable {
    entries: Vec<MountEntry>,
}

impl MountTable {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn add(&mut self, entry: MountEntry) -> Result<(), Errno> {
        if self.entries.len() >= MAX_MOUNTS {
            return Err(Errno::Enomem);
        }
        if self.entries.iter().any(|m| m.target == entry.target) {
            // Linux would report EBUSY; fallback to EEXIST in current errno set.
            return Err(Errno::Eexist);
        }
        self.entries.push(entry);
        Ok(())
    }

    fn remove_by_target(&mut self, target: &str) -> Result<(), Errno> {
        if let Some(idx) = self.entries.iter().position(|m| m.target == target) {
            self.entries.remove(idx);
            Ok(())
        } else {
            // Linux commonly reports EINVAL for non-mountpoint.
            Err(Errno::Einval)
        }
    }
}

static MOUNT_TABLE: SpinMutex<MountTable, 5> = SpinMutex::new(MountTable::new());

fn normalize_absolute_path(path: &str) -> String {
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
    for (i, comp) in comps.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(comp);
    }
    out
}

fn parse_source_root(source: &str) -> Result<String, Errno> {
    // Accept "dev:/path" and "/path" styles.
    let source_path = if let Some(rest) = source.strip_prefix("dev:") {
        rest
    } else {
        source
    };
    if !source_path.starts_with('/') {
        return Err(Errno::Einval);
    }
    Ok(normalize_absolute_path(source_path))
}

fn is_target_prefix(path: &str, target: &str) -> bool {
    if target == "/" {
        return true;
    }
    if path == target {
        return true;
    }
    path.starts_with(target) && path.as_bytes().get(target.len()) == Some(&b'/')
}

fn mount_suffix<'a>(path: &'a str, target: &str) -> &'a str {
    if target == "/" {
        path
    } else if path.len() == target.len() {
        ""
    } else {
        &path[target.len()..]
    }
}

fn join_mapped_path(source_root: &str, suffix: &str) -> String {
    if source_root == "/" {
        if suffix.is_empty() {
            return String::from("/");
        }
        return normalize_absolute_path(suffix);
    }
    if suffix.is_empty() {
        return String::from(source_root);
    }
    let mut out = String::from(source_root);
    if out.ends_with('/') && suffix.starts_with('/') {
        out.push_str(&suffix[1..]);
    } else {
        out.push_str(suffix);
    }
    normalize_absolute_path(&out)
}

fn best_match<'a>(table: &'a MountTable, path: &str) -> Option<&'a MountEntry> {
    table
        .entries
        .iter()
        .filter(|m| is_target_prefix(path, &m.target))
        .max_by_key(|m| m.target.len())
}

pub fn register_mount(source: &str, target: &str, fstype: &str, flags: usize) -> Result<(), Errno> {
    if source.is_empty() || target.is_empty() || fstype.is_empty() {
        return Err(Errno::Einval);
    }
    if !target.starts_with('/') {
        return Err(Errno::Einval);
    }
    let source_path = parse_source_root(source)?;
    let target_path = normalize_absolute_path(target);

    let mut table = MOUNT_TABLE.lock();
    table.add(MountEntry {
        source: String::from(source),
        source_path,
        target: target_path,
        fstype: String::from(fstype),
        flags,
    })
}

pub fn unregister_mount(target: &str) -> Result<(), Errno> {
    if target.is_empty() {
        return Err(Errno::Einval);
    }
    if !target.starts_with('/') {
        return Err(Errno::Einval);
    }
    let target = normalize_absolute_path(target);

    let mut table = MOUNT_TABLE.lock();
    table.remove_by_target(&target)
}

pub fn is_mounted(target: &str) -> bool {
    if !target.starts_with('/') {
        return false;
    }
    let target = normalize_absolute_path(target);
    let table = MOUNT_TABLE.lock();
    table.entries.iter().any(|m| m.target == target)
}

/// Translate a namespace path to backend path using longest mountpoint match.
///
/// Example:
/// mount("dev:/dev/vda2", "/mnt", ...)
/// then:
///   /mnt/a/b -> /dev/vda2/a/b
///   /tmp/x   -> /tmp/x
pub fn resolve_to_source(path: &str) -> String {
    if !path.starts_with('/') {
        return String::from(path);
    }
    let normalized = normalize_absolute_path(path);
    let table = MOUNT_TABLE.lock();
    if let Some(m) = best_match(&table, &normalized) {
        let suffix = mount_suffix(&normalized, &m.target);
        return join_mapped_path(&m.source_path, suffix);
    }
    normalized
}

/// Return true when two namespace paths fall under the same mounted domain.
///
/// This is used to reject cross-mount rename/link in the minimal model.
pub fn same_mount_domain(path_a: &str, path_b: &str) -> bool {
    if !path_a.starts_with('/') || !path_b.starts_with('/') {
        return false;
    }
    let a = normalize_absolute_path(path_a);
    let b = normalize_absolute_path(path_b);
    let table = MOUNT_TABLE.lock();
    let a_target = best_match(&table, &a)
        .map(|m| m.target.as_str())
        .unwrap_or("/");
    let b_target = best_match(&table, &b)
        .map(|m| m.target.as_str())
        .unwrap_or("/");
    a_target == b_target
}
