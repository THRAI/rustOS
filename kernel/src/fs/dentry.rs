//! Global dentry cache: maps (parent VnodeId, name) to child vnode.
//!
//! Simple BTreeMap-based cache for path component lookup with capacity-bounded
//! eviction.  When the cache reaches `MAX_ENTRIES`, a batch of the oldest
//! entries (by insertion order) is evicted to make room.
//!
//! Insertion order is tracked via monotonically increasing sequence numbers
//! stored alongside each entry.  Eviction removes the entries with the
//! smallest sequence numbers — a FIFO approximation of LRU that avoids the
//! overhead of a doubly-linked list in `no_std`.

use alloc::{collections::BTreeMap, string::String, sync::Arc, vec::Vec};

use crate::{
    fs::{Vnode, VnodeId},
    hal_common::IrqSafeSpinLock,
};

/// Dentry cache key: (parent vnode id, child name).
type DentryKey = (VnodeId, String);

/// Maximum number of entries before eviction kicks in.
const MAX_ENTRIES: usize = 2048;

/// Number of entries to evict in one batch when the cache is full.
/// Evicting in batches amortises the O(n log n) sort cost.
const EVICT_BATCH: usize = 256;

/// Cached dentry entry with insertion-order sequence number.
struct DentryEntry {
    vnode: Arc<dyn Vnode>,
    seq: u64,
}

/// Inner state of the dentry cache.
struct DentryInner {
    map: BTreeMap<DentryKey, DentryEntry>,
    next_seq: u64,
}

impl DentryInner {
    const fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            next_seq: 0,
        }
    }
}

/// Global dentry cache.
static DENTRY_CACHE: IrqSafeSpinLock<Option<DentryInner>, 5> = IrqSafeSpinLock::new(None);

/// Initialize the dentry cache (call once at boot).
pub fn init() {
    *DENTRY_CACHE.lock() = Some(DentryInner::new());
}

/// Look up a child vnode by parent ID and name.
pub fn lookup(parent_id: VnodeId, name: &str) -> Option<Arc<dyn Vnode>> {
    let cache = DENTRY_CACHE.lock();
    let inner = cache.as_ref()?;
    inner
        .map
        .get(&(parent_id, String::from(name)))
        .map(|e| Arc::clone(&e.vnode))
}

/// Insert a dentry into the cache, evicting old entries if at capacity.
pub fn insert(parent_id: VnodeId, name: &str, vnode: Arc<dyn Vnode>) {
    let mut cache = DENTRY_CACHE.lock();
    if let Some(inner) = cache.as_mut() {
        // Evict oldest entries when at capacity.
        if inner.map.len() >= MAX_ENTRIES {
            evict_oldest(inner, EVICT_BATCH);
        }

        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner
            .map
            .insert((parent_id, String::from(name)), DentryEntry { vnode, seq });
    }
}

/// Remove a dentry from the cache (used after unlink/rmdir).
pub fn invalidate(parent_id: VnodeId, name: &str) {
    let mut cache = DENTRY_CACHE.lock();
    if let Some(inner) = cache.as_mut() {
        inner.map.remove(&(parent_id, String::from(name)));
    }
}

/// Evict up to `count` entries with the smallest sequence numbers (oldest).
fn evict_oldest(inner: &mut DentryInner, count: usize) {
    // Collect (seq, key) pairs and sort by seq to find oldest.
    let mut entries: Vec<(u64, DentryKey)> =
        inner.map.iter().map(|(k, e)| (e.seq, k.clone())).collect();
    entries.sort_unstable_by_key(|(seq, _)| *seq);

    let to_remove = count.min(entries.len());
    for (_, key) in entries.into_iter().take(to_remove) {
        inner.map.remove(&key);
    }
}
