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
#[derive(Clone, Copy, PartialEq, Eq)]
struct SyscallId(usize);

impl SyscallId {
    const GETCWD: Self = Self(17);
    const DUP: Self = Self(23);
    const DUP3: Self = Self(24);
    const IOCTL: Self = Self(29);
    const OPENAT: Self = Self(56);
    const CLOSE: Self = Self(57);
    const GETDENTS64: Self = Self(61);
    const LSEEK: Self = Self(62);
    const READ: Self = Self(63);
    const WRITE: Self = Self(64);
    const WRITEV: Self = Self(66);
    const FSTATAT: Self = Self(79);
    const FSTAT: Self = Self(80);
    const EXIT: Self = Self(93);
    const EXIT_GROUP: Self = Self(94);
    const SET_TID_ADDRESS: Self = Self(96);
    const CLOCK_GETTIME: Self = Self(113);
    const SCHED_YIELD: Self = Self(124);
    const SIGACTION: Self = Self(134);
    const SIGPROCMASK: Self = Self(135);
    const SIGRETURN: Self = Self(139);
    const TIMES: Self = Self(153);
    const UNAME: Self = Self(160);
    const GETPID: Self = Self(172);
    const GETPPID: Self = Self(173);
    const GETUID: Self = Self(174);
    const GETEUID: Self = Self(175);
    const GETGID: Self = Self(176);
    const GETEGID: Self = Self(177);
    const GETTID: Self = Self(178);
    const BRK: Self = Self(214);
    const MUNMAP: Self = Self(215);
    const CLONE: Self = Self(220);
    const EXECVE: Self = Self(221);
    const MMAP: Self = Self(222);
    const MPROTECT: Self = Self(226);
    const WAIT4: Self = Self(260);
    const PIPE2: Self = Self(59);
}

impl core::fmt::Display for SyscallId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match *self {
            Self::GETCWD => "getcwd",
            Self::DUP => "dup",
            Self::DUP3 => "dup3",
            Self::IOCTL => "ioctl",
            Self::OPENAT => "openat",
            Self::CLOSE => "close",
            Self::GETDENTS64 => "getdents64",
            Self::LSEEK => "lseek",
            Self::READ => "read",
            Self::WRITE => "write",
            Self::WRITEV => "writev",
            Self::FSTATAT => "fstatat",
            Self::FSTAT => "fstat",
            Self::EXIT => "exit",
            Self::EXIT_GROUP => "exit_group",
            Self::SET_TID_ADDRESS => "set_tid_address",
            Self::CLOCK_GETTIME => "clock_gettime",
            Self::SCHED_YIELD => "sched_yield",
            Self::SIGACTION => "sigaction",
            Self::SIGPROCMASK => "sigprocmask",
            Self::SIGRETURN => "sigreturn",
            Self::TIMES => "times",
            Self::UNAME => "uname",
            Self::GETPID => "getpid",
            Self::GETPPID => "getppid",
            Self::GETUID => "getuid",
            Self::GETEUID => "geteuid",
            Self::GETGID => "getgid",
            Self::GETEGID => "getegid",
            Self::GETTID => "gettid",
            Self::BRK => "brk",
            Self::MUNMAP => "munmap",
            Self::CLONE => "clone",
            Self::EXECVE => "execve",
            Self::MMAP => "mmap",
            Self::MPROTECT => "mprotect",
            Self::WAIT4 => "wait4",
            Self::PIPE2 => "pipe2",
            _ => return write!(f, "unknown({})", self.0),
        };
        write!(f, "{}", name)
    }
}

impl core::fmt::Debug for SyscallId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}({})", self, self.0)
    }
}

/// Result of handling a user trap.
enum TrapResult {
    /// Continue the trap loop (return to user).
    Continue,
    /// Process has exited.
    Exit,
}

/// The persistent trap loop for a user process.
async fn run_tasks(task: Arc<Task>) {
    klog!(sched, debug, "run_tasks: starting pid={}", task.pid);
    loop {
        // Activate per-process page table before returning to user mode.
        {
            let mut pmap = task.pmap.lock();
            crate::mm::pmap::pmap_activate(&mut pmap);
        }

        klog!(sched, debug, "run_tasks: calling trap_return pid={}", task.pid);

        // Return to userspace. Blocks until user traps back.
        trap_return(&task);

        klog!(sched, debug, "run_tasks: returned from trap_return pid={}", task.pid);

        // Dispatch based on scause in the trap frame.
        // Keep user pmap active — syscall handlers need it for copy_user / fault resolution.
        let result = user_trap_handler(&task).await;

        // Deactivate pmap (back in kernel context).
        {
            let mut pmap = task.pmap.lock();
            crate::mm::pmap::pmap_deactivate(&mut pmap);
        }

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

    if is_interrupt {
        match code {
            IRQ_S_TIMER => {
                crate::hal::rv64::timer::handle_timer_irq();
            }
            IRQ_S_SOFTWARE => {
                crate::hal::rv64::ipi::handle_ipi();
            }
            IRQ_S_EXTERNAL => {
                // External IRQ handling
            }
            _ => {
                klog!(trap, debug, "unhandled interrupt: code={}", code);
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
                let pc = task.trap_frame.lock().sepc;
                klog!(trap, error,
                    "fatal fault: pid={} va={:#x} pc={:#x} code={} err={:?}",
                    task.pid, stval, pc, code, e
                );
                // TODO(Phase 4): deliver SIGSEGV. For now, kill process.
                do_exit(task, -11); // SIGSEGV = 11
                return TrapResult::Exit;
            }
            TrapResult::Continue
        }
        _ => {
            klog!(trap, error,
                "unhandled exception: code={} sepc={:#x} stval={:#x}",
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
    let (vnode_id, vnode_path, file_offset, file_size, _vma_file_offset, vma_start) = {
        let map = task.vm_map.lock();
        let vma = map.find_area(fault_va).ok_or(FaultError::NotMapped)?;
        let vnode = vma.vnode.as_ref()
            .ok_or(FaultError::InvalidAccess)?;
        let page_idx = (fault_va.as_usize() - vma.range.start.as_usize()) / PAGE_SIZE;
        let file_offset = vma.file_offset + (page_idx * PAGE_SIZE) as u64;
        (
            vnode.vnode_id(),
            String::from(vnode.path()),
            file_offset,
            vma.file_size,
            vma.file_offset,
            vma.range.start.as_usize(),
        )
    }; // lock dropped before .await

    // Compute how far into the VMA this page starts (in bytes)
    let fault_va_aligned = fault_va.as_usize() & !(PAGE_SIZE - 1);
    let vma_page_byte_offset = (fault_va_aligned - vma_start) as u64;

    let pa = if vma_page_byte_offset >= file_size {
        // Entirely beyond file data — pure BSS zero page
        let frame = crate::mm::allocator::frame_alloc_sync()
            .ok_or(FaultError::OutOfMemory)?;
        unsafe { core::ptr::write_bytes(frame.as_usize() as *mut u8, 0, PAGE_SIZE); }
        frame
    } else if vma_page_byte_offset + PAGE_SIZE as u64 > file_size {
        // Partial page: file data + zero fill for the rest
        let fetched = page_cache_fetch_by_id(vnode_id, &vnode_path, file_offset).await?;
        // Copy file portion to a new frame and zero the tail
        let frame = crate::mm::allocator::frame_alloc_sync()
            .ok_or(FaultError::OutOfMemory)?;
        let file_bytes = (file_size - vma_page_byte_offset) as usize;
        unsafe {
            let src = fetched.as_usize() as *const u8;
            let dst = frame.as_usize() as *mut u8;
            core::ptr::copy_nonoverlapping(src, dst, file_bytes);
            core::ptr::write_bytes(dst.add(file_bytes), 0, PAGE_SIZE - file_bytes);
        }
        frame
    } else {
        // Fully within file data — use page cache directly
        page_cache_fetch_by_id(vnode_id, &vnode_path, file_offset).await?
    };

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
    let (id, a0, a1, a2, _a3, _a4, _a5) = {
        let tf = task.trap_frame.lock();
        (SyscallId(tf.x[17]), tf.x[10], tf.x[11], tf.x[12], tf.x[13], tf.x[14], tf.x[15])
    };

    klog!(syscall, debug, "pid={} {} a0={:#x} a1={:#x} a2={:#x}", task.pid, id, a0, a1, a2);

    let ret: usize = match id {
        // --- Fast-path synchronous syscalls ---
        SyscallId::GETPID => task.pid as usize,
        SyscallId::GETPPID => task.ppid() as usize,
        SyscallId::GETUID | SyscallId::GETEUID | SyscallId::GETGID | SyscallId::GETEGID => 0,
        SyscallId::GETTID => task.pid as usize,
        SyscallId::SET_TID_ADDRESS => task.pid as usize,
        SyscallId::DUP => {
            match task.fd_table.lock().dup(a0 as u32) {
                Ok(fd) => fd as usize,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::DUP3 => {
            // a0=oldfd, a1=newfd, a2=flags
            let cloexec = (a2 & 0o2000000) != 0;
            match task.fd_table.lock().dup3(a0 as u32, a1 as u32, cloexec) {
                Ok(fd) => fd as usize,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::PIPE2 => {
            // a0=pipefd[2] user pointer, a1=flags
            match sys_pipe2(task, a0, a1) {
                Ok(()) => 0,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::SCHED_YIELD => {
            yield_now().await;
            0
        }
        SyscallId::BRK => {
            use crate::mm::vm::vm_map::{VmArea, VmAreaType, MapPerm};
            use crate::mm::vm::vm_object::VmObject;
            use crate::mm::vm::vm_object::PageOwnership;

            let current_brk = task.brk.load(core::sync::atomic::Ordering::Relaxed);
            if a0 == 0 {
                // Query current brk
                current_brk
            } else {
                let new_brk = (a0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
                let old_brk = current_brk;
                if new_brk > old_brk {
                    // --- Expand ---
                    let mut vm = task.vm_map.lock();
                    if let Some(heap_vma) = vm.find_area_ending_at_mut(
                        VirtAddr::new(old_brk), VmAreaType::Heap,
                    ) {
                        // In-place extension: slide end, grow VmObject
                        heap_vma.range.end = VirtAddr::new(new_brk);
                        let mut obj = heap_vma.object.write();
                        obj.set_size(new_brk - heap_vma.range.start.as_usize());
                    } else {
                        // First brk or no adjacent heap VMA — create new
                        let size = new_brk - old_brk;
                        let obj = VmObject::new(size);
                        let vma = VmArea::new(
                            VirtAddr::new(old_brk)..VirtAddr::new(new_brk),
                            MapPerm::R | MapPerm::W | MapPerm::U,
                            obj,
                            0,
                            VmAreaType::Heap,
                        );
                        if vm.insert(vma).is_err() {
                            return set_syscall_ret(task, current_brk);
                        }
                    }
                } else if new_brk < old_brk {
                    // --- Shrink ---
                    // 1. Tear down hardware PTEs + TLB shootdown
                    {
                        let mut pmap = task.pmap.lock();
                        crate::mm::pmap::pmap_remove(
                            &mut pmap,
                            VirtAddr::new(new_brk),
                            VirtAddr::new(old_brk),
                        );
                    }
                    // 2. Truncate VmObject pages and free anonymous frames
                    let mut vm = task.vm_map.lock();
                    // Find the heap VMA that contains old_brk - 1
                    if let Some(heap_vma) = vm.find_area_mut(
                        VirtAddr::new(old_brk - 1),
                    ) {
                        if heap_vma.vma_type == VmAreaType::Heap {
                            let vma_start = heap_vma.range.start.as_usize();
                            let from_page = ((new_brk - vma_start) / PAGE_SIZE) as u64;
                            // Truncate pages from VmObject (top-level only — COW safe)
                            let freed = {
                                let mut obj = heap_vma.object.write();
                                let pages = obj.truncate_pages(from_page);
                                obj.set_size(new_brk.saturating_sub(vma_start));
                                pages
                            };
                            // Free anonymous frames
                            for page in freed {
                                if matches!(page.ownership, PageOwnership::Anonymous) {
                                    crate::mm::allocator::frame_free(page.phys);
                                }
                            }
                            // Slide VMA end down (or remove if fully shrunk)
                            if new_brk <= vma_start {
                                vm.remove(VirtAddr::new(vma_start));
                            } else {
                                // Re-lookup since we dropped the borrow for frame_free
                                if let Some(vma) = vm.find_area_mut(VirtAddr::new(vma_start)) {
                                    vma.range.end = VirtAddr::new(new_brk);
                                }
                            }
                        }
                    }
                }
                task.brk.store(new_brk, core::sync::atomic::Ordering::Relaxed);
                new_brk
            }
        }
        SyscallId::MMAP => {
            // Anonymous mmap: create a real VMA so page faults resolve
            let addr = a0;
            let len = a1;
            let prot_bits = a2;
            if len == 0 {
                (-22isize) as usize // EINVAL
            } else {
                use crate::mm::vm::vm_map::{VmArea, VmAreaType, MapPerm};
                use crate::mm::vm::vm_object::VmObject;

                let aligned_len = (len + 0xFFF) & !0xFFF;
                let pages = aligned_len / PAGE_SIZE;

                // Bump allocator for mmap base address
                static MMAP_NEXT: core::sync::atomic::AtomicUsize =
                    core::sync::atomic::AtomicUsize::new(0x0000_0020_0000_0000);
                let base = if addr != 0 {
                    // MAP_FIXED hint — use requested address
                    addr & !0xFFF
                } else {
                    MMAP_NEXT.fetch_add(aligned_len, core::sync::atomic::Ordering::Relaxed)
                };

                let mut perm = MapPerm::U;
                if prot_bits & 0x1 != 0 { perm |= MapPerm::R; }
                if prot_bits & 0x2 != 0 { perm |= MapPerm::W; }
                if prot_bits & 0x4 != 0 { perm |= MapPerm::X; }

                let obj = VmObject::new(pages);
                let vma = VmArea::new(
                    VirtAddr::new(base)..VirtAddr::new(base + aligned_len),
                    perm,
                    obj,
                    0,
                    VmAreaType::Anonymous,
                );

                let mut vm = task.vm_map.lock();
                match vm.insert(vma) {
                    Ok(()) => base,
                    Err(_) => (-12isize) as usize, // ENOMEM
                }
            }
        }
        SyscallId::MPROTECT | SyscallId::MUNMAP => 0, // stub success
        SyscallId::SIGACTION | SyscallId::SIGPROCMASK | SyscallId::SIGRETURN => 0, // stub
        SyscallId::CLOCK_GETTIME => 0, // stub
        SyscallId::TIMES => 0, // stub
        SyscallId::UNAME => {
            // TODO: write utsname to user memory
            0
        }
        SyscallId::IOCTL => {
            // a0=fd, a1=request, a2=argp
            match sys_ioctl(task, a0 as u32, a1, a2) {
                Ok(v) => v as usize,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }

        // --- Async syscalls ---
        SyscallId::WRITE => {
            // a0=fd, a1=buf, a2=len
            match sys_write_async(task, a0 as u32, a1, a2).await {
                Ok(n) => n,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::READ => {
            // a0=fd, a1=user_buf, a2=len
            match sys_read_async(task, a0 as u32, a1, a2).await {
                Ok(n) => n,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::OPENAT => {
            // a0=dirfd, a1=pathname, a2=flags, a3=mode
            match sys_openat_async(task, a1, a2).await {
                Ok(fd) => fd as usize,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::CLOSE => {
            let fd = a0 as u32;
            match crate::fs::syscalls::sys_close(&task.fd_table, fd) {
                Ok(()) => 0,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::FSTAT => {
            match crate::fs::syscalls::sys_stat(&task.fd_table, a0 as u32) {
                Ok((_size, _vtype)) => {
                    // TODO: write stat struct to user memory at a1
                    0
                }
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::EXIT | SyscallId::EXIT_GROUP => {
            do_exit(task, a0 as i32);
            return TrapResult::Exit;
        }
        SyscallId::CLONE => {
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
        SyscallId::EXECVE => {
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
        SyscallId::WAIT4 => {
            match sys_wait4_async(task, a0 as isize, a1).await {
                Ok(pid) => pid as usize,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        _ => {
            klog!(syscall, info, "unimplemented {}", id);
            (-38isize) as usize // ENOSYS
        }
    };

    task.trap_frame.lock().set_ret_val(ret);
    TrapResult::Continue
}

/// sys_read through the three-layer FD model.
async fn sys_read_async(
    task: &Arc<Task>,
    fd: u32,
    user_buf: usize,
    len: usize,
) -> Result<usize, Errno> {
    use crate::fs::fd_table::{FileObject, DeviceKind};
    use crate::mm::uio::{uiomove, UioDir};

    if len == 0 {
        return Ok(0);
    }

    // Snapshot what we need from the fd table
    enum ReadSource {
        Vnode { id: u64, path: String, size: u64, offset: u64 },
        PipeRead(alloc::sync::Arc<crate::fs::pipe::Pipe>),
        DevNull,
        DevZero,
        DevConsole,
    }

    let (source, desc) = {
        let tab = task.fd_table.lock();
        let d = tab.get(fd).ok_or(Errno::EBADF)?;
        if !d.flags.read {
            return Err(Errno::EPERM);
        }
        let src = match &d.object {
            FileObject::Vnode(v) => ReadSource::Vnode {
                id: v.vnode_id(),
                path: String::from(v.path()),
                size: v.size(),
                offset: d.offset.load(core::sync::atomic::Ordering::Relaxed),
            },
            FileObject::PipeRead(p) => ReadSource::PipeRead(Arc::clone(p)),
            FileObject::PipeWrite(_) => return Err(Errno::EBADF),
            FileObject::Device(DeviceKind::Null) => ReadSource::DevNull,
            FileObject::Device(DeviceKind::Zero) => ReadSource::DevZero,
            FileObject::Device(DeviceKind::ConsoleRead) => ReadSource::DevConsole,
            FileObject::Device(DeviceKind::ConsoleWrite) => return Err(Errno::EBADF),
        };
        (src, Arc::clone(d))
    };

    match source {
        ReadSource::DevNull => Ok(0), // EOF
        ReadSource::DevZero => {
            // Fill user buffer with zeros
            fault_in_user_buffer(task, user_buf, len, PageFaultAccessType::WRITE).await;
            let zeros = alloc::vec![0u8; len];
            let rc = unsafe {
                crate::hal::rv64::copy_user::copy_user_chunk(
                    user_buf as *mut u8, zeros.as_ptr(), len,
                )
            };
            if rc != 0 { Err(Errno::EFAULT) } else { Ok(len) }
        }
        ReadSource::DevConsole => {
            // Console read: return 0 for now (no input buffer yet)
            Ok(0)
        }
        ReadSource::PipeRead(pipe) => {
            // Async pipe read: loop until data available or EOF
            PipeReadFuture { pipe, task, user_buf, len }.await
        }
        ReadSource::Vnode { id, path, size, mut offset } => {
            if offset >= size {
                return Ok(0);
            }
            let to_read = core::cmp::min(len, (size - offset) as usize);
            let mut total = 0usize;

            while total < to_read {
                let page_offset = offset / PAGE_SIZE as u64;
                let offset_in_page = (offset % PAGE_SIZE as u64) as usize;
                let chunk = core::cmp::min(PAGE_SIZE - offset_in_page, to_read - total);

                let pa = page_cache_fetch_by_id(id, &path, page_offset * PAGE_SIZE as u64).await
                    .map_err(|_| Errno::EIO)?;

                let kern = (pa.as_usize() + offset_in_page) as *mut u8;
                let user = (user_buf + total) as *mut u8;

                match uiomove(kern, user, chunk, UioDir::CopyOut) {
                    Ok(result) => {
                        total += result.done;
                        offset += result.done as u64;
                    }
                    Err(Errno::EFAULT) => {
                        resolve_user_fault(
                            task,
                            VirtAddr::new(user_buf + total),
                            PageFaultAccessType::WRITE,
                        ).await.map_err(|_| Errno::EFAULT)?;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            desc.offset.store(offset, core::sync::atomic::Ordering::Relaxed);
            Ok(total)
        }
    }
}

/// Future for async pipe read.
struct PipeReadFuture<'a> {
    pipe: alloc::sync::Arc<crate::fs::pipe::Pipe>,
    task: &'a Arc<Task>,
    user_buf: usize,
    len: usize,
}

impl<'a> core::future::Future for PipeReadFuture<'a> {
    type Output = Result<usize, Errno>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut kbuf = alloc::vec![0u8; this.len];
        match this.pipe.read(&mut kbuf) {
            Ok(0) => Poll::Ready(Ok(0)), // EOF
            Ok(n) => {
                // Copy to user buffer (best-effort, fault in pages first would be ideal)
                let rc = unsafe {
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        this.user_buf as *mut u8, kbuf.as_ptr(), n,
                    )
                };
                if rc != 0 {
                    Poll::Ready(Err(Errno::EFAULT))
                } else {
                    Poll::Ready(Ok(n))
                }
            }
            Err(Errno::EAGAIN) => {
                this.pipe.register_reader_waker(cx.waker());
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

/// sys_write through the three-layer FD model.
async fn sys_write_async(
    task: &Arc<Task>,
    fd: u32,
    user_buf: usize,
    len: usize,
) -> Result<usize, Errno> {
    use crate::fs::fd_table::{FileObject, DeviceKind};

    if len == 0 {
        return Ok(0);
    }

    enum WriteTarget {
        DevNull,
        DevConsole,
        PipeWrite(alloc::sync::Arc<crate::fs::pipe::Pipe>),
    }

    let target = {
        let tab = task.fd_table.lock();
        let d = tab.get(fd).ok_or(Errno::EBADF)?;
        if !d.flags.write {
            return Err(Errno::EPERM);
        }
        match &d.object {
            FileObject::Device(DeviceKind::Null) => WriteTarget::DevNull,
            FileObject::Device(DeviceKind::Zero) => WriteTarget::DevNull,
            FileObject::Device(DeviceKind::ConsoleWrite) | FileObject::Device(DeviceKind::ConsoleRead) => WriteTarget::DevConsole,
            FileObject::PipeWrite(p) => WriteTarget::PipeWrite(Arc::clone(p)),
            FileObject::PipeRead(_) => return Err(Errno::EBADF),
            FileObject::Vnode(_) => return Err(Errno::EPERM), // read-only fs
        }
    };

    match target {
        WriteTarget::DevNull => Ok(len),
        WriteTarget::DevConsole => {
            fault_in_user_buffer(task, user_buf, len, PageFaultAccessType::READ).await;
            let mut kbuf = alloc::vec![0u8; len];
            let rc = unsafe {
                crate::hal::rv64::copy_user::copy_user_chunk(
                    kbuf.as_mut_ptr(), user_buf as *const u8, len,
                )
            };
            if rc != 0 {
                return Err(Errno::EFAULT);
            }
            for &b in &kbuf {
                crate::console::putchar(b);
            }
            Ok(len)
        }
        WriteTarget::PipeWrite(pipe) => {
            // Copy user data to kernel buffer first
            fault_in_user_buffer(task, user_buf, len, PageFaultAccessType::READ).await;
            let mut kbuf = alloc::vec![0u8; len];
            let rc = unsafe {
                crate::hal::rv64::copy_user::copy_user_chunk(
                    kbuf.as_mut_ptr(), user_buf as *const u8, len,
                )
            };
            if rc != 0 {
                return Err(Errno::EFAULT);
            }
            PipeWriteFuture { pipe, data: kbuf, written: 0 }.await
        }
    }
}

/// Future for async pipe write.
struct PipeWriteFuture {
    pipe: alloc::sync::Arc<crate::fs::pipe::Pipe>,
    data: alloc::vec::Vec<u8>,
    written: usize,
}

impl core::future::Future for PipeWriteFuture {
    type Output = Result<usize, Errno>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        loop {
            if this.written >= this.data.len() {
                return Poll::Ready(Ok(this.written));
            }
            match this.pipe.write(&this.data[this.written..]) {
                Ok(n) => {
                    this.written += n;
                    if this.written >= this.data.len() {
                        return Poll::Ready(Ok(this.written));
                    }
                    // Partial write — register waker and wait for space
                    this.pipe.register_writer_waker(cx.waker());
                    return Poll::Pending;
                }
                Err(Errno::EAGAIN) => {
                    this.pipe.register_writer_waker(cx.waker());
                    return Poll::Pending;
                }
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }
}

/// sys_ioctl: handle terminal ioctls for isatty() support.
fn sys_ioctl(
    task: &Arc<Task>,
    fd: u32,
    request: usize,
    argp: usize,
) -> Result<i32, Errno> {
    use crate::fs::fd_table::{FileObject, DeviceKind};

    let tab = task.fd_table.lock();
    let desc = tab.get(fd).ok_or(Errno::EBADF)?;

    // Only console devices support ioctls
    let is_console = matches!(
        &desc.object,
        FileObject::Device(DeviceKind::ConsoleRead) | FileObject::Device(DeviceKind::ConsoleWrite)
    );
    if !is_console {
        return Err(Errno::ENOTTY);
    }

    const TCGETS: usize = 0x5401;
    const TCSETS: usize = 0x5402;
    const TCSETSW: usize = 0x5403;
    const TCSETSF: usize = 0x5404;
    const TIOCGWINSZ: usize = 0x5413;
    const FIONBIO: usize = 0x5421;

    match request {
        TCGETS => {
            // Write a basic Termios struct: ICANON|ECHO, B38400
            // Linux struct termios is 60 bytes (on most arches)
            if argp != 0 {
                let mut termios = [0u32; 15]; // 60 bytes
                termios[0] = 0; // c_iflag
                termios[1] = 0; // c_oflag
                termios[2] = 0o000017; // c_cflag: CS8 | B38400
                termios[3] = 0o000012; // c_lflag: ICANON | ECHO
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        termios.as_ptr() as *const u8,
                        argp as *mut u8,
                        60,
                    );
                }
            }
            Ok(0)
        }
        TIOCGWINSZ => {
            // struct winsize { unsigned short ws_row, ws_col, ws_xpixel, ws_ypixel; }
            if argp != 0 {
                let winsize: [u16; 4] = [24, 80, 0, 0];
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        winsize.as_ptr() as *const u8,
                        argp as *mut u8,
                        8,
                    );
                }
            }
            Ok(0)
        }
        TCSETS | TCSETSW | TCSETSF => {
            // Accept silently (no hardware effect)
            Ok(0)
        }
        FIONBIO => {
            // Accept silently
            Ok(0)
        }
        _ => Err(Errno::ENOTTY),
    }
}

/// sys_pipe2: create a pipe and return two fds.
fn sys_pipe2(
    task: &Arc<Task>,
    pipefd_ptr: usize,
    flags: usize,
) -> Result<(), Errno> {
    use crate::fs::fd_table::{FdFlags, FileDescription, FileObject, OpenFlags};
    use crate::fs::pipe::Pipe;

    let pipe = Pipe::new();
    let cloexec = (flags & 0o2000000) != 0;
    let fd_flags = if cloexec { FdFlags::CLOEXEC } else { FdFlags::empty() };

    let read_desc = FileDescription::new(FileObject::PipeRead(Arc::clone(&pipe)), OpenFlags::RDONLY);
    let write_desc = FileDescription::new(FileObject::PipeWrite(pipe), OpenFlags::WRONLY);

    let (read_fd, write_fd) = {
        let mut tab = task.fd_table.lock();
        let rfd = tab.insert(read_desc, fd_flags)?;
        let wfd = match tab.insert(write_desc, fd_flags) {
            Ok(fd) => fd,
            Err(e) => {
                tab.remove(rfd); // rollback
                return Err(e);
            }
        };
        (rfd, wfd)
    };

    // Write [read_fd, write_fd] to user memory
    if pipefd_ptr != 0 {
        let fds: [i32; 2] = [read_fd as i32, write_fd as i32];
        unsafe {
            core::ptr::copy_nonoverlapping(
                fds.as_ptr() as *const u8,
                pipefd_ptr as *mut u8,
                8,
            );
        }
    }

    Ok(())
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
