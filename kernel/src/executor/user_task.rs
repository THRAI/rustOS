//! User task future: persistent per-process future + async trap handler.
//!
//! UserTaskFuture wraps run_tasks and switches address space on poll.
//! run_tasks is the trap loop: trap_return → user runs → trap back → dispatch.
//! spawn_user_task creates and detaches the future on a target CPU.

use alloc::string::String;
use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use hal_common::{Errno, VirtAddr, PAGE_SIZE};

use crate::proc::task::Task;
use crate::hal::rv64::user_trap::trap_return;
use crate::mm::vm::fault::{sync_fault_handler, FaultResult, FaultError, PageFaultAccessType};
use crate::kprintln;

use super::schedule::{spawn_kernel_task, yield_now};

// Interrupt bit in scause (bit 63 on rv64)
const SCAUSE_INTERRUPT: usize = 1 << 63;

// Interrupt cause codes
const IRQ_S_TIMER: usize = 5;
const IRQ_S_SOFTWARE: usize = 1;
const IRQ_S_EXTERNAL: usize = 9;

// Exception cause codes
const EXC_ECALL_U: usize = 8;
const EXC_INST_PAGE_FAULT: usize = 12;
const EXC_LOAD_PAGE_FAULT: usize = 13;
const EXC_LOAD_ACCESS_FAULT: usize = 5;
const EXC_STORE_ACCESS_FAULT: usize = 7;
const EXC_STORE_PAGE_FAULT: usize = 15;

// Linux-compatible rv64 syscall numbers
const SYS_GETCWD: usize = 17;
const SYS_DUP: usize = 23;
const SYS_DUP3: usize = 24;
const SYS_IOCTL: usize = 29;
const SYS_OPENAT: usize = 56;
const SYS_CLOSE: usize = 57;
const SYS_GETDENTS64: usize = 61;
const SYS_LSEEK: usize = 62;
const SYS_READ: usize = 63;
const SYS_WRITE: usize = 64;
const SYS_WRITEV: usize = 66;
const SYS_FSTAT: usize = 80;
const SYS_FSTATAT: usize = 79;
const SYS_EXIT: usize = 93;
const SYS_EXIT_GROUP: usize = 94;
const SYS_SET_TID_ADDRESS: usize = 96;
const SYS_CLOCK_GETTIME: usize = 113;
const SYS_SCHED_YIELD: usize = 124;
const SYS_SIGACTION: usize = 134;
const SYS_SIGPROCMASK: usize = 135;
const SYS_SIGRETURN: usize = 139;
const SYS_TIMES: usize = 153;
const SYS_UNAME: usize = 160;
const SYS_GETPID: usize = 172;
const SYS_GETPPID: usize = 173;
const SYS_GETUID: usize = 174;
const SYS_GETEUID: usize = 175;
const SYS_GETGID: usize = 176;
const SYS_GETEGID: usize = 177;
const SYS_GETTID: usize = 178;
const SYS_BRK: usize = 214;
const SYS_MUNMAP: usize = 215;
const SYS_CLONE: usize = 220;
const SYS_EXECVE: usize = 221;
const SYS_MMAP: usize = 222;
const SYS_MPROTECT: usize = 226;
const SYS_WAIT4: usize = 260;

/// Result of handling a user trap.
enum TrapResult {
    /// Continue the trap loop (return to user).
    Continue,
    /// Process has exited.
    Exit,
}

/// The persistent trap loop for a user process.
async fn run_tasks(task: Arc<Task>) {
    kprintln!("[run_tasks] starting pid={}", task.pid);
    loop {
        // Activate per-process page table before returning to user mode.
        {
            let mut pmap = task.pmap.lock();
            crate::mm::pmap::pmap_activate(&mut pmap);
        }

        kprintln!("[run_tasks] calling trap_return pid={}", task.pid);

        // Return to userspace. Blocks until user traps back.
        trap_return(&task);

        kprintln!("[run_tasks] returned from trap_return pid={}", task.pid);

        // Deactivate pmap (back in kernel context).
        {
            let mut pmap = task.pmap.lock();
            crate::mm::pmap::pmap_deactivate(&mut pmap);
        }

        // Dispatch based on scause in the trap frame.
        let result = user_trap_handler(&task).await;

        match result {
            TrapResult::Continue => {}
            TrapResult::Exit => break,
        }

        // Cooperative preemption point.
        yield_now().await;
    }
}

/// Async trap handler: dispatches syscalls, page faults, and interrupts.
async fn user_trap_handler(task: &Arc<Task>) -> TrapResult {
    let scause;
    let stval;
    {
        let tf = task.trap_frame.lock();
        scause = tf.scause;
        stval = tf.stval;
    }

    let is_interrupt = scause & SCAUSE_INTERRUPT != 0;
    let code = scause & !SCAUSE_INTERRUPT;

    {
        static TRAP_LOG_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
        let c = TRAP_LOG_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        if c < 10 {
            kprintln!("[user_trap] scause={:#x} is_int={} code={} cpu={}", scause, is_interrupt, code, crate::executor::per_cpu::current().cpu_id);
        }
    }

    if is_interrupt {
        match code {
            IRQ_S_TIMER => {
                static USER_TIMER_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
                let c = USER_TIMER_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                if c < 5 {
                    kprintln!("[user_trap] timer IRQ on cpu={}", crate::executor::per_cpu::current().cpu_id);
                }
                crate::hal::rv64::timer::handle_timer_irq();
            }
            IRQ_S_SOFTWARE => {
                crate::hal::rv64::ipi::handle_ipi();
            }
            IRQ_S_EXTERNAL => {
                // External IRQ handling
            }
            _ => {
                kprintln!("[user_trap] unhandled interrupt: code={}", code);
            }
        }
        return TrapResult::Continue;
    }

    // Exception handling
    match code {
        EXC_ECALL_U => {
            // Advance PC past ecall
            { task.trap_frame.lock().sepc += 4; }
            dispatch_syscall(task).await
        }
        EXC_LOAD_ACCESS_FAULT | EXC_STORE_ACCESS_FAULT |
        EXC_INST_PAGE_FAULT | EXC_LOAD_PAGE_FAULT | EXC_STORE_PAGE_FAULT => {
            // FINDING-01 fix: check pcb_onfault for copy_user fixup.
            // When copy_user_chunk faults from an async syscall handler,
            // the trap comes through __user_trap, not __kernel_trap.
            // We must redirect to the landing pad just like kernel_trap_handler does.
            {
                let pc = crate::executor::per_cpu::current();
                let onfault = pc.pcb_onfault.load(core::sync::atomic::Ordering::Relaxed);
                if onfault != 0 {
                    // Redirect sepc to landing pad
                    task.trap_frame.lock().sepc = onfault;
                    pc.pcb_onfault.store(0, core::sync::atomic::Ordering::Relaxed);
                    return TrapResult::Continue;
                }
            }

            // No fixup — real user page fault, try to resolve it.
            let fault_va = VirtAddr::new(stval & !(PAGE_SIZE - 1));
            let access_type = match code {
                EXC_STORE_PAGE_FAULT | EXC_STORE_ACCESS_FAULT => PageFaultAccessType::WRITE,
                EXC_INST_PAGE_FAULT => PageFaultAccessType::EXECUTE,
                _ => PageFaultAccessType::READ,
            };

            if let Err(e) = resolve_user_fault(task, fault_va, access_type).await {
                kprintln!(
                    "[user_trap] fatal fault: pid={} va={:#x} code={} err={:?}",
                    task.pid, stval, code, e
                );
                // TODO(Phase 4): deliver SIGSEGV. For now, kill process.
                do_exit(task, -11); // SIGSEGV = 11
                return TrapResult::Exit;
            }
            TrapResult::Continue
        }
        _ => {
            kprintln!(
                "[user_trap] unhandled exception: code={} sepc={:#x} stval={:#x}",
                code,
                { task.trap_frame.lock().sepc },
                stval
            );
            do_exit(task, -1);
            TrapResult::Exit
        }
    }
}

/// Pre-fault all user pages covering [user_ptr, user_ptr+len).
async fn fault_in_user_buffer(task: &Arc<Task>, user_ptr: usize, len: usize, access: PageFaultAccessType) {
    if len == 0 { return; }
    let start = user_ptr & !(PAGE_SIZE - 1);
    let end = (user_ptr + len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let mut va = start;
    while va < end {
        let _ = resolve_user_fault(task, VirtAddr::new(va), access).await;
        va += PAGE_SIZE;
    }
}

/// Unified fault resolution: sync path first, async fallback for file-backed.
async fn resolve_user_fault(
    task: &Arc<Task>,
    fault_va: VirtAddr,
    access_type: PageFaultAccessType,
) -> Result<(), FaultError> {
    // 1. Sync path: anonymous zero-fill, COW
    let sync_result = {
        let vm_map = task.vm_map.lock();
        let mut pmap = task.pmap.lock();
        sync_fault_handler(&vm_map, &mut pmap, fault_va, access_type)
    };

    match sync_result {
        FaultResult::Resolved => Ok(()),
        FaultResult::NeedsAsyncIO => {
            // 2. Async path: file-backed pages
            fault_in_page_async(task, fault_va).await
        }
        FaultResult::Error(e) => Err(e),
    }
}

/// Async file-backed page fault resolution with TOCTOU re-validation.
async fn fault_in_page_async(
    task: &Arc<Task>,
    fault_va: VirtAddr,
) -> Result<(), FaultError> {
    // Time-of-check: snapshot VMA state under lock
    let (vnode_id, vnode_path, file_offset) = {
        let map = task.vm_map.lock();
        let vma = map.find_area(fault_va).ok_or(FaultError::NotMapped)?;
        let vnode = vma.vnode.as_ref()
            .ok_or(FaultError::InvalidAccess)?;
        let page_idx = (fault_va.as_usize() - vma.range.start.as_usize()) / PAGE_SIZE;
        let file_offset = vma.file_offset + (page_idx * PAGE_SIZE) as u64;
        (vnode.vnode_id(), String::from(vnode.path()), file_offset)
    }; // lock dropped before .await

    // Fetch page through page cache (may await delegate I/O)
    let pa = page_cache_fetch_by_id(vnode_id, &vnode_path, file_offset).await?;

    // Time-of-use: re-validate VMA under lock before mapping
    {
        let map = task.vm_map.lock();
        let vma = map.find_area(fault_va).ok_or(FaultError::NotMapped)?;
        let expected_offset = vma.file_offset
            + ((fault_va.as_usize() - vma.range.start.as_usize()) & !(PAGE_SIZE - 1)) as u64;
        let current_id = vma.vnode.as_ref().map(|v| v.vnode_id());
        if current_id != Some(vnode_id) || expected_offset != file_offset {
            return Err(FaultError::InvalidAccess);
        }

        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));
        let mut pmap = task.pmap.lock();
        let _ = crate::mm::pmap::pmap_enter(&mut pmap, fault_va_aligned, pa, vma.prot, false);
    }

    Ok(())
}

/// Fetch a page from the page cache, initiating I/O if needed.
async fn page_cache_fetch_by_id(
    vnode_id: u64,
    path: &str,
    file_offset: u64,
) -> Result<hal_common::PhysAddr, FaultError> {
    use crate::fs::page_cache::{self, LookupResult};

    let page_offset = file_offset / PAGE_SIZE as u64;

    loop {
        // Use a no-op waker for the non-blocking probe path.
        let noop = noop_waker();
        let result = page_cache::lookup(vnode_id, page_offset, &noop);
        match result {
            LookupResult::Hit(pa) => return Ok(pa),
            LookupResult::InitiateFetch => {
                // We're responsible for fetching this page.
                match crate::fs::delegate::fs_read_page(path, file_offset).await {
                    Ok(pa_usize) => {
                        let pa = hal_common::PhysAddr::new(pa_usize);
                        page_cache::complete(vnode_id, page_offset, pa);
                        return Ok(pa);
                    }
                    Err(_) => {
                        // TODO: page_cache::abort(vnode_id, page_offset);
                        return Err(FaultError::IoError);
                    }
                }
            }
            LookupResult::WaitingOnFetch => {
                // Another task is fetching. Wait via proper future with real waker.
                PageCacheWaitFuture { vnode_id, page_offset }.await;
            }
        }
    }
}

/// Future that waits for a page cache fetch to complete.
struct PageCacheWaitFuture {
    vnode_id: u64,
    page_offset: u64,
}

impl Future for PageCacheWaitFuture {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        use crate::fs::page_cache::{self, LookupResult};
        match page_cache::lookup(self.vnode_id, self.page_offset, cx.waker()) {
            LookupResult::Hit(_) => Poll::Ready(()),
            LookupResult::WaitingOnFetch => Poll::Pending,
            LookupResult::InitiateFetch => Poll::Ready(()), // race: caller loop handles it
        }
    }
}

/// Create a no-op waker (for page_cache::lookup when we don't need to wait).
fn noop_waker() -> core::task::Waker {
    use core::task::{RawWaker, RawWakerVTable, Waker};
    fn noop(_: *const ()) {}
    fn clone_fn(p: *const ()) -> RawWaker { RawWaker::new(p, &VTABLE) }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_fn, noop, noop, noop);
    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) }
}

/// Async syscall dispatch.
async fn dispatch_syscall(task: &Arc<Task>) -> TrapResult {
    let (syscall_num, a0, a1, a2, _a3, _a4, _a5) = {
        let tf = task.trap_frame.lock();
        (tf.x[17], tf.x[10], tf.x[11], tf.x[12], tf.x[13], tf.x[14], tf.x[15])
    };

    let ret: usize = match syscall_num {
        // --- Fast-path synchronous syscalls ---
        SYS_GETPID => task.pid as usize,
        SYS_GETPPID => task.ppid() as usize,
        SYS_GETUID | SYS_GETEUID | SYS_GETGID | SYS_GETEGID => 0,
        SYS_GETTID => task.pid as usize,
        SYS_SET_TID_ADDRESS => task.pid as usize,
        SYS_SCHED_YIELD => {
            yield_now().await;
            0
        }
        SYS_BRK => {
            // Stub: return current brk (0 = let libc use mmap)
            0
        }
        SYS_MMAP => {
            // Stub: anonymous mmap bump allocator
            let len = a1;
            if len == 0 {
                (-22isize) as usize // EINVAL
            } else {
                static MMAP_NEXT: core::sync::atomic::AtomicUsize =
                    core::sync::atomic::AtomicUsize::new(0x0000_0020_0000_0000);
                MMAP_NEXT.fetch_add(
                    (len + 0xFFF) & !0xFFF,
                    core::sync::atomic::Ordering::Relaxed,
                )
            }
        }
        SYS_MPROTECT | SYS_MUNMAP => 0, // stub success
        SYS_SIGACTION | SYS_SIGPROCMASK | SYS_SIGRETURN => 0, // stub
        SYS_CLOCK_GETTIME => 0, // stub
        SYS_TIMES => 0, // stub
        SYS_UNAME => {
            // TODO: write utsname to user memory
            0
        }
        SYS_IOCTL => 0, // stub

        // --- Async syscalls ---
        SYS_WRITE => {
            // a0=fd, a1=buf, a2=len
            if (a0 == 1 || a0 == 2) && a2 > 0 {
                // stdout/stderr → UART
                // Fault in user pages covering the buffer, then copy with SUM.
                fault_in_user_buffer(task, a1, a2, PageFaultAccessType::READ).await;
                let mut kbuf = alloc::vec![0u8; a2];
                let rc = unsafe {
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        kbuf.as_mut_ptr(), a1 as *const u8, a2,
                    )
                };
                if rc != 0 {
                    (-14isize) as usize // EFAULT
                } else {
                    for &b in &kbuf {
                        crate::console::putchar(b);
                    }
                    a2
                }
            } else if a0 == 1 || a0 == 2 {
                0 // zero-length write
            } else {
                (-1isize) as usize // EPERM
            }
        }
        SYS_READ => {
            // a0=fd, a1=user_buf, a2=len
            match sys_read_async(task, a0 as u32, a1, a2).await {
                Ok(n) => n,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SYS_OPENAT => {
            // a0=dirfd, a1=pathname, a2=flags, a3=mode
            match sys_openat_async(task, a1, a2).await {
                Ok(fd) => fd as usize,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SYS_CLOSE => {
            let fd = a0 as u32;
            match crate::fs::syscalls::sys_close(&task.fd_table, fd) {
                Ok(()) => 0,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SYS_FSTAT => {
            match crate::fs::syscalls::sys_stat(&task.fd_table, a0 as u32) {
                Ok((_size, _vtype)) => {
                    // TODO: write stat struct to user memory at a1
                    0
                }
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SYS_EXIT | SYS_EXIT_GROUP => {
            do_exit(task, a0 as i32);
            return TrapResult::Exit;
        }
        SYS_CLONE => {
            // Basic fork (flags ignored for now)
            let child = crate::proc::fork::fork(task);
            let child_pid = child.pid;
            // Parent gets child PID in a0
            let parent_ret = child_pid as usize;
            // Spawn child on same CPU (child's trap_frame already has a0=0 from fork)
            let cpu = super::per_cpu::current().cpu_id;
            spawn_user_task(child, cpu);
            parent_ret
        }
        SYS_EXECVE => {
            // a0=pathname, a1=argv, a2=envp
            // Read pathname from user memory
            let path = match copyinstr(a0, 256) {
                Some(s) => s,
                None => return set_syscall_ret(task, (-(Errno::EFAULT.as_i32() as isize)) as usize),
            };
            // Read argv array from user memory (before exec destroys address space)
            let argv = copyin_argv(a1, 64, 4096);
            // Read envp array (optional, musl can cope with empty)
            let envp = copyin_argv(a2, 64, 4096);

            match crate::proc::exec::exec_with_args(task, &path, &argv, &envp).await {
                Ok((entry, sp)) => {
                    let mut tf = task.trap_frame.lock();
                    tf.sepc = entry;
                    tf.x[2] = sp;
                    for i in 1..32 {
                        if i != 2 { tf.x[i] = 0; }
                    }
                    tf.sstatus = 1 << 5; // SPP=0, SPIE=1
                    return TrapResult::Continue;
                }
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SYS_WAIT4 => {
            match sys_wait4_async(task, a0 as isize, a1).await {
                Ok(pid) => pid as usize,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        _ => {
            kprintln!("[syscall] unimplemented syscall {}", syscall_num);
            (-38isize) as usize // ENOSYS
        }
    };

    task.trap_frame.lock().set_ret_val(ret);
    TrapResult::Continue
}

/// sys_read through page cache with uiomove to user addresses.
async fn sys_read_async(
    task: &Arc<Task>,
    fd: u32,
    user_buf: usize,
    len: usize,
) -> Result<usize, Errno> {
    use crate::mm::uio::{uiomove, UioDir};

    let (vnode_id, vnode_path, file_size, mut offset) = {
        let tab = task.fd_table.lock();
        let file = tab.get(fd).ok_or(Errno::EBADF)?;
        let offset = file.offset.load(core::sync::atomic::Ordering::Relaxed);
        let vnode = &file.vnode;
        (vnode.vnode_id(), String::from(vnode.path()), vnode.size(), offset)
    };

    if offset >= file_size {
        return Ok(0);
    }
    let to_read = core::cmp::min(len, (file_size - offset) as usize);
    let mut total = 0usize;

    while total < to_read {
        let page_offset = offset / PAGE_SIZE as u64;
        let offset_in_page = (offset % PAGE_SIZE as u64) as usize;
        let chunk = core::cmp::min(PAGE_SIZE - offset_in_page, to_read - total);

        // Ensure page is in cache
        let pa = page_cache_fetch_by_id(vnode_id, &vnode_path, page_offset * PAGE_SIZE as u64).await
            .map_err(|_| Errno::EIO)?;

        // Copy from page cache to user buffer via uiomove
        // uiomove(kern, user, len, dir): CopyOut copies kern→user
        let kern = (pa.as_usize() + offset_in_page) as *mut u8;
        let user = (user_buf + total) as *mut u8;

        match uiomove(kern, user, chunk, UioDir::CopyOut) {
            Ok(result) => {
                total += result.done;
                offset += result.done as u64;
            }
            Err(Errno::EFAULT) => {
                // User buffer page not present — resolve fault and retry
                resolve_user_fault(
                    task,
                    VirtAddr::new(user_buf + total),
                    PageFaultAccessType::WRITE,
                ).await.map_err(|_| Errno::EFAULT)?;
                continue; // retry, don't advance
            }
            Err(e) => return Err(e),
        }
    }

    // Update file offset
    {
        let tab = task.fd_table.lock();
        if let Some(file) = tab.get(fd) {
            file.offset.store(offset, core::sync::atomic::Ordering::Relaxed);
        }
    }

    Ok(total)
}

/// sys_openat: resolve path and open file.
async fn sys_openat_async(
    task: &Arc<Task>,
    pathname_ptr: usize,
    flags: usize,
) -> Result<u32, Errno> {
    use crate::fs::fd_table::OpenFlags;

    // Read pathname from user memory using fault-safe copyinstr.
    let path_str = copyinstr(pathname_ptr, 256).ok_or(Errno::EFAULT)?;

    let open_flags = OpenFlags {
        read: true,
        write: (flags & 0x1) != 0 || (flags & 0x2) != 0,
    };

    crate::fs::syscalls::sys_open(&task.fd_table, &path_str, open_flags).await
}

/// sys_wait4: wait for child process.
async fn sys_wait4_async(
    task: &Arc<Task>,
    _pid: isize,
    wstatus_ptr: usize,
) -> Result<u32, Errno> {
    use crate::proc::exit_wait::WaitChildFuture;

    // WaitChildFuture returns Option<(child_pid, exit_status)>.
    // None means no children exist.
    let result = WaitChildFuture::new(Arc::clone(task)).await;

    match result {
        Some((child_pid, status)) => {
            // Write status to user memory if pointer is non-null
            if wstatus_ptr != 0 {
                let encoded = (status << 8) & 0xff00; // WEXITSTATUS encoding
                unsafe { *(wstatus_ptr as *mut i32) = encoded; }
            }
            Ok(child_pid)
        }
        None => Err(Errno::ECHILD),
    }
}

/// Read a NUL-terminated string from user memory. Returns None on fault.
fn copyinstr(user_ptr: usize, max_len: usize) -> Option<String> {
    if user_ptr == 0 {
        return None;
    }
    let mut buf = alloc::vec![0u8; max_len];
    // Copy from user memory using fault-safe copy_user_chunk.
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            buf.as_mut_ptr(),
            user_ptr as *const u8,
            max_len,
        )
    };
    if rc != 0 {
        return None; // EFAULT
    }
    // Find NUL terminator
    let nul_pos = buf.iter().position(|&b| b == 0)?;
    buf.truncate(nul_pos);
    Some(unsafe { String::from_utf8_unchecked(buf) })
}

/// Read a NULL-terminated array of string pointers from user memory.
/// Returns a Vec of Strings. Stops at NULL pointer or max_count.
fn copyin_argv(user_argv: usize, max_count: usize, max_total: usize) -> alloc::vec::Vec<String> {
    let mut result = alloc::vec::Vec::new();
    if user_argv == 0 {
        return result;
    }
    let mut total = 0usize;
    for i in 0..max_count {
        let ptr_addr = user_argv + i * core::mem::size_of::<usize>();
        let mut str_ptr: usize = 0;
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                &mut str_ptr as *mut usize as *mut u8,
                ptr_addr as *const u8,
                core::mem::size_of::<usize>(),
            )
        };
        if rc != 0 || str_ptr == 0 {
            break;
        }
        if let Some(s) = copyinstr(str_ptr, 256) {
            total += s.len() + 1;
            if total > max_total {
                break;
            }
            result.push(s);
        } else {
            break;
        }
    }
    result
}

/// Helper: set syscall return value and return Continue.
fn set_syscall_ret(task: &Arc<Task>, val: usize) -> TrapResult {
    task.trap_frame.lock().set_ret_val(val);
    TrapResult::Continue
}

/// Mark process as zombie and wake parent.
fn do_exit(task: &Arc<Task>, status: i32) {
    task.exit_status.store(status, core::sync::atomic::Ordering::Release);
    task.set_zombie();

    // Wake parent's WaitChildFuture
    if let Some(parent) = task.parent.upgrade() {
        if let Some(waker) = parent.parent_waker.lock().take() {
            waker.wake();
        }
    }
}

/// Spawn a user task on the given CPU.
pub fn spawn_user_task(task: Arc<Task>, cpu: usize) {
    let handle = spawn_kernel_task(run_tasks(task), cpu);
    handle.detach();
}
