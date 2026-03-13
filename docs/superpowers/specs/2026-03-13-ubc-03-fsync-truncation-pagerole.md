# UBC Part 3: fsync, Truncation Coherence, PageRole Fix

**Parent:** `ubc-00-overview.md`

## 1. fsync Unification

### Current State (`syscall/fs.rs:838-849`)

```rust
pub async fn sys_fsync_async(task: &Arc<Task>, fd: u32) -> Result<(), Errno> {
    let path = { /* extract path from fd */ };
    crate::fs::fs_cache_flush(&path).await  // flushes lwext4 block cache ONLY
}
```

This is wrong under UBC: dirty VmObject pages are never flushed to lwext4 by fsync.

### New fsync

Two-phase flush: VmObject dirty pages → lwext4 → then lwext4 block cache → disk.

```rust
pub async fn sys_fsync_async(task: &Arc<Task>, fd: u32) -> Result<(), Errno> {
    let (path, vnode_id) = {
        let tab = task.fd_table.lock();
        let d = tab.get(fd).ok_or(Errno::Ebadf)?;
        match &d.object {
            FileObject::Vnode(v) => (String::from(v.path()), v.vnode_id()),
            _ => return Ok(()),
        }
    };

    // Phase 1: Flush dirty VmObject pages through pager → delegate → lwext4
    if let Some(obj_arc) = vnode_object_if_exists(vnode_id) {
        let (dirty_pages, pager) = {
            let obj = obj_arc.read();
            (obj.collect_dirty_pages(), obj.pager.as_ref().map(Arc::clone))
        };
        // VmObject lock released before I/O

        if let Some(pager) = pager {
            for (idx, pa) in &dirty_pages {
                pager.page_out(idx.to_bytes(), *pa)
                    .await
                    .map_err(|_| Errno::Eio)?;
            }
        }

        // Clear dirty state
        {
            let obj = obj_arc.read();
            for (idx, _) in &dirty_pages {
                if let Some(page) = obj.get_page(*idx) {
                    page.clear_dirty_all();
                }
            }
            obj.mark_clean();
        }
    }

    // Phase 2: Flush lwext4's internal block cache to VirtIO
    crate::fs::fs_cache_flush(&path).await
}
```

### fdatasync

Remains a thin wrapper: `sys_fdatasync_async` delegates to `sys_fsync_async`. Under UBC this is still correct — we don't distinguish data-only vs full sync (no separate metadata tracking).

### sys_sync

`sys_sync_async` should flush ALL dirty VmObjects, then call `fs_cache_flush("/")`:

```rust
pub async fn sys_sync_async() -> Result<(), Errno> {
    // Flush all dirty vnodes (reuse page daemon logic)
    crate::mm::vm::page_daemon::flush_all_dirty().await;
    // Then flush lwext4 block cache
    crate::fs::fs_cache_flush("/").await
}
```

`flush_all_dirty()` is a public async function in `page_daemon.rs` that runs one full dirty-scan cycle synchronously (same logic as the daemon loop body).

## 2. Truncation Coherence

Two truncation paths exist. Both already interact with VmObject but need adjustment for dirty page safety.

### ftruncate (`syscall/fs.rs:815-835`)

**Current:**
1. `fs_truncate(&path, len)` → delegate → lwext4
2. `vnode.set_size(len)` → truncates VmObject pages if shrinking

**Problem:** If dirty pages exist in the truncated range, their data is lost silently — never written back.

**Fix:** Before truncating, flush dirty pages in the truncated range:

```rust
pub async fn sys_ftruncate_async(task: &Arc<Task>, fd: u32, len: u64) -> Result<(), Errno> {
    let (path, vnode_id, vnode) = { /* extract from fd */ };

    // Flush dirty pages in the range being truncated
    if let Some(obj_arc) = vnode_object_if_exists(vnode_id) {
        let (dirty_in_range, pager) = {
            let obj = obj_arc.read();
            let trunc_idx = VObjIndex::from_bytes_ceil(len as usize);
            let dirty: Vec<_> = obj.pages_with_index()
                .filter(|(idx, page)| *idx >= trunc_idx && page.is_dirty())
                .map(|(idx, page)| (*idx, page.phys_addr()))
                .collect();
            (dirty, obj.pager.as_ref().map(Arc::clone))
        };
        // Lock released

        if let Some(pager) = pager {
            for (idx, pa) in &dirty_in_range {
                let _ = pager.page_out(idx.to_bytes(), *pa).await;
            }
        }
    }

    // Then truncate on disk and in VmObject (existing logic)
    crate::fs::fs_truncate(&path, len).await?;
    vnode.set_size(len);  // This calls truncate_pages() internally
    // ... adjust offset if needed
    Ok(())
}
```

### O_TRUNC on open (`syscall/fs.rs:157-164`)

**Current:**
1. `fs_truncate(path, 0)` → delegate → lwext4
2. `vnode_destroy_object(vnode_id)` — destroys entire VmObject
3. `vnode.set_size(0)`

**Fix:** Flush all dirty pages for this vnode before destroying the object:

```rust
if trunc && flags.write && vnode.vtype() == VnodeType::Regular {
    // Flush dirty pages before destroying
    if let Some(obj_arc) = vnode_object_if_exists(vnode.vnode_id()) {
        let (dirty, pager) = {
            let obj = obj_arc.read();
            (obj.collect_dirty_pages(), obj.pager.as_ref().map(Arc::clone))
        };
        if let Some(pager) = pager {
            for (idx, pa) in &dirty {
                let _ = pager.page_out(idx.to_bytes(), *pa).await;
            }
        }
    }

    crate::fs::fs_truncate(path_str, 0).await.map_err(|_| Errno::Eio)?;
    vnode_destroy_object(vnode.vnode_id());
    vnode.set_size(0);
}
```

Note: One could argue the dirty pages don't matter since the file is being truncated to zero anyway. The flush is for correctness — if a concurrent reader is in the middle of reading those pages, destroying the VmObject while the reader holds page references is safe (Arc protects the page), but we want the on-disk state to be consistent up to the truncation point.

## 3. PageRole Fix

### Problem

File-backed pages allocated during demand faults use `PageRole::UserAnon`:

```rust
// fault_async.rs:181
let frame = crate::mm::alloc_raw_frame_sync(PageRole::UserAnon)
    .ok_or(FaultError::OutOfMemory)?;
```

This makes file cache pages indistinguishable from anonymous pages — blocking any future page reclaim policy that treats them differently (file cache pages can be evicted and re-read; anonymous pages cannot).

### Fix

In `fault_async.rs`, determine the correct role from the pager:

```rust
// Determine page role from pager type
let role = match pager.as_ref() {
    Some(p) if !p.is_anon() => PageRole::FileCache,
    _ => PageRole::UserAnon,
};
let frame = crate::mm::alloc_raw_frame_sync(role)
    .ok_or(FaultError::OutOfMemory)?;
```

The `Pager::is_anon()` method already exists (`mm/vm/object.rs:37`) — `AnonPager` returns `true`, `VnodePager` returns `false`. This is the exact discrimination we need.

### Where the same fix applies

The `fetch_page_async` path (`object.rs:469-506`) also allocates pages. If it calls `alloc_raw_frame_sync` internally, the same `is_anon()` check should be applied there. Verify during implementation.

## Files Changed

| File | Lines | Change |
|------|-------|--------|
| `kernel/src/syscall/fs.rs` | 838-854 | Rewrite fsync — two-phase flush |
| `kernel/src/syscall/fs.rs` | 857-859 | Update sys_sync to call `flush_all_dirty()` |
| `kernel/src/syscall/fs.rs` | 815-835 | Modify ftruncate — flush dirty pages in truncated range |
| `kernel/src/syscall/fs.rs` | 157-164 | Modify O_TRUNC — flush before destroy |
| `kernel/src/mm/vm/fault_async.rs` | 181 | Use `PageRole::FileCache` for file-backed pages |
| `kernel/src/mm/vm/page_daemon.rs` | — | Add `pub async fn flush_all_dirty()` |
