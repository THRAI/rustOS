//! Page cache with 3-state CAS transitions.
//!
//! Index: (VnodeId, page_offset) -> PageState.
//! States: Absent -> Fetching(WakerList) -> Cached(PhysAddr).
//!
//! When a page is requested:
//! 1. If Cached: return immediately.
//! 2. If Absent: transition to Fetching, initiate I/O, wake all waiters on completion.
//! 3. If Fetching: register waker and wait.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::task::Waker;
use crate::hal_common::{IrqSafeSpinLock, PhysAddr};

use super::vnode::VnodeId;

/// Page cache state for a single page.
pub enum PageState {
    /// I/O in progress; waiters will be woken when data arrives.
    Fetching(Vec<Waker>),
    /// Page is resident in memory at the given physical address.
    Cached(PhysAddr),
}

/// Result of a page cache lookup.
pub enum LookupResult {
    /// Page is cached and ready.
    Hit(PhysAddr),
    /// This caller should initiate the fetch (transitioned Absent -> Fetching).
    InitiateFetch,
    /// Another caller is already fetching; waker registered.
    WaitingOnFetch,
}

/// Global page cache.
static PAGE_CACHE: IrqSafeSpinLock<Option<BTreeMap<(VnodeId, u64), PageState>>> =
    IrqSafeSpinLock::new(None);

/// Initialize the page cache (call once at boot).
pub fn init() {
    *PAGE_CACHE.lock() = Some(BTreeMap::new());
}

/// Look up a page in the cache. Returns the action the caller should take.
///
/// - Hit: page is ready, use the PhysAddr.
/// - InitiateFetch: caller won the race, must fetch the page and call `complete`.
/// - WaitingOnFetch: another task is fetching, waker registered.
pub fn lookup(vnode_id: VnodeId, page_offset: u64, waker: &Waker) -> LookupResult {
    let mut cache = PAGE_CACHE.lock();
    let map = cache.as_mut().expect("page cache not initialized");

    let key = (vnode_id, page_offset);
    match map.get_mut(&key) {
        Some(PageState::Cached(pa)) => LookupResult::Hit(*pa),
        Some(PageState::Fetching(waiters)) => {
            waiters.push(waker.clone());
            LookupResult::WaitingOnFetch
        }
        None => {
            // Transition Absent -> Fetching
            map.insert(key, PageState::Fetching(Vec::new()));
            LookupResult::InitiateFetch
        }
    }
}

/// Complete a page fetch: transition Fetching -> Cached and wake all waiters.
pub fn complete(vnode_id: VnodeId, page_offset: u64, phys: PhysAddr) {
    let mut cache = PAGE_CACHE.lock();
    let map = cache.as_mut().expect("page cache not initialized");

    let key = (vnode_id, page_offset);
    if let Some(old) = map.insert(key, PageState::Cached(phys)) {
        if let PageState::Fetching(waiters) = old {
            for w in waiters {
                w.wake();
            }
        }
    }
}

/// Check if a page is cached (non-blocking, no waker registration).
pub fn probe(vnode_id: VnodeId, page_offset: u64) -> Option<PhysAddr> {
    let cache = PAGE_CACHE.lock();
    let map = cache.as_ref()?;
    match map.get(&(vnode_id, page_offset)) {
        Some(PageState::Cached(pa)) => Some(*pa),
        _ => None,
    }
}

/// Invalidate cached pages in the byte range [offset, offset + len).
/// Fetching pages are left intact to avoid dropping waiter lists.
pub fn invalidate_range(vnode_id: VnodeId, offset: u64, len: usize) {
    if len == 0 {
        return;
    }
    let start_page = offset / 4096;
    let end_page = (offset + len as u64 - 1) / 4096;

    let mut cache = PAGE_CACHE.lock();
    let map = match cache.as_mut() {
        Some(m) => m,
        None => return,
    };

    let mut to_remove = alloc::vec::Vec::new();
    for page in start_page..=end_page {
        let key = (vnode_id, page);
        if matches!(map.get(&key), Some(PageState::Cached(_))) {
            to_remove.push(key);
        }
    }
    for key in to_remove {
        map.remove(&key);
    }
}

/// Invalidate all cached pages for a vnode.
pub fn invalidate_all(vnode_id: VnodeId) {
    let mut cache = PAGE_CACHE.lock();
    let map = match cache.as_mut() {
        Some(m) => m,
        None => return,
    };

    let mut to_remove = alloc::vec::Vec::new();
    for (&(vid, page), state) in map.iter() {
        if vid == vnode_id && matches!(state, PageState::Cached(_)) {
            to_remove.push((vid, page));
        }
    }
    for key in to_remove {
        map.remove(&key);
    }
}
