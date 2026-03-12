//! In-memory symlink table used for compatibility before native ext4 symlink ops.

use alloc::{string::String, vec::Vec};

use crate::hal_common::{Errno, SpinMutex};

const MAX_SYMLINKS: usize = 256;
const MAX_FOLLOW: usize = 8;

struct SymlinkTable {
    links: Vec<(String, String)>, // (link_path -> target_path)
}

impl SymlinkTable {
    const fn new() -> Self {
        Self { links: Vec::new() }
    }
}

static SYMLINK_TABLE: SpinMutex<SymlinkTable> = SpinMutex::new(SymlinkTable::new());

fn normalize_absolute(path: &str) -> String {
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

fn parent_dir(path: &str) -> &str {
    if path == "/" {
        return "/";
    }
    if let Some(pos) = path.rfind('/') {
        if pos == 0 {
            "/"
        } else {
            &path[..pos]
        }
    } else {
        "/"
    }
}

pub fn create(link_path: &str, target_path: &str) -> Result<(), Errno> {
    if link_path.is_empty() || target_path.is_empty() {
        return Err(Errno::Einval);
    }

    let mut table = SYMLINK_TABLE.lock();
    if let Some((_, target)) = table.links.iter_mut().find(|(link, _)| link == link_path) {
        *target = String::from(target_path);
        return Ok(());
    }

    if table.links.len() >= MAX_SYMLINKS {
        return Err(Errno::Enomem);
    }

    table
        .links
        .push((String::from(link_path), String::from(target_path)));
    Ok(())
}

pub fn remove(link_path: &str) -> bool {
    let mut table = SYMLINK_TABLE.lock();
    if let Some(idx) = table.links.iter().position(|(link, _)| link == link_path) {
        table.links.remove(idx);
        true
    } else {
        false
    }
}

pub fn read(link_path: &str) -> Option<String> {
    let table = SYMLINK_TABLE.lock();
    table
        .links
        .iter()
        .find(|(link, _)| link == link_path)
        .map(|(_, target)| target.clone())
}

pub fn resolve(path: &str) -> String {
    if path.is_empty() || !path.starts_with('/') {
        return String::from(path);
    }

    let mut cur = normalize_absolute(path);

    for _ in 0..MAX_FOLLOW {
        let next = read(&cur);
        let Some(target) = next else {
            break;
        };

        if target.starts_with('/') {
            cur = normalize_absolute(&target);
        } else {
            let mut joined = String::from(parent_dir(&cur));
            if joined != "/" {
                joined.push('/');
            }
            joined.push_str(&target);
            cur = normalize_absolute(&joined);
        }
    }

    cur
}
