# UBC Part 1: Unified Write Path

**Parent:** `ubc-00-overview.md`

## Current Write Path (to be replaced)

`syscall/fs.rs:1253-1314` — the vnode branch of `sys_write_async`:

```
1. fault_in_user_buffer()               — ensure user pages are mapped
2. kbuf = alloc::vec![0u8; len]         — HEAP ALLOCATION
3. copy_user_chunk(kbuf, user_buf, len) — FULL COPY: user → kernel heap
4. fs_write_at(&path, offset, &kbuf)    — delegate → lwext4 → disk
5. IF vnode_object_if_exists(vnode_id):  — retroactive patch
     copy kbuf → VmObject pages
     page.set_dirty()
6. vnode.set_size(max(old, offset+n))
```

**Problems:**
- Write goes to disk first, VmObject is patched after — incoherent window
- Heap allocation per write — unnecessary pressure on kernel allocator
- 2-3 copies (user → heap → lwext4, optionally → VmObject pages)
- VmObject pages not created if they don't already exist — inconsistent cache population

## New Write Path

Replace the vnode branch of `sys_write_async` (`syscall/fs.rs:1253-1314`):

```rust
// --- New vnode write path ---
WriteTarget::Vnode { vnode, size, mut offset } => {
    // O_APPEND: use vnode's authoritative size
    if is_append {
        offset = vnode.size();
    }

    let obj = vnode_object(&*vnode);  // get-or-create VmObject
    let to_write = len;
    let mut total = 0usize;

    while total < to_write {
        let page_idx = VObjIndex::from_bytes_floor((offset as usize) + total);
        let offset_in_page = ((offset as usize) + total) % PAGE_SIZE;
        let chunk = core::cmp::min(PAGE_SIZE - offset_in_page, to_write - total);

        // 1. Fetch page into VmObject (page_in from disk if not cached)
        let page_info = VmObject::fetch_page_async(
            Arc::clone(&obj), page_idx
        ).await.map_err(|_| Errno::Eio)?;

        // 2. Copy user data INTO the VmObject page
        let kern = (page_info.phys_addr.as_usize() + offset_in_page) as *mut u8;
        let user = (user_buf + total) as *mut u8;

        match uiomove(kern, user, chunk, UioDir::CopyIn) {
            Ok(result) => {
                total += result.done;
            }
            Err(Errno::Efault) => {
                // Demand-page user buffer, retry
                resolve_user_fault(
                    task,
                    VirtAddr::new(user_buf + total),
                    PageFaultAccessType::READ,
                ).await.map_err(|_| Errno::Efault)?;
                continue; // retry this chunk
            }
            Err(e) => {
                if total > 0 { break; } // short write
                return Err(e);
            }
        }

        // 3. Mark page dirty
        page_info.set_dirty();
    }

    // 4. Bump object dirty generation (under read lock is fine —
    //    generation is AtomicU32, no write lock needed)
    obj.read().bump_generation();

    // 5. Maybe wake page daemon
    maybe_wake_page_daemon();

    // 6. Extend file size if needed
    let new_end = offset + total as u64;
    if new_end > vnode.size() {
        vnode.set_size(new_end);
        obj.write().set_size(new_end as usize);
    }

    // 7. Update file offset
    update_offset(&desc, offset + total as u64);

    Ok(total)
}
```

## Key Design Decisions

### No immediate disk I/O on write

Data stays in dirty VmObject pages until the page daemon flushes them or `fsync()` is called. This is standard write-back semantics. `fsync()` provides the durability guarantee — see Part 3.

### `fetch_page_async` for partial-page overwrites

Even for writes that don't cover a full page, we must fetch the existing page first (read-modify-write at page granularity). This ensures the non-written portion of the page retains correct data. For writes to pages beyond EOF, `VnodePager::page_in` zero-fills automatically.

### File extension

When a write extends past EOF:
1. `vnode.set_size()` updates the atomic file size on the vnode
2. `obj.write().set_size()` extends the VmObject's logical size
3. Newly allocated pages beyond old EOF are zero-filled by the pager
4. The on-disk size update happens at writeback time (page daemon calls `page_out` which uses `fs_write_at`, and the delegate does `ext4_write` which extends the file)

### O_APPEND

Currently queries file size via `fs_lookup()` through the delegate — a round-trip. Under UBC, `vnode.size()` is the authoritative size (it's an `AtomicU64` on `Ext4Vnode`), so O_APPEND just reads the local atomic. No delegate call needed.

### `sys_writev_async` inherits the fix

`sys_writev_async` (`syscall/fs.rs:1370-1418`) delegates to `sys_write_async` per iovec. No changes needed — it inherits the unified write path automatically.

## Copy Count Comparison

| Path | Before | After |
|------|--------|-------|
| write() to uncached file | 2 copies (user→heap, heap→lwext4) | 1 copy (user→VmObject page) |
| write() to mmap'd file | 3 copies (user→heap, heap→lwext4, heap→VmObject) | 1 copy (user→VmObject page) |
| write() then read() same offset | 2+2 copies | 1 copy write + 1 copy read (page cache hit) |

## Locking

The write path acquires:
1. `fd_table` lock (level 4) — briefly, to extract vnode + offset
2. VmObject read lock (level 3) — during `fetch_page_async`. Released before any delegate call.
3. VmObject write lock (level 3) — only if extending size via `set_size()`

`fetch_page_async` internally may call `VnodePager::page_in` which sends a request through the delegate (level 6). The VmObject lock is **not held** during the delegate await — `fetch_page_async` acquires and releases the lock around page lookup/insertion, not around I/O. This is safe.

## Files Changed

| File | Lines | Change |
|------|-------|--------|
| `kernel/src/syscall/fs.rs` | 1253-1314 | Replace vnode write path (~80 lines replaced with ~60) |
| `kernel/src/syscall/fs.rs` | 1259-1263 | Remove O_APPEND `fs_lookup` round-trip |
