//! ELF exec — async IO shell with three-stage commit semantics.
//!
//! This module is the **only** place where IO happens during exec:
//!
//!   - Stage 1: vnode resolution + page-0 fetch + `elf::parse_elf`
//!   - Stage 2: pure `ExecPipeline` chain (delegated to `exec_ctx.rs`)
//!             + one IO call to `materialize_stack`
//!   - Stage 3: atomic commit (pmap swap, signal reset, fd cleanup)
//!
//! The design rule is simple: if it touches a pager, a pmap, or a lock
//! on the running task, it lives here.  If it's a pure transform over
//! `VmMap` state, it lives in `exec_ctx.rs`.
//!
//! # Monadic pipeline
//!
//! Stage 2 reads as a single `and_then` chain:
//!
//! ```ignore
//! ExecPipeline::new(pmap)
//!     .and_then(|p| p.with_segments(&main_elf, ...))?
//!     .and_then(|p| match interp_elf { ... })?
//!     .and_then(|p| p.with_stack())?
//!     .finalize_auxv()
//!     .with_sigcode()
//! ```
//!
//! Every step is `Self -> Result<Self, Errno>`.  If any step fails the
//! partial `VmMap` is dropped and the process's own address space is
//! untouched.

mod elf;
mod exec_ctx;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

use self::{
    elf::{ElfError, ElfParseResult},
    exec_ctx::{AuxvEntry, ExecPipeline, StackLayout},
};
use crate::{
    fs::{resolve, VnodeType},
    hal_common::{Errno, PageCursor, SpinMutex, VirtAddr, PAGE_SIZE},
    mm::{
        pmap_activate, pmap_create, pmap_destroy, pmap_enter,
        vm::{MapPerm, PageFaultAccessType, VObjIndex},
        VmMap, VmObject,
    },
    proc::{SigSet, Task, SIG_DFL, SIG_IGN},
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Dynamic linker load offset.
const DL_INTERP_OFFSET: usize = 0x20_0000_0000;

/// User stack geometry (must match exec_ctx.rs — consider a shared constants
/// module if this grows).
const USER_STACK_SIZE: usize = 64 * 1024;
const USER_STACK_TOP: usize = 0x0000_003F_FFFF_F000;

/// AT_RANDOM auxv tag.
const AT_RANDOM: usize = 25;

// ---------------------------------------------------------------------------
// Error mapping: ElfError -> Errno
// ---------------------------------------------------------------------------

/// Map a parse-layer `ElfError` into a kernel `Errno`.
///
/// This is the only bridge between the pure parse world and the kernel
/// error world.  Log context is added here so `elf.rs` stays log-free.
fn elf_err_to_errno(e: ElfError) -> Errno {
    match e {
        ElfError::ParseFailed(msg) => {
            kerr!(
                exec,
                warn,
                Errno::Enoexec,
                "exec: ELF parse failed: {}",
                msg
            )
        },
        ElfError::UnsupportedType(t) => {
            kerr!(exec, warn, Errno::Enoexec, "exec: unsupported e_type={}", t)
        },
        ElfError::UnsupportedMachine(m) => {
            kerr!(
                exec,
                warn,
                Errno::Enoexec,
                "exec: unsupported e_machine={}",
                m
            )
        },
        ElfError::PhdrOverflow {
            phdr_end,
            buffer_len,
        } => {
            kerr!(
                exec,
                warn,
                Errno::Enoexec,
                "exec: phdrs overflow buffer end={} buf={}",
                phdr_end,
                buffer_len
            )
        },
    }
}

// ---------------------------------------------------------------------------
// Stage 1: IO — fetch page 0 and parse
// ---------------------------------------------------------------------------

/// Fetch page 0 of a vnode, returning the raw bytes as a slice.
///
/// The returned `Arc<VmObject>` keeps the page alive — the slice borrows
/// from its identity-mapped physical page.  Callers inspect the bytes
/// (shebang check, ELF parse) before proceeding to the pipeline.
async fn fetch_page0(
    vnode: &dyn crate::fs::Vnode,
) -> Result<
    (
        Arc<crate::hal_common::LeveledRwLock<crate::mm::VmObject, 3>>,
        crate::hal_common::PhysAddr,
    ),
    Errno,
> {
    let obj = VmObject::new_file(vnode);

    VmObject::fetch_page_async(Arc::clone(&obj), VObjIndex::new(0))
        .await
        .map_err(|_| kerr!(exec, warn, Errno::Enoexec, "exec: page 0 fetch failed"))?;

    let phys = obj
        .read()
        .lookup_page(VObjIndex::new(0))
        .ok_or_else(|| kerr!(exec, warn, Errno::Enoexec, "exec: page 0 lookup failed"))?;

    Ok((obj, phys))
}

/// Resolve a path and fetch+parse its ELF, returning the parse result
/// alongside the vnode metadata the pipeline needs.
async fn resolve_and_parse(path: &str) -> Result<(ElfParseResult, usize, String), Errno> {
    let vnode = resolve(path).await?;
    if vnode.vtype() != VnodeType::Regular {
        return Err(kerr!(
            exec,
            warn,
            Errno::Enoexec,
            "exec: not a regular file path={}",
            path
        ));
    }
    let id = vnode.vnode_id() as usize;
    let vpath = String::from(vnode.path());
    let (_obj, phys) = fetch_page0(&*vnode).await?;
    let parse = elf::parse_elf(phys.as_slice()).map_err(elf_err_to_errno)?;
    Ok((parse, id, vpath))
}

// ---------------------------------------------------------------------------
// Shebang (#!) support
// ---------------------------------------------------------------------------

/// Parse a `#!` shebang line from the first bytes of a file.
///
/// Returns `(interpreter_path, optional_arg)`.
/// Pure — no IO, no kernel types, no logging.
fn parse_shebang(bytes: &[u8]) -> Option<(String, Option<String>)> {
    if bytes.len() < 4 || bytes[0] != b'#' || bytes[1] != b'!' {
        return None;
    }
    // Find end of first line (max 254 chars after "#!", matching Linux BINPRM_BUF_SIZE)
    let end = bytes[2..]
        .iter()
        .take(254)
        .position(|&b| b == b'\n' || b == b'\0')
        .unwrap_or(254.min(bytes.len() - 2));
    let line = core::str::from_utf8(&bytes[2..2 + end]).ok()?;
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let mut parts = line.splitn(2, |c: char| c.is_ascii_whitespace());
    let interp = String::from(parts.next()?);
    let arg = parts
        .next()
        .map(|s| String::from(s.trim()))
        .filter(|s| !s.is_empty());
    Some((interp, arg))
}

/// If `page0` starts with `#!`, parse the interpreter and re-exec.
///
/// Returns `Some(result)` if this was a shebang script (success or error).
/// Returns `None` if not a shebang — caller falls through to ELF path.
///
/// Depth-limited to 4 to prevent `#!` loops.
async fn try_shebang(
    task: &Arc<Task>,
    script_path: &str,
    page0: &[u8],
    argv: &[String],
    envp: &[String],
    depth: u32,
) -> Option<Result<(usize, usize), Errno>> {
    const MAX_SHEBANG_DEPTH: u32 = 4;

    let (interp, arg) = parse_shebang(page0)?;

    if depth >= MAX_SHEBANG_DEPTH {
        return Some(Err(kerr!(
            exec,
            warn,
            Errno::Enoexec,
            "exec: shebang recursion limit path={}",
            script_path
        )));
    }

    klog!(
        exec,
        debug,
        "exec: shebang pid={} script={} interp={} arg={:?}",
        task.pid,
        script_path,
        interp,
        arg
    );

    // Build new argv: [interp, arg?, script_path, original_argv[1..]]
    let mut new_argv = Vec::new();
    new_argv.push(interp.clone());
    if let Some(a) = arg {
        new_argv.push(a);
    }
    new_argv.push(String::from(script_path));
    if argv.len() > 1 {
        new_argv.extend_from_slice(&argv[1..]);
    }

    Some(Box::pin(do_execve_inner(task, &interp, &new_argv, envp, depth + 1)).await)
}

// ---------------------------------------------------------------------------
// do_execve — top-level entry point
// ---------------------------------------------------------------------------

/// Unified exec entry point with three-stage commit semantics.
///
/// A failed exec at any stage before `commit` leaves the calling
/// process's address space completely intact.
///
/// Returns `(entry_point, stack_pointer)` on success.
pub async fn do_execve(
    task: &Arc<Task>,
    elf_path: &str,
    argv: &[String],
    envp: &[String],
) -> Result<(usize, usize), Errno> {
    do_execve_inner(task, elf_path, argv, envp, 0).await
}

/// Eagerly resolve the current image's user pages into the task pmap.
///
/// LA64 full-system bring-up still lacks a complete TLB-refill/demand-fault
/// story for the very first user instructions, so callers can prefault the
/// freshly exec'd image before the first `trap_return`.
pub async fn prefault_user_image(task: &Arc<Task>) -> Result<(), Errno> {
    let pages: Vec<(VirtAddr, PageFaultAccessType)> = {
        let vm = task.vm_map.read();
        let mut pages = Vec::new();

        for vma in vm.iter() {
            let access = if vma.protection.contains(MapPerm::X) {
                PageFaultAccessType::EXECUTE
            } else if vma.protection.contains(MapPerm::W) {
                PageFaultAccessType::WRITE
            } else {
                PageFaultAccessType::READ
            };

            let mut va = vma.start().as_usize();
            while va < vma.end().as_usize() {
                pages.push((VirtAddr::new(va), access));
                va += PAGE_SIZE;
            }
        }

        pages
    };

    for (va, access) in pages {
        crate::mm::resolve_user_fault(task, va, access).await?;
    }

    Ok(())
}

/// Inner exec with shebang depth tracking.
///
/// Separated from `do_execve` so `try_shebang` can recurse with an
/// incremented depth counter without changing the public API.
async fn do_execve_inner(
    task: &Arc<Task>,
    elf_path: &str,
    argv: &[String],
    envp: &[String],
    depth: u32,
) -> Result<(usize, usize), Errno> {
    klog!(
        exec,
        debug,
        "do_execve pid={} path={} depth={}",
        task.pid,
        elf_path,
        depth
    );

    // =======================================================================
    // STAGE 1: Fetch page 0, check for shebang, then parse ELF.
    // =======================================================================

    let vnode = resolve(elf_path).await?;
    if vnode.vtype() != VnodeType::Regular {
        return Err(kerr!(
            exec,
            warn,
            Errno::Enoexec,
            "exec: not a regular file path={}",
            elf_path
        ));
    }
    let main_vnode_id = vnode.vnode_id() as usize;
    let main_vnode_path = String::from(vnode.path());
    let (_obj, phys) = fetch_page0(&*vnode).await?;
    let page0 = phys.as_slice();

    // Shebang check — if "#!", parse interpreter and recurse.
    if let Some(result) = try_shebang(task, elf_path, page0, argv, envp, depth).await {
        return result;
    }

    // Normal ELF path.
    let main_elf = elf::parse_elf(page0).map_err(elf_err_to_errno)?;

    let interp_elf: Option<(ElfParseResult, usize, String)> = match main_elf.interp {
        Some(ref ipath) => {
            klog!(
                exec,
                debug,
                "do_execve pid={} loading interp={}",
                task.pid,
                ipath
            );
            Some(resolve_and_parse(ipath).await?)
        },
        None => None,
    };

    // =======================================================================
    // STAGE 2: Pure pipeline — no awaits, no IO (except stack materialize).
    //
    // Every step is ExecPipeline -> Result<ExecPipeline, Errno>.
    // =======================================================================

    let new_pmap = Arc::new(SpinMutex::new(pmap_create()));

    let pipeline = ExecPipeline::new(Arc::clone(&new_pmap))
        // 2a. Map main binary segments.
        .and_then(|p| {
            p.with_segments(&main_elf, main_vnode_id, &main_vnode_path, 0)
        })?
        // 2b. Map interpreter segments (if present), overriding entry.
        .and_then(|p| match interp_elf {
            Some((ref iparse, vid, ref vpath)) => {
                p.with_interp_segments(iparse, vid, vpath, DL_INTERP_OFFSET)
            }
            None => Ok(p),
        })?
        // 2c. Attach anonymous stack VMA.
        .and_then(|p| p.with_stack())?
        // 2d. Seal auxv (adds AT_RANDOM placeholder + AT_NULL).
        .finalize_auxv()
        // 2e. Attach sigcode trampoline.
        .with_sigcode();

    // 2f. Materialize the stack (single-page IO: alloc + pmap_enter + write).
    let layout = StackLayout::build(argv, envp, &pipeline.auxv);
    let sp = materialize_stack(&pipeline, &layout, argv, envp, &new_pmap)?;

    let entry = pipeline.entry;

    // =======================================================================
    // STAGE 3: Point of no return — atomic swap.
    // =======================================================================

    commit(task, pipeline);

    klog!(
        exec,
        debug,
        "do_execve pid={} entry={:#x} sp={:#x}",
        task.pid,
        entry,
        sp
    );
    Ok((entry, sp))
}

// ---------------------------------------------------------------------------
// Stage 2 IO: materialize_stack
// ---------------------------------------------------------------------------

/// Materialize a `StackLayout` onto the top physical stack page.
///
/// This is the only non-pure operation in stage 2: one anonymous page is
/// allocated, wired into the pmap, and written with AT_RANDOM bytes,
/// NUL-terminated strings, and the pointer table.
///
/// Returns the final stack pointer (virtual, 16-byte aligned).
fn materialize_stack(
    pipeline: &ExecPipeline,
    layout: &StackLayout,
    argv: &[String],
    envp: &[String],
    pmap_arc: &Arc<SpinMutex<crate::mm::Pmap, 2>>,
) -> Result<usize, Errno> {
    let stack_obj = pipeline
        .stack_obj
        .as_ref()
        .expect("with_stack must be called before materialize_stack");

    let stack_page_va = USER_STACK_TOP - PAGE_SIZE;
    let stack_page_idx = VObjIndex::new((USER_STACK_SIZE / PAGE_SIZE) - 1);

    // Allocate the top physical stack page through the VmObject.
    let phys = {
        let mut obj = stack_obj.write();
        obj.fault_allocate_anon(stack_page_idx)
            .map_err(|_| kerr!(exec, error, Errno::Enomem, "exec: stack page alloc failed"))?
    };

    // Wire the page into the new pmap (stack top is always needed immediately).
    {
        let mut pmap = pmap_arc.lock();
        let _ = pmap_enter(
            &mut pmap,
            VirtAddr::new(stack_page_va),
            phys,
            crate::map_perm!(R, W, U),
            false,
        );
    }

    let mut cursor = PageCursor::new(phys, PAGE_SIZE).unwrap();
    let stack_page_vbase = VirtAddr::new(stack_page_va);

    // 1. Push AT_RANDOM bytes (16 zeroed bytes; TODO: fill from CSPRNG).
    let random_va = {
        let slice = cursor.alloc_down_bytes(16).unwrap();
        slice.fill(0);
        cursor.current_va(stack_page_vbase)
    };

    // 2. Push argv strings (high → low), record their virtual addresses.
    let argv_vas: Vec<usize> = argv
        .iter()
        .map(|arg| {
            let s = arg.as_bytes();
            let buf = cursor.alloc_down_bytes(s.len() + 1).unwrap();
            buf[..s.len()].copy_from_slice(s);
            buf[s.len()] = 0;
            cursor.current_va(stack_page_vbase).as_usize()
        })
        .collect();

    // 3. Push envp strings, record their virtual addresses.
    let envp_vas: Vec<usize> = envp
        .iter()
        .map(|env| {
            let s = env.as_bytes();
            let buf = cursor.alloc_down_bytes(s.len() + 1).unwrap();
            buf[..s.len()].copy_from_slice(s);
            buf[s.len()] = 0;
            cursor.current_va(stack_page_vbase).as_usize()
        })
        .collect();

    // 4. Resolve AT_RANDOM to its actual VA.
    let auxv: Vec<(usize, usize)> = layout
        .auxv
        .iter()
        .map(|e| {
            if e.atype == AT_RANDOM {
                (AT_RANDOM, random_va.as_usize())
            } else {
                (e.atype, e.value)
            }
        })
        .collect();

    // 5. Align to 16 bytes, then write the pointer table downward.
    cursor.align_down(16);
    let prev_offset = cursor.current_offset();
    let slots_bytes = layout.table_bytes;

    if prev_offset < slots_bytes {
        return Err(kerr!(
            exec,
            error,
            Errno::Enomem,
            "exec: not enough stack for argv/envp needed={} avail={}",
            slots_bytes,
            prev_offset
        ));
    }

    let sp = stack_page_vbase.as_usize() + prev_offset - slots_bytes;

    let allocated = cursor.alloc_down_bytes(slots_bytes).ok_or_else(|| {
        kerr!(
            exec,
            error,
            Errno::Enomem,
            "exec: pointer-table alloc failed size={}",
            slots_bytes
        )
    })?;

    // SAFETY: `allocated` is inside the identity-mapped physical page;
    // `slots_bytes` was verified above.
    let table = unsafe { core::slice::from_raw_parts_mut(allocated.as_mut_ptr(), slots_bytes) };

    let mut off = 0usize;
    let mut push = |val: usize| {
        const W: usize = core::mem::size_of::<usize>();
        table[off..off + W].copy_from_slice(&val.to_ne_bytes());
        off += W;
    };

    push(argv.len());
    for &va in &argv_vas {
        push(va);
    }
    push(0); // argv NULL
    for &va in &envp_vas {
        push(va);
    }
    push(0); // envp NULL
    for &(tag, val) in &auxv {
        push(tag);
        push(val);
    }

    Ok(sp)
}

// ---------------------------------------------------------------------------
// Stage 3: commit — point of no return
// ---------------------------------------------------------------------------

/// Atomically swap the address space and clean up exec-time state.
///
/// After this returns: old pmap destroyed, satp switched, signal
/// dispositions reset, O_CLOEXEC fds closed, vfork parent woken.
///
/// Nothing here can fail — all fallible work belongs in stage 2.
fn commit(task: &Arc<Task>, pipeline: ExecPipeline) {
    let brk = pipeline.brk;

    // Swap VmMap and activate new pmap under the vm_map write lock.
    {
        let mut vm = task.vm_map.write();
        let old_vm = core::mem::replace(&mut *vm, pipeline.vm);

        let mut new_pmap = vm.pmap_lock();
        pmap_activate(&mut new_pmap);
        drop(new_pmap);

        // satp now points at the new root — tear down the old one.
        let mut old_pmap = old_vm.pmap_lock();
        pmap_destroy(&mut old_pmap);
    }

    // Update program break.
    task.brk.store(brk.0, core::sync::atomic::Ordering::Relaxed);

    // Reset caught signal dispositions to SIG_DFL (POSIX exec semantics).
    {
        let mut actions = task.signals.actions.lock();
        for act in actions.iter_mut() {
            if act.handler != SIG_DFL && act.handler != SIG_IGN {
                act.handler = SIG_DFL;
                act.flags = 0;
                act.mask = 0;
            }
        }
    }
    task.signals
        .pending
        .store(SigSet(0), core::sync::atomic::Ordering::Relaxed);
    task.signals
        .blocked
        .store(SigSet(0), core::sync::atomic::Ordering::Relaxed);

    // Close O_CLOEXEC descriptors.
    task.fd_table.lock().strip_cloexec();

    // Wake vfork parent.
    if let Some(ref vfork) = task.vfork_done {
        vfork.signal();
    }
}
