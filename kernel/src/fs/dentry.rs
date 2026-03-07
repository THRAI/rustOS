//! Global dentry cache: maps (parent VnodeId, name) to child vnode.
//!
//! Simple HashMap-based cache for path component lookup.
//! Eviction is not implemented (sufficient for competition workloads).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use crate::hal_common::IrqSafeSpinLock;

use super::vnode::{Vnode, VnodeId};

/// Dentry cache key: (parent vnode id, child name).
type DentryKey = (VnodeId, String);

/// Global dentry cache.
static DENTRY_CACHE: IrqSafeSpinLock<Option<BTreeMap<DentryKey, Arc<dyn Vnode>>>> =
    IrqSafeSpinLock::new(None);

/// Initialize the dentry cache (call once at boot).
pub fn init() {
    *DENTRY_CACHE.lock() = Some(BTreeMap::new());
}

/// Look up a child vnode by parent ID and name.
pub fn lookup(parent_id: VnodeId, name: &str) -> Option<Arc<dyn Vnode>> {
    let cache = DENTRY_CACHE.lock();
    let map = cache.as_ref()?;
    map.get(&(parent_id, String::from(name))).cloned()
}

/// Insert a dentry into the cache.
pub fn insert(parent_id: VnodeId, name: &str, vnode: Arc<dyn Vnode>) {
    let mut cache = DENTRY_CACHE.lock();
    if let Some(map) = cache.as_mut() {
        map.insert((parent_id, String::from(name)), vnode);
    }
}

/// Remove a dentry from the cache (used after unlink/rmdir).
pub fn invalidate(parent_id: VnodeId, name: &str) {
    let mut cache = DENTRY_CACHE.lock();
    if let Some(map) = cache.as_mut() {
        map.remove(&(parent_id, String::from(name)));
    }
}
