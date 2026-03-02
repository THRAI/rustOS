//! User task future: persistent per-process future + async trap handler.
//!
//! UserTaskFuture wraps run_tasks and switches address space on poll.
//! run_tasks is the trap loop: trap_return → user runs → trap back → dispatch.
//! spawn_user_task creates and detaches the future on a target CPU.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use hal_common::{Errno, VirtAddr, PAGE_SIZE};

use crate::hal::rv64::user_trap::trap_return;
use crate::mm::vm::fault::{sync_fault_handler, FaultError, FaultResult, PageFaultAccessType};
use crate::proc::task::Task;

use super::schedule::{spawn_kernel_task, yield_now};
// Debug logging is now controlled by standard `klog!` levels and Makefile `LEVEL=trace`.
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
const EXC_INST_ACCESS_FAULT: usize = 1;
const EXC_STORE_PAGE_FAULT: usize = 15;

// Linux-compatible rv64 syscall numbers
#[derive(Clone, Copy, PartialEq, Eq)]
struct SyscallId(usize);

impl SyscallId {
    const GETCWD: Self = Self(17);
    const DUP: Self = Self(23);
    const DUP3: Self = Self(24);
    const IOCTL: Self = Self(29);
    const CHDIR: Self = Self(49);
    const OPENAT: Self = Self(56);
    const CLOSE: Self = Self(57);
    const GETDENTS64: Self = Self(61);
    const LSEEK: Self = Self(62);
    const READ: Self = Self(63);
    const WRITE: Self = Self(64);
    const READV: Self = Self(65);
    const WRITEV: Self = Self(66);
    const SENDFILE: Self = Self(71);
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
    const NANOSLEEP: Self = Self(101);
    const FUTEX: Self = Self(98);
    const KILL: Self = Self(129);
    const SETPGID: Self = Self(154);
    const GETPGID: Self = Self(155);
    const SIGALTSTACK: Self = Self(132);
    const PPOLL: Self = Self(73);
    const FCNTL: Self = Self(25);
    const REBOOT: Self = Self(142); // busybox poweroff / reboot 使用此 syscall
}

impl core::fmt::Display for SyscallId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match *self {
            Self::GETCWD => "getcwd",
            Self::DUP => "dup",
            Self::DUP3 => "dup3",
            Self::IOCTL => "ioctl",
            Self::CHDIR => "chdir",
            Self::OPENAT => "openat",
            Self::CLOSE => "close",
            Self::GETDENTS64 => "getdents64",
            Self::LSEEK => "lseek",
            Self::READ => "read",
            Self::READV => "readv",
            Self::WRITE => "write",
            Self::WRITEV => "writev",
            Self::SENDFILE => "sendfile",
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
            Self::NANOSLEEP => "nanosleep",
            Self::FUTEX => "futex",
            Self::KILL => "kill",
            Self::SETPGID => "setpgid",
            Self::GETPGID => "getpgid",
            Self::SIGALTSTACK => "sigaltstack",
            Self::PPOLL => "ppoll",
            Self::FCNTL => "fcntl",
            Self::REBOOT => "reboot",
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

/// Linux special dirfd value: use current working directory.
const AT_FDCWD: isize = -100;

/// The persistent trap loop for a user process.
async fn run_tasks(task: Arc<Task>) {
    klog!(sched, debug, "run_tasks: starting pid={}", task.pid);

    // Capture top-level waker for async signal injection.
    SignalWakeHelper(&task).await;

    loop {
        // Activate per-process page table before signal delivery and returning to user mode.
        // sendsig writes the signal frame to the user stack via copy_user_chunk,
        // which requires the task's pmap to be active.
        {
            let mut pmap = task.pmap.lock();
            crate::mm::pmap::pmap_activate(&mut pmap);
        }

        // Check for pending signals before returning to user mode.
        match crate::proc::signal::check_pending_signals(&task) {
            Ok(_) => {} // signal delivered (or none pending), continue
            Err(sig) => {
                // fatal signal (SIGKILL or unhandled)
                // Linux wstatus for signal-killed: low 7 bits = signal number
                let wstatus = crate::proc::exit_wait::WaitStatus::signaled(sig);
                klog!(
                    signal,
                    trace,
                    "run_tasks: FATAL signal pid={} sig={} wstatus={:#x}",
                    task.pid,
                    crate::proc::signal::Signal::new_unchecked(sig),
                    wstatus.0
                );
                do_exit(&task, wstatus);
                break;
            }
        }

        klog!(
            sched,
            debug,
            "run_tasks: calling trap_return pid={}",
            task.pid
        );

        // DEBUG: dump trap frame a0 and wstatus before returning to userspace
        klog!(
            proc,
            trace,
            "PRE-SRET pid={} a0={:#x} sepc={:#x} sp={:#x} ra={:#x}",
            task.pid,
            task.trap_frame.lock().arg(0),
            task.trap_frame.lock().sepc,
            task.trap_frame.lock().sp(),
            task.trap_frame.lock().ra()
        );

        // Return to userspace. Blocks until user traps back.
        trap_return(&task);

        klog!(
            sched,
            debug,
            "run_tasks: returned from trap_return pid={}",
            task.pid
        );

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

/// Helper future that captures the executor waker into task.top_level_waker
/// on first poll, then immediately returns Ready.
struct SignalWakeHelper<'a>(&'a Task);

impl<'a> Future for SignalWakeHelper<'a> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        *self.0.top_level_waker.lock() = Some(cx.waker().clone());
        Poll::Ready(())
    }
}

/// Async trap handler: dispatches syscalls, page faults, and interrupts.
async fn user_trap_handler(task: &Arc<Task>) -> TrapResult {
    let scause;
    let stval;
    let sepc;
    {
        let tf = task.trap_frame.lock();
        scause = tf.scause;
        stval = tf.stval;
        sepc = tf.sepc;
    }

    let is_interrupt = scause & SCAUSE_INTERRUPT != 0;
    let code = scause & !SCAUSE_INTERRUPT;

    // Debug: log all traps for pid=1 after verbose flag is set
    klog!(
        trap,
        trace,
        "VERBOSE pid={} trap code={} sepc={:#x} stval={:#x}",
        task.pid,
        code,
        sepc,
        stval
    );

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
            // sepc advance is done inside dispatch_syscall (skipped for sigreturn)
            dispatch_syscall(task).await
        }
        EXC_LOAD_ACCESS_FAULT
        | EXC_STORE_ACCESS_FAULT
        | EXC_INST_ACCESS_FAULT
        | EXC_INST_PAGE_FAULT
        | EXC_LOAD_PAGE_FAULT
        | EXC_STORE_PAGE_FAULT => {
            let fault_va = VirtAddr::new(stval & !(PAGE_SIZE - 1));
            let access_type = match code {
                EXC_STORE_PAGE_FAULT | EXC_STORE_ACCESS_FAULT => PageFaultAccessType::WRITE,
                EXC_INST_PAGE_FAULT | EXC_INST_ACCESS_FAULT => PageFaultAccessType::EXECUTE,
                _ => PageFaultAccessType::READ,
            };

            // Try to resolve the fault first (demand paging). pcb_onfault is
            // only a fallback — if the page can be mapped, the CPU retries the
            // faulting instruction and the copy continues transparently.
            match resolve_user_fault(task, fault_va, access_type).await {
                Ok(_) => {
                    // Page resolved — return to the faulting instruction.
                    // pcb_onfault (if set) stays armed for future faults.
                    TrapResult::Continue
                }
                Err(e) => {
                    // Resolution failed. If pcb_onfault is set (copy_user_chunk),
                    // redirect to the EFAULT landing pad instead of killing.
                    let percpu = crate::executor::per_cpu::current();
                    let onfault = percpu
                        .pcb_onfault
                        .load(core::sync::atomic::Ordering::Relaxed);
                    if onfault != 0 {
                        task.trap_frame.lock().sepc = onfault;
                        percpu
                            .pcb_onfault
                            .store(0, core::sync::atomic::Ordering::Relaxed);
                        return TrapResult::Continue;
                    }
                    // No fixup — truly fatal user fault.
                    let pc = task.trap_frame.lock().sepc;
                    klog!(
                        trap,
                        trace,
                        "fatal fault: pid={} va={:#x} pc={:#x} code={} err={:?}",
                        task.pid,
                        stval,
                        pc,
                        code,
                        e
                    );
                    task.signals.post_signal(crate::proc::signal::SIGSEGV);
                    TrapResult::Continue
                }
            }
        }
        _ => {
            klog!(
                trap,
                trace,
                "unhandled exception: code={} sepc={:#x} stval={:#x}",
                code,
                { task.trap_frame.lock().sepc },
                stval
            );
            task.signals.post_signal(crate::proc::signal::SIGSEGV);
            TrapResult::Continue
        }
    }
}

/// Pre-fault all user pages covering [user_ptr, user_ptr+len).
pub(crate) async fn fault_in_user_buffer(
    task: &Arc<Task>,
    user_ptr: usize,
    len: usize,
    access: PageFaultAccessType,
) {
    if len == 0 {
        return;
    }
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
    // Fast path: if the page is already mapped with sufficient permissions, skip.
    // Must check PTE flags — not just presence — to avoid bypassing COW faults.
    {
        let pmap = task.pmap.lock();
        let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));
        if let Some((_pa, flags)) =
            crate::mm::pmap::pmap_extract_with_flags(&pmap, fault_va_aligned)
        {
            use crate::mm::pmap::pte::PteFlags;
            let mut ok = true;
            if access_type.write && !flags.contains(PteFlags::W) {
                ok = false; // COW page — must go through fault handler
            }
            if access_type.execute && !flags.contains(PteFlags::X) {
                ok = false;
            }
            if ok {
                return Ok(());
            }
        }
    }

    // 1. Sync path: anonymous zero-fill, COW
    let sync_result = {
        let vm_map = task.vm_map.lock();
        let mut pmap = task.pmap.lock();
        sync_fault_handler(&vm_map, &mut pmap, fault_va, access_type)
    };

    match sync_result {
        FaultResult::Resolved => Ok(()),
        FaultResult::NeedsAsyncIO => {
            klog!(
                vm,
                trace,
                "resolve_user_fault: NeedsAsyncIO pid={} va={:#x}",
                task.pid,
                fault_va.as_usize()
            );
            // 2. Async path: file-backed pages
            klog!(
                vm,
                trace,
                "resolve_user_fault: ENTERING async pid={} va={:#x}",
                task.pid,
                fault_va.as_usize()
            );
            let async_result = fault_in_page_async(task, fault_va).await;
            match &async_result {
                Ok(()) => klog!(
                    vm,
                    trace,
                    "resolve_user_fault: async OK pid={} va={:#x}",
                    task.pid,
                    fault_va.as_usize()
                ),
                Err(e) => klog!(
                    vm,
                    trace,
                    "resolve_user_fault: async FAILED pid={} va={:#x} err={:?}",
                    task.pid,
                    fault_va.as_usize(),
                    e
                ),
            }
            async_result
        }
        FaultResult::Error(e) => {
            klog!(
                vm,
                trace,
                "resolve_user_fault: sync FAILED pid={} va={:#x} err={:?}",
                task.pid,
                fault_va.as_usize(),
                e
            );
            Err(e)
        }
    }
}

/// Async file-backed page fault resolution with TOCTOU re-validation.
///
/// Also handles anonymous VMAs (stack, heap, BSS) that reach this path:
/// allocate a zeroed frame and map it directly.
async fn fault_in_page_async(task: &Arc<Task>, fault_va: VirtAddr) -> Result<(), FaultError> {
    // Time-of-check: snapshot VMA state under lock
    let (vnode_id, vnode_path, file_offset, file_size, _vma_file_offset, vma_start) = {
        let map = task.vm_map.lock();
        let vma = map.find_area(fault_va).ok_or(FaultError::NotMapped)?;
        let vnode = match vma.vnode.as_ref() {
            Some(v) => v,
            None => {
                // Anonymous VMA (stack, heap, BSS): allocate zeroed frame and map.
                let fault_va_aligned = VirtAddr::new(fault_va.as_usize() & !(PAGE_SIZE - 1));
                let obj_offset = ((fault_va_aligned.as_usize() - vma.range.start.as_usize())
                    / PAGE_SIZE) as u64
                    + vma.obj_offset;
                let prot = vma.prot;

                // TOCTOU: another core may have already resolved this fault.
                {
                    let obj = vma.object.read();
                    if let Some(existing) = obj.lookup_page(obj_offset) {
                        let mut pmap = task.pmap.lock();
                        if crate::mm::pmap::pmap_extract(&pmap, fault_va_aligned).is_none() {
                            let _ = crate::mm::pmap::pmap_enter(
                                &mut pmap,
                                fault_va_aligned,
                                existing,
                                prot,
                                false,
                            );
                        }
                        return Ok(());
                    }
                }

                let frame =
                    crate::mm::allocator::frame_alloc_sync().ok_or(FaultError::OutOfMemory)?;
                crate::mm::pmap::pmap_zero_page(frame);

                {
                    let mut obj = vma.object.write();
                    // Re-check under write lock to avoid double-insert race.
                    if let Some(existing) = obj.lookup_page(obj_offset) {
                        // Another core won — use their page, free ours.
                        drop(obj);
                        crate::mm::allocator::frame_free(frame);
                        let mut pmap = task.pmap.lock();
                        if crate::mm::pmap::pmap_extract(&pmap, fault_va_aligned).is_none() {
                            let _ = crate::mm::pmap::pmap_enter(
                                &mut pmap,
                                fault_va_aligned,
                                existing,
                                prot,
                                false,
                            );
                        }
                        return Ok(());
                    }
                    obj.insert_page(
                        obj_offset,
                        crate::mm::vm::vm_object::OwnedPage::new_anonymous(frame),
                    );
                }

                let mut pmap = task.pmap.lock();
                if crate::mm::pmap::pmap_extract(&pmap, fault_va_aligned).is_none() {
                    let _ = crate::mm::pmap::pmap_enter(
                        &mut pmap,
                        fault_va_aligned,
                        frame,
                        prot,
                        false,
                    );
                }
                return Ok(());
            }
        };
        let page_idx = (fault_va.as_usize() - vma.range.start.as_usize()) / PAGE_SIZE;
        let file_offset = vma.file_offset + (page_idx * PAGE_SIZE) as u64;
        // Debug: log file-backed fault details
        klog!(vm, trace, "fault_in_page_async pid={} va={:#x} vnode={} path={} file_offset={:#x} file_size={:#x} vma_start={:#x} page_idx={}",
            task.pid, fault_va.as_usize(), vnode.vnode_id(), vnode.path(), file_offset, vma.file_size, vma.range.start.as_usize(), page_idx);
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
        let frame = crate::mm::allocator::frame_alloc_sync().ok_or(FaultError::OutOfMemory)?;
        unsafe {
            core::ptr::write_bytes(frame.as_usize() as *mut u8, 0, PAGE_SIZE);
        }
        frame
    } else if vma_page_byte_offset + PAGE_SIZE as u64 > file_size {
        // Partial page: file data + zero fill for the rest
        let fetched = page_cache_fetch_by_id(vnode_id, &vnode_path, file_offset).await?;
        // Copy file portion to a new frame and zero the tail
        let frame = crate::mm::allocator::frame_alloc_sync().ok_or(FaultError::OutOfMemory)?;
        let file_bytes = (file_size - vma_page_byte_offset) as usize;
        unsafe {
            let src_slice = fetched.as_slice();
            let dst_slice = frame.as_mut_slice();
            dst_slice[..file_bytes].copy_from_slice(&src_slice[..file_bytes]);
            dst_slice[file_bytes..].fill(0);
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

        // Guard: if the page is already mapped, don't overwrite it.
        // This can happen when fault_in_user_buffer pre-faults a page that
        // was already demand-paged in by a real hardware fault.
        if let Some(_existing_pa) = crate::mm::pmap::pmap_extract(&pmap, fault_va_aligned) {
            // Free the frame we just allocated (BSS/partial paths) to avoid leak.
            // For the FILE path, `pa` came from the page cache — don't free it.
            if vma_page_byte_offset >= file_size
                || vma_page_byte_offset + PAGE_SIZE as u64 > file_size
            {
                crate::mm::allocator::frame_free(pa);
            }
            return Ok(());
        }

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
                PageCacheWaitFuture {
                    vnode_id,
                    page_offset,
                }
                .await;
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
    fn clone_fn(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_fn, noop, noop, noop);
    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) }
}

/// Async syscall dispatch.
async fn dispatch_syscall(task: &Arc<Task>) -> TrapResult {
    let (id, a0, a1, a2, a3, _a4, _a5) = {
        let tf = task.trap_frame.lock();
        (
            SyscallId(tf.x[17]),
            tf.x[10],
            tf.x[11],
            tf.x[12],
            tf.x[13],
            tf.x[14],
            tf.x[15],
        )
    };

    klog!(
        syscall,
        debug,
        "pid={} {} a0={:#x} a1={:#x} a2={:#x}",
        task.pid,
        id,
        a0,
        a1,
        a2
    );

    klog!(
        syscall,
        trace,
        "VERBOSE pid={} syscall {} a0={:#x} a1={:#x} a2={:#x}",
        task.pid,
        id,
        a0,
        a1,
        a2
    );

    let ret: usize = match id {
        // --- Fast-path synchronous syscalls ---
        SyscallId::GETPID => crate::proc::syscalls::sys_getpid(task),
        SyscallId::GETPPID => crate::proc::syscalls::sys_getppid(task),
        SyscallId::GETUID => crate::proc::syscalls::sys_getuid(task),
        SyscallId::GETEUID => crate::proc::syscalls::sys_geteuid(task),
        SyscallId::GETGID => crate::proc::syscalls::sys_getgid(task),
        SyscallId::GETEGID => crate::proc::syscalls::sys_getegid(task),
        SyscallId::GETTID => crate::proc::syscalls::sys_gettid(task),
        SyscallId::SET_TID_ADDRESS => crate::proc::syscalls::sys_gettid(task),
        SyscallId::DUP => match task.fd_table.lock().dup(a0 as u32) {
            Ok(fd) => fd as usize,
            Err(e) => (-(e.as_i32() as isize)) as usize,
        },
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
            use crate::mm::vm::vm_map::{VmArea, VmAreaType};
            use crate::mm::vm::vm_object::PageOwnership;
            use crate::mm::vm::vm_object::VmObject;

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
                    if let Some(heap_vma) =
                        vm.find_area_ending_at_mut(VirtAddr::new(old_brk), VmAreaType::Heap)
                    {
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
                            crate::map_perm!(R, W, U),
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
                    if let Some(heap_vma) = vm.find_area_mut(VirtAddr::new(old_brk - 1)) {
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
                task.brk
                    .store(new_brk, core::sync::atomic::Ordering::Relaxed);
                new_brk
            }
        }
        SyscallId::MMAP => sys_mmap(task, a0, a1, a2, a3, _a4 as u32, _a5 as u64),
        SyscallId::MUNMAP => sys_munmap(task, a0, a1),
        SyscallId::MPROTECT => sys_mprotect(task, a0, a1, a2),
        SyscallId::SIGACTION => {
            // a0=signum, a1=act, a2=oldact
            // Pre-fault user buffers for the synchronous copy_user_chunk calls
            if a1 != 0 {
                fault_in_user_buffer(task, a1, 32, PageFaultAccessType::READ).await;
            }
            if a2 != 0 {
                fault_in_user_buffer(task, a2, 32, PageFaultAccessType::WRITE).await;
            }
            match crate::proc::syscalls::sys_sigaction(task, a0, a1, a2) {
                Ok(v) => v,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::SIGPROCMASK => {
            // a0=how, a1=set, a2=oldset, a3=sigsetsize
            // Pre-fault user buffers for the synchronous copy_user_chunk calls
            if a1 != 0 {
                fault_in_user_buffer(task, a1, 8, PageFaultAccessType::READ).await;
            }
            if a2 != 0 {
                fault_in_user_buffer(task, a2, 8, PageFaultAccessType::WRITE).await;
            }
            match crate::proc::syscalls::sys_sigprocmask(task, a0, a1, a2) {
                Ok(v) => v,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::SIGRETURN => {
            let _ = crate::proc::syscalls::sys_sigreturn(task);
            // sigreturn sets sepc/sstatus directly; don't overwrite a0
            return TrapResult::Continue;
        }
        SyscallId::KILL => {
            // a0=pid (or -pgid), a1=sig
            match crate::proc::syscalls::sys_kill(task, a0 as isize, a1 as u8) {
                Ok(v) => v,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::SETPGID => {
            // a0=pid, a1=pgid
            match crate::proc::syscalls::sys_setpgid(task, a0 as u32, a1 as u32) {
                Ok(v) => v,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::GETPGID => {
            // a0=pid
            match crate::proc::syscalls::sys_getpgid(task, a0 as u32) {
                Ok(v) => v,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::SIGALTSTACK => {
            // a0=ss, a1=old_ss — stub for now (accept silently)
            0
        }
        SyscallId::CLOCK_GETTIME => {
            // a0=clockid, a1=timespec pointer
            match sys_clock_gettime(task, a0 as u32, a1) {
                Ok(()) => 0,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::NANOSLEEP => {
            // a0=req timespec, a1=rem timespec (may be null)
            match sys_nanosleep_async(task, a0, a1).await {
                Ok(()) => 0,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::TIMES => 0, // stub
        SyscallId::UNAME => {
            // a0 = pointer to struct utsname (6 fields × 65 bytes each = 390 bytes)
            match sys_uname(task, a0) {
                Ok(()) => 0,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::IOCTL => {
            // a0=fd, a1=request, a2=argp
            match sys_ioctl_async(task, a0 as u32, a1, a2).await {
                Ok(v) => v as usize,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }

        SyscallId::GETCWD => {
            // a0=buf, a1=size
            match sys_getcwd(task, a0, a1) {
                Ok(v) => v,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::FCNTL => {
            // a0=fd, a1=cmd, a2=arg
            match sys_fcntl(task, a0 as u32, a1 as u32, a2) {
                Ok(v) => v,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }

        // --- Async syscalls ---
        SyscallId::WRITEV => {
            // a0=fd, a1=iov, a2=iovcnt
            match sys_writev_async(task, a0 as u32, a1, a2).await {
                Ok(n) => n,
                Err(Errno::EINTR) => {
                    if should_restart_syscall(task) {
                        return TrapResult::Continue; // re-execute ecall with original args
                    }
                    (-(Errno::EINTR.as_i32() as isize)) as usize
                }
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::PPOLL => {
            // a0=fds, a1=nfds, a2=timeout_ts, a3=sigmask
            match sys_ppoll_async(task, a0, a1, a2).await {
                Ok(n) => n,
                Err(Errno::EINTR) => {
                    if should_restart_syscall(task) {
                        return TrapResult::Continue;
                    }
                    (-(Errno::EINTR.as_i32() as isize)) as usize
                }
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::WRITE => {
            // a0=fd, a1=buf, a2=len
            match sys_write_async(task, a0 as u32, a1, a2).await {
                Ok(n) => n,
                Err(Errno::EINTR) => {
                    if should_restart_syscall(task) {
                        return TrapResult::Continue;
                    }
                    (-(Errno::EINTR.as_i32() as isize)) as usize
                }
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::READ => {
            // a0=fd, a1=user_buf, a2=len
            match sys_read_async(task, a0 as u32, a1, a2).await {
                Ok(n) => n,
                Err(Errno::EINTR) => {
                    if should_restart_syscall(task) {
                        return TrapResult::Continue;
                    }
                    (-(Errno::EINTR.as_i32() as isize)) as usize
                }
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::READV => {
            // a0=fd, a1=iov, a2=iovcnt
            match sys_readv_async(task, a0 as u32, a1, a2).await {
                Ok(n) => n,
                Err(Errno::EINTR) => {
                    if should_restart_syscall(task) {
                        return TrapResult::Continue;
                    }
                    (-(Errno::EINTR.as_i32() as isize)) as usize
                }
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::SENDFILE => {
            // Signal to Busybox `cat` that it should fall back to read/write for pipes.
            (-(Errno::EINVAL.as_i32() as isize)) as usize
        }
        SyscallId::OPENAT => {
            // a0=dirfd, a1=pathname, a2=flags, a3=mode
            match sys_openat_async(task, a0 as isize, a1, a2).await {
                Ok(fd) => fd as usize,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::CHDIR => {
            // a0=pathname
            match sys_chdir_async(task, a0).await {
                Ok(()) => 0,
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
            // a0=fd, a1=statbuf
            match sys_fstat(task, a0 as u32, a1) {
                Ok(()) => 0,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::LSEEK => {
            // a0=fd, a1=offset, a2=whence
            match sys_lseek(task, a0 as u32, a1 as i64, a2 as u32) {
                Ok(off) => off as usize,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::EXIT | SyscallId::EXIT_GROUP => {
            crate::proc::syscalls::sys_exit(task, a0 as i32);
            return TrapResult::Exit;
        }
        SyscallId::REBOOT => {
            // busybox poweroff/reboot 触发此 syscall，直接调用 SBI 关机
            // a0=magic1, a1=magic2, a2=cmd: 无论参数，只要 cmd!=0 就关机
            klog!(syscall, info, "reboot syscall: shutting down (cmd={:#x})", a2);
            crate::hal::rv64::sbi::shutdown();
            0
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
            let raw_path = match copyinstr(task, a0, 256).await {
                Some(s) => s,
                None => return set_syscall_ret(task, (-(Errno::EFAULT.as_i32() as isize)) as usize),
            };
            let path = match absolutize_path(task, AT_FDCWD, &raw_path) {
                Ok(p) => p,
                Err(e) => return set_syscall_ret(task, (-(e.as_i32() as isize)) as usize),
            };
            // Read argv array from user memory (before exec destroys address space)
            let mut argv = copyin_argv(task, a1, 64, 4096).await;
            // Read envp array (optional, musl can cope with empty)
            let envp = copyin_argv(task, a2, 64, 4096).await;

            // .sh 脚本：参考 Chronix，重定向到 busybox sh 执行
            let exec_path = if path.ends_with(".sh") {
                #[cfg(feature = "autotest")]
                let busybox = alloc::string::String::from("/riscv/musl/busybox");
                #[cfg(not(feature = "autotest"))]
                let busybox = alloc::string::String::from("/bin/busybox");
                argv.insert(0, busybox.clone());
                argv.insert(1, alloc::string::String::from("sh"));
                busybox
            } else {
                path.clone()
            };

            match crate::proc::exec::exec_with_args(task, &exec_path, &argv, &envp).await {
                Ok((entry, sp)) => {
                    let mut tf = task.trap_frame.lock();
                    tf.sepc = entry;
                    tf.x[2] = sp;
                    for i in 1..32 {
                        if i != 2 {
                            tf.x[i] = 0;
                        }
                    }
                    tf.sstatus = (1 << 5) | (1 << 13); // SPP=0, SPIE=1, FS=Initial
                    return TrapResult::Continue;
                }
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::WAIT4 => {
            let wstatus_ptr = a1;
            let options = a2;
            // Debug: dump wstatus at user pointer before wait4
            if a1 != 0 {
                let peek = unsafe { *(a1 as *const i32) };
                klog!(
                    proc,
                    trace,
                    "VERBOSE wait4 ENTRY pid={} wstatus@{:#x}={:#x}",
                    task.pid,
                    a1,
                    peek
                );
            }
            match crate::proc::syscalls::sys_wait4_async(task, a0 as isize, wstatus_ptr, options)
                .await
            {
                Ok(pid) => pid as usize,
                // wait4 is never restarted (matches Linux behavior).
                // The SIGCHLD handler typically calls waitpid() itself,
                // so restarting would find no children and loop with ECHILD.
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::FUTEX => {
            // a0=uaddr, a1=op, a2=val, a3=timeout/val2, a4=uaddr2, a5=val3
            match sys_futex_async(task, a0, a1 as u32, a2 as u32).await {
                Ok(v) => v as usize,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        SyscallId::FSTATAT => {
            // a0=dirfd, a1=pathname, a2=statbuf, a3=flags
            match sys_fstatat_async(task, a0 as isize, a1, a2).await {
                Ok(()) => 0,
                Err(e) => (-(e.as_i32() as isize)) as usize,
            }
        }
        _ => {
            klog!(
                syscall,
                error,
                "unimplemented {} (a0={:#x} a1={:#x} a2={:#x})",
                id,
                a0,
                a1,
                a2
            );
            (-38isize) as usize // ENOSYS
        }
    };

    {
        klog!(syscall, debug, "pid={} {} -> {:#x}", task.pid, id, ret);
        klog!(
            syscall,
            trace,
            "VERBOSE pid={} {} -> ret={:#x}",
            task.pid,
            id,
            ret
        );
        let mut tf = task.trap_frame.lock();
        tf.advance_pc(); // advance past ecall
        tf.set_ret_val(ret);
    }
    TrapResult::Continue
}

// ---------------------------------------------------------------------------
// mmap / munmap / mprotect
// ---------------------------------------------------------------------------

/// Free anonymous frames from removed VMAs.
fn free_removed_frames(removed: alloc::vec::Vec<crate::mm::vm::vm_map::VmArea>) {
    use crate::mm::vm::vm_object::PageOwnership;
    for vma in removed {
        let obj = vma.object.read();
        for page in obj.pages_iter() {
            if matches!(page.ownership, PageOwnership::Anonymous) {
                crate::mm::allocator::frame_free(page.phys);
            }
        }
    }
}

/// sys_mmap: real mmap with top-down allocation and MAP_FIXED.
fn sys_mmap(
    task: &Arc<Task>,
    addr: usize,
    len: usize,
    prot_bits: usize,
    flags: usize,
    _fd: u32,
    _offset: u64,
) -> usize {
    use crate::mm::vm::vm_map::{MapPerm, VmArea, VmAreaType};
    use crate::mm::vm::vm_object::VmObject;

    let aligned_len = (len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    if aligned_len == 0 {
        return (-(Errno::EINVAL.as_i32() as isize)) as usize;
    }

    let map_fixed = flags & 0x10 != 0; // MAP_FIXED
    let _map_anon = flags & 0x20 != 0; // MAP_ANONYMOUS
    let _map_private = flags & 0x02 != 0; // MAP_PRIVATE

    let mut vm = task.vm_map.lock();

    let base = if map_fixed {
        let start = VirtAddr::new(addr & !0xFFF);
        let end = VirtAddr::new(start.as_usize() + aligned_len);
        // MAP_FIXED: delete existing mappings in range first
        let removed = vm.remove_range(start, end);
        // Tear down PTEs for removed range
        {
            let mut pmap = task.pmap.lock();
            crate::mm::pmap::pmap_remove(&mut pmap, start, end);
        }
        // Free anonymous frames from removed VMAs
        free_removed_frames(removed);
        start.as_usize()
    } else if addr != 0 {
        // Hint address: try it, fall back to top-down
        let hint = VirtAddr::new(addr & !0xFFF);
        let hint_end = VirtAddr::new(hint.as_usize() + aligned_len);
        // Check if hint range is free
        let hint_ok = vm.find_area(hint).is_none()
            && vm
                .find_area(VirtAddr::new(hint_end.as_usize().saturating_sub(1)))
                .is_none();
        if hint_ok {
            hint.as_usize()
        } else {
            match vm.find_free_area_topdown(aligned_len) {
                Some(va) => va.as_usize(),
                None => return (-(Errno::ENOMEM.as_i32() as isize)) as usize,
            }
        }
    } else {
        // Top-down allocation
        match vm.find_free_area_topdown(aligned_len) {
            Some(va) => va.as_usize(),
            None => return (-(Errno::ENOMEM.as_i32() as isize)) as usize,
        }
    };

    // Build VMA
    let mut perm = MapPerm::U;
    if prot_bits & 1 != 0 {
        perm |= MapPerm::R;
    }
    if prot_bits & 2 != 0 {
        perm |= MapPerm::W;
    }
    if prot_bits & 4 != 0 {
        perm |= MapPerm::X;
    }

    let obj = VmObject::new(aligned_len / PAGE_SIZE);
    let vma = VmArea::new(
        VirtAddr::new(base)..VirtAddr::new(base + aligned_len),
        perm,
        obj,
        0,
        VmAreaType::Anonymous,
    );
    match vm.insert(vma) {
        Ok(()) => base,
        Err(_) => (-(Errno::ENOMEM.as_i32() as isize)) as usize,
    }
}

/// sys_munmap: tear down PTEs + TLB + remove/split VMAs.
fn sys_munmap(task: &Arc<Task>, addr: usize, len: usize) -> usize {
    let aligned_start = VirtAddr::new(addr & !0xFFF);
    let aligned_end = VirtAddr::new((addr + len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1));
    if aligned_start >= aligned_end {
        return (-(Errno::EINVAL.as_i32() as isize)) as usize;
    }
    let mut vm = task.vm_map.lock();
    let removed = vm.remove_range(aligned_start, aligned_end);
    {
        let mut pmap = task.pmap.lock();
        crate::mm::pmap::pmap_remove(&mut pmap, aligned_start, aligned_end);
    }
    free_removed_frames(removed);
    0
}

/// sys_mprotect: change VMA permissions + update PTEs.
fn sys_mprotect(task: &Arc<Task>, addr: usize, len: usize, prot_bits: usize) -> usize {
    use crate::mm::vm::vm_map::MapPerm;

    let start = VirtAddr::new(addr & !0xFFF);
    let end = VirtAddr::new((addr + len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1));
    if start >= end {
        return (-(Errno::EINVAL.as_i32() as isize)) as usize;
    }

    let mut perm = MapPerm::U;
    if prot_bits & 1 != 0 {
        perm |= MapPerm::R;
    }
    if prot_bits & 2 != 0 {
        perm |= MapPerm::W;
    }
    if prot_bits & 4 != 0 {
        perm |= MapPerm::X;
    }

    let mut vm = task.vm_map.lock();
    vm.protect_range(start, end, perm);
    {
        let mut pmap = task.pmap.lock();
        crate::mm::pmap::pmap_protect(&mut pmap, start, end, perm);
    }
    0
}

// ---------------------------------------------------------------------------
// lseek / fstat / clock_gettime / nanosleep / futex
// ---------------------------------------------------------------------------

/// sys_lseek: move file offset. ESPIPE on pipes.
fn sys_lseek(task: &Arc<Task>, fd: u32, offset: i64, whence: u32) -> Result<u64, Errno> {
    use crate::fs::fd_table::FileObject;
    use core::sync::atomic::Ordering;

    const SEEK_SET: u32 = 0;
    const SEEK_CUR: u32 = 1;
    const SEEK_END: u32 = 2;

    let tab = task.fd_table.lock();
    let desc = tab.get(fd).ok_or(Errno::EBADF)?;

    // Pipes and devices are not seekable
    match &desc.object {
        FileObject::PipeRead(_) | FileObject::PipeWrite(_) => return Err(Errno::ESPIPE),
        FileObject::Device(_) => return Err(Errno::ESPIPE),
        FileObject::Vnode(_) => {}
    }

    let size = match &desc.object {
        FileObject::Vnode(v) => v.size(),
        _ => 0,
    };

    let cur = desc.offset.load(Ordering::Relaxed) as i64;
    let new_off = match whence {
        SEEK_SET => offset,
        SEEK_CUR => cur + offset,
        SEEK_END => size as i64 + offset,
        _ => return Err(Errno::EINVAL),
    };

    if new_off < 0 {
        return Err(Errno::EINVAL);
    }

    desc.offset.store(new_off as u64, Ordering::Relaxed);
    Ok(new_off as u64)
}

/// Linux struct stat for rv64 (128 bytes).
#[repr(C)]
struct LinuxStat {
    st_dev: u64,
    st_ino: u64,
    st_mode: u32,
    st_nlink: u32,
    st_uid: u32,
    st_gid: u32,
    st_rdev: u64,
    __pad1: u64,
    st_size: i64,
    st_blksize: i32,
    __pad2: i32,
    st_blocks: i64,
    st_atime: i64,
    st_atime_nsec: i64,
    st_mtime: i64,
    st_mtime_nsec: i64,
    st_ctime: i64,
    st_ctime_nsec: i64,
    __unused: [i32; 2],
}

/// sys_fstat: write stat struct to user memory.
fn sys_fstat(task: &Arc<Task>, fd: u32, statbuf: usize) -> Result<(), Errno> {
    use crate::fs::fd_table::FileObject;

    if statbuf == 0 {
        return Err(Errno::EFAULT);
    }

    let tab = task.fd_table.lock();
    let desc = tab.get(fd).ok_or(Errno::EBADF)?;

    let mut st = LinuxStat {
        st_dev: 0,
        st_ino: 0,
        st_mode: 0,
        st_nlink: 1,
        st_uid: 0,
        st_gid: 0,
        st_rdev: 0,
        __pad1: 0,
        st_size: 0,
        st_blksize: 4096,
        __pad2: 0,
        st_blocks: 0,
        st_atime: 0,
        st_atime_nsec: 0,
        st_mtime: 0,
        st_mtime_nsec: 0,
        st_ctime: 0,
        st_ctime_nsec: 0,
        __unused: [0; 2],
    };

    match &desc.object {
        FileObject::Vnode(v) => {
            let size = v.size();
            st.st_size = size as i64;
            st.st_blocks = ((size + 511) / 512) as i64;
            st.st_ino = v.vnode_id();
            // S_IFREG=0o100000 or S_IFDIR=0o040000
            use crate::fs::vnode::VnodeType;
            st.st_mode = match v.vtype() {
                VnodeType::Regular => 0o100644,
                VnodeType::Directory => 0o040755,
            };
        }
        FileObject::PipeRead(_) | FileObject::PipeWrite(_) => {
            st.st_mode = 0o010600; // S_IFIFO | rw
        }
        FileObject::Device(dk) => {
            use crate::fs::fd_table::DeviceKind;
            st.st_mode = 0o020666; // S_IFCHR | rw
            st.st_rdev = match dk {
                DeviceKind::Null => 0x0103,                                   // 1:3
                DeviceKind::Zero => 0x0105,                                   // 1:5
                DeviceKind::ConsoleRead | DeviceKind::ConsoleWrite => 0x0501, // 5:1
            };
        }
    }

    // Copy stat struct to user memory
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            statbuf as *mut u8,
            &st as *const LinuxStat as *const u8,
            core::mem::size_of::<LinuxStat>(),
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// sys_fstatat: stat a file by path (relative to dirfd).
async fn sys_fstatat_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    statbuf: usize,
) -> Result<(), Errno> {
    if statbuf == 0 {
        return Err(Errno::EFAULT);
    }

    let raw_path = copyinstr(task, pathname_ptr, 256).await.ok_or(Errno::EFAULT)?;
    let path_str = absolutize_path(task, dirfd, &raw_path)?;

    // Resolve the path to a vnode
    let vnode = crate::fs::path::resolve(&path_str).await?;

    let mut st = LinuxStat {
        st_dev: 0,
        st_ino: 0,
        st_mode: 0,
        st_nlink: 1,
        st_uid: 0,
        st_gid: 0,
        st_rdev: 0,
        __pad1: 0,
        st_size: 0,
        st_blksize: 4096,
        __pad2: 0,
        st_blocks: 0,
        st_atime: 0,
        st_atime_nsec: 0,
        st_mtime: 0,
        st_mtime_nsec: 0,
        st_ctime: 0,
        st_ctime_nsec: 0,
        __unused: [0; 2],
    };

    st.st_ino = vnode.vnode_id();
    st.st_size = vnode.size() as i64;
    st.st_blocks = ((vnode.size() + 511) / 512) as i64;
    use crate::fs::vnode::VnodeType;
    st.st_mode = match vnode.vtype() {
        VnodeType::Regular => 0o100755, // executable
        VnodeType::Directory => 0o040755,
    };

    fault_in_user_buffer(
        task,
        statbuf,
        core::mem::size_of::<LinuxStat>(),
        PageFaultAccessType::WRITE,
    )
    .await;
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            statbuf as *mut u8,
            &st as *const LinuxStat as *const u8,
            core::mem::size_of::<LinuxStat>(),
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// Read rdtime CSR (RISC-V cycle counter used as time source).
fn read_rdtime() -> u64 {
    let val: u64;
    unsafe {
        core::arch::asm!("rdtime {}", out(reg) val);
    }
    val
}

/// Timer frequency: QEMU virt = 10 MHz.
const TIMER_FREQ: u64 = 10_000_000;

/// sys_clock_gettime: read hardware time via rdtime CSR.
fn sys_clock_gettime(task: &Arc<Task>, _clockid: u32, tp: usize) -> Result<(), Errno> {
    let _ = task; // used for user memory access context
    if tp == 0 {
        return Err(Errno::EFAULT);
    }

    let ticks = read_rdtime();
    let secs = ticks / TIMER_FREQ;
    let nsecs = (ticks % TIMER_FREQ) * (1_000_000_000 / TIMER_FREQ);

    // struct timespec { time_t tv_sec; long tv_nsec; } — 16 bytes on rv64
    let ts: [u64; 2] = [secs, nsecs];
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(tp as *mut u8, ts.as_ptr() as *const u8, 16)
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// sys_nanosleep: async sleep via timer wheel.
async fn sys_nanosleep_async(
    task: &Arc<Task>,
    req_ptr: usize,
    rem_ptr: usize,
) -> Result<(), Errno> {
    if req_ptr == 0 {
        return Err(Errno::EFAULT);
    }

    // Read struct timespec from user memory
    let mut ts = [0u64; 2];
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            ts.as_mut_ptr() as *mut u8,
            req_ptr as *const u8,
            16,
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }
    let secs = ts[0];
    let nsecs = ts[1];
    let total_ms = secs * 1000 + nsecs / 1_000_000;

    if total_ms == 0 {
        // Zero sleep: just yield
        yield_now().await;
        return Ok(());
    }

    // Interruptible sleep: poll in 10ms increments to check signals
    let start = crate::hal::rv64::timer::read_time_ms();
    let deadline = start + total_ms;

    loop {
        // Check for pending signals
        if task.signals.has_actionable_pending() {
            // Write remaining time to rem pointer
            if rem_ptr != 0 {
                let now = crate::hal::rv64::timer::read_time_ms();
                let remaining_ms = deadline.saturating_sub(now);
                let rem_secs = remaining_ms / 1000;
                let rem_nsecs = (remaining_ms % 1000) * 1_000_000;
                let rem_ts = [rem_secs, rem_nsecs];
                let _ = unsafe {
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        rem_ptr as *mut u8,
                        rem_ts.as_ptr() as *const u8,
                        16,
                    )
                };
            }
            return Err(Errno::EINTR);
        }

        let now = crate::hal::rv64::timer::read_time_ms();
        if now >= deadline {
            break;
        }

        // Sleep in small increments (max 10ms) to stay responsive to signals
        let remaining = deadline - now;
        let chunk = core::cmp::min(remaining, 10);
        super::schedule::sleep(chunk).await;
    }

    // On normal completion, write zero remaining time
    if rem_ptr != 0 {
        let zero_ts = [0u64; 2];
        let _ = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                rem_ptr as *mut u8,
                zero_ts.as_ptr() as *const u8,
                16,
            )
        };
    }
    Ok(())
}

/// sys_futex: FUTEX_WAIT and FUTEX_WAKE.
async fn sys_futex_async(
    task: &Arc<Task>,
    uaddr: usize,
    op: u32,
    val: u32,
) -> Result<usize, Errno> {
    const FUTEX_WAIT: u32 = 0;
    const FUTEX_WAKE: u32 = 1;
    // Mask out FUTEX_PRIVATE_FLAG (128) and FUTEX_CLOCK_REALTIME (256)
    let cmd = op & 0x7f;

    match cmd {
        FUTEX_WAIT => {
            // Read current value at uaddr
            let current = unsafe { core::ptr::read_volatile(uaddr as *const u32) };
            if current != val {
                // Value changed — don't sleep
                return Err(Errno::EAGAIN);
            }
            // Resolve physical address for futex key
            let pa = {
                let pmap = task.pmap.lock();
                crate::mm::pmap::pmap_extract(&pmap, VirtAddr::new(uaddr & !0xFFF))
                    .ok_or(Errno::EFAULT)?
            };
            let pa_key = hal_common::PhysAddr::new(pa.as_usize() + (uaddr & 0xFFF));
            // Park on the futex
            crate::ipc::futex::futex_wait(pa_key, task).await?;
            Ok(0)
        }
        FUTEX_WAKE => {
            // Resolve physical address for futex key
            let pa = {
                let pmap = task.pmap.lock();
                crate::mm::pmap::pmap_extract(&pmap, VirtAddr::new(uaddr & !0xFFF))
                    .ok_or(Errno::EFAULT)?
            };
            let pa_key = hal_common::PhysAddr::new(pa.as_usize() + (uaddr & 0xFFF));
            let woken = crate::ipc::futex::futex_wake(pa_key, val as usize);
            Ok(woken)
        }
        _ => Err(Errno::ENOSYS),
    }
}

/// sys_read through the three-layer FD model.
async fn sys_read_async(
    task: &Arc<Task>,
    fd: u32,
    user_buf: usize,
    len: usize,
) -> Result<usize, Errno> {
    use crate::fs::fd_table::{DeviceKind, FileObject};
    use crate::mm::uio::{uiomove, UioDir};

    if len == 0 {
        return Ok(0);
    }

    // Snapshot what we need from the fd table
    enum ReadSource {
        Vnode {
            id: u64,
            path: String,
            size: u64,
            offset: u64,
        },
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
                    user_buf as *mut u8,
                    zeros.as_ptr(),
                    len,
                )
            };
            if rc != 0 {
                Err(Errno::EFAULT)
            } else {
                Ok(len)
            }
        }
        ReadSource::DevConsole => {
            ConsoleReadFuture {
                task,
                user_buf,
                len,
            }
            .await
        }
        ReadSource::PipeRead(pipe) => {
            // Async pipe read: loop until data available or EOF
            PipeReadFuture {
                pipe,
                task,
                user_buf,
                len,
            }
            .await
        }
        ReadSource::Vnode {
            id,
            path,
            size,
            mut offset,
        } => {
            if offset >= size {
                return Ok(0);
            }
            let to_read = core::cmp::min(len, (size - offset) as usize);
            let mut total = 0usize;

            while total < to_read {
                let page_offset = offset / PAGE_SIZE as u64;
                let offset_in_page = (offset % PAGE_SIZE as u64) as usize;
                let chunk = core::cmp::min(PAGE_SIZE - offset_in_page, to_read - total);

                let pa = page_cache_fetch_by_id(id, &path, page_offset * PAGE_SIZE as u64)
                    .await
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
                        )
                        .await
                        .map_err(|_| Errno::EFAULT)?;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }

            desc.offset
                .store(offset, core::sync::atomic::Ordering::Relaxed);
            Ok(total)
        }
    }
}

/// sys_readv: read into multiple user buffers (iovecs)
async fn sys_readv_async(
    task: &Arc<Task>,
    fd: u32,
    iov_ptr: usize,
    iovcnt: usize,
) -> Result<usize, Errno> {
    if iovcnt > 1024 {
        return Err(Errno::EINVAL);
    }
    if iovcnt == 0 {
        return Ok(0);
    }

    let iov_size = iovcnt * 16;
    fault_in_user_buffer(task, iov_ptr, iov_size, PageFaultAccessType::READ).await;
    let mut iov_buf = alloc::vec![0u8; iov_size];
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            iov_buf.as_mut_ptr(),
            iov_ptr as *const u8,
            iov_size,
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }

    let mut total_read = 0;
    for i in 0..iovcnt {
        let off = i * 16;
        let base = usize::from_le_bytes(iov_buf[off..off + 8].try_into().unwrap());
        let len = usize::from_le_bytes(iov_buf[off + 8..off + 16].try_into().unwrap());
        if len == 0 {
            continue;
        }

        match sys_read_async(task, fd, base, len).await {
            Ok(0) => break, // EOF
            Ok(n) => {
                total_read += n;
                if n < len {
                    // Short read (e.g. pipe empty after reading some bytes)
                    break;
                }
            }
            Err(e) => {
                if total_read > 0 {
                    return Ok(total_read);
                }
                return Err(e);
            }
        }
    }
    Ok(total_read)
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
        // EINTR guard: check for pending signals before blocking
        if this.task.signals.has_actionable_pending() {
            return Poll::Ready(Err(Errno::EINTR));
        }
        let mut kbuf = alloc::vec![0u8; this.len];
        match this.pipe.read(&mut kbuf) {
            Ok(0) => Poll::Ready(Ok(0)), // EOF
            Ok(n) => {
                // Copy to user buffer (best-effort, fault in pages first would be ideal)
                let rc = unsafe {
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        this.user_buf as *mut u8,
                        kbuf.as_ptr(),
                        n,
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

struct ConsoleReadFuture<'a> {
    task: &'a Arc<Task>,
    user_buf: usize,
    len: usize,
}

impl<'a> core::future::Future for ConsoleReadFuture<'a> {
    type Output = Result<usize, Errno>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // EINTR guard
        if this.task.signals.has_actionable_pending() {
            return Poll::Ready(Err(Errno::EINTR));
        }
        let mut kbuf = alloc::vec![0u8; this.len];
        let n = crate::console::console_read(&mut kbuf);
        if n > 0 {
            let rc = unsafe {
                crate::hal::rv64::copy_user::copy_user_chunk(
                    this.user_buf as *mut u8,
                    kbuf.as_ptr(),
                    n,
                )
            };
            if rc != 0 {
                Poll::Ready(Err(Errno::EFAULT))
            } else {
                Poll::Ready(Ok(n))
            }
        } else {
            crate::console::console_register_waker(cx.waker());
            Poll::Pending
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
    use crate::fs::fd_table::{DeviceKind, FileObject};

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
            FileObject::Device(DeviceKind::ConsoleWrite)
            | FileObject::Device(DeviceKind::ConsoleRead) => WriteTarget::DevConsole,
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
                    kbuf.as_mut_ptr(),
                    user_buf as *const u8,
                    len,
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
                    kbuf.as_mut_ptr(),
                    user_buf as *const u8,
                    len,
                )
            };
            if rc != 0 {
                return Err(Errno::EFAULT);
            }
            let result = PipeWriteFuture {
                pipe,
                task,
                data: kbuf,
                written: 0,
            }
            .await;
            // Post SIGPIPE on broken pipe
            if let Err(Errno::EPIPE) = &result {
                task.signals.post_signal(crate::proc::signal::SIGPIPE);
            }
            result
        }
    }
}

/// Future for async pipe write.
struct PipeWriteFuture<'a> {
    pipe: alloc::sync::Arc<crate::fs::pipe::Pipe>,
    task: &'a Arc<Task>,
    data: alloc::vec::Vec<u8>,
    written: usize,
}

impl<'a> core::future::Future for PipeWriteFuture<'a> {
    type Output = Result<usize, Errno>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        // EINTR guard: check for pending signals before blocking
        if this.task.signals.has_actionable_pending() {
            // Return partial write if any, else EINTR
            if this.written > 0 {
                return Poll::Ready(Ok(this.written));
            }
            return Poll::Ready(Err(Errno::EINTR));
        }
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

// ---------------------------------------------------------------------------
// writev
// ---------------------------------------------------------------------------

/// sys_writev: gather-write from an iovec array.
async fn sys_writev_async(
    task: &Arc<Task>,
    fd: u32,
    iov_ptr: usize,
    iovcnt: usize,
) -> Result<usize, Errno> {
    if iovcnt == 0 {
        return Ok(0);
    }
    if iovcnt > 1024 {
        return Err(Errno::EINVAL);
    }

    // Read iovec array from user memory (each entry: *const u8, usize = 16 bytes on rv64)
    let iov_size = iovcnt * 16;
    fault_in_user_buffer(task, iov_ptr, iov_size, PageFaultAccessType::READ).await;
    let mut iov_buf = alloc::vec![0u8; iov_size];
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            iov_buf.as_mut_ptr(),
            iov_ptr as *const u8,
            iov_size,
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }

    let mut total = 0usize;
    for i in 0..iovcnt {
        let off = i * 16;
        let base = usize::from_le_bytes(iov_buf[off..off + 8].try_into().unwrap());
        let len = usize::from_le_bytes(iov_buf[off + 8..off + 16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        match sys_write_async(task, fd, base, len).await {
            Ok(n) => {
                total += n;
                if n < len {
                    break; // short write
                }
            }
            Err(e) => {
                if total > 0 {
                    return Ok(total);
                }
                return Err(e);
            }
        }
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// ppoll
// ---------------------------------------------------------------------------

/// sys_ppoll: poll file descriptors with timeout.
async fn sys_ppoll_async(
    task: &Arc<Task>,
    fds_ptr: usize,
    nfds: usize,
    timeout_ptr: usize,
) -> Result<usize, Errno> {
    use crate::fs::fd_table::{DeviceKind, FileObject};

    if nfds > 256 {
        return Err(Errno::EINVAL);
    }

    // struct pollfd { int fd; short events; short revents; } = 8 bytes
    let poll_size = nfds * 8;
    if nfds > 0 {
        fault_in_user_buffer(task, fds_ptr, poll_size, PageFaultAccessType::READ).await;
    }

    let mut poll_buf = alloc::vec![0u8; poll_size];
    if nfds > 0 {
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                poll_buf.as_mut_ptr(),
                fds_ptr as *const u8,
                poll_size,
            )
        };
        if rc != 0 {
            return Err(Errno::EFAULT);
        }
    }

    // Parse timeout
    let timeout_ms: Option<u64> = if timeout_ptr != 0 {
        fault_in_user_buffer(task, timeout_ptr, 16, PageFaultAccessType::READ).await;
        let mut ts_buf = [0u8; 16];
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                ts_buf.as_mut_ptr(),
                timeout_ptr as *const u8,
                16,
            )
        };
        if rc != 0 {
            return Err(Errno::EFAULT);
        }
        let sec = i64::from_le_bytes(ts_buf[0..8].try_into().unwrap());
        let nsec = i64::from_le_bytes(ts_buf[8..16].try_into().unwrap());
        if sec < 0 || nsec < 0 {
            return Err(Errno::EINVAL);
        }
        Some(sec as u64 * 1000 + nsec as u64 / 1_000_000)
    } else {
        None // block indefinitely
    };

    const POLLIN: i16 = 0x001;
    const POLLOUT: i16 = 0x004;
    const POLLERR: i16 = 0x008;
    const POLLHUP: i16 = 0x010;
    const POLLNVAL: i16 = 0x020;

    // Poll loop: check once, if nothing ready either return (timeout=0) or sleep+retry
    let deadline = timeout_ms.map(|ms| {
        let now = crate::hal::rv64::timer::read_time_ms();
        now + ms
    });

    loop {
        let mut ready_count = 0usize;

        for i in 0..nfds {
            let off = i * 8;
            let fd = i32::from_le_bytes(poll_buf[off..off + 4].try_into().unwrap());
            let events = i16::from_le_bytes(poll_buf[off + 4..off + 6].try_into().unwrap());
            let mut revents: i16 = 0;

            if fd < 0 {
                // Negative fd: ignore, revents=0
                poll_buf[off + 6..off + 8].copy_from_slice(&0i16.to_le_bytes());
                continue;
            }

            let tab = task.fd_table.lock();
            match tab.get(fd as u32) {
                None => {
                    revents = POLLNVAL;
                }
                Some(desc) => {
                    match &desc.object {
                        FileObject::Device(DeviceKind::ConsoleRead) => {
                            // Console stdin: always readable (blocking read handles wait)
                            if events & POLLIN != 0 {
                                revents |= POLLIN;
                            }
                        }
                        FileObject::Device(DeviceKind::ConsoleWrite) => {
                            if events & POLLOUT != 0 {
                                revents |= POLLOUT;
                            }
                        }
                        FileObject::Device(DeviceKind::Null)
                        | FileObject::Device(DeviceKind::Zero) => {
                            revents |= events & (POLLIN | POLLOUT);
                        }
                        FileObject::PipeRead(pipe) => {
                            if pipe.readable_len() > 0 {
                                revents |= POLLIN;
                            }
                            if pipe.is_writer_closed() {
                                revents |= POLLHUP;
                            }
                        }
                        FileObject::PipeWrite(pipe) => {
                            if pipe.is_reader_closed() {
                                revents |= POLLERR;
                            } else if events & POLLOUT != 0 {
                                revents |= POLLOUT;
                            }
                        }
                        FileObject::Vnode(_) => {
                            // Regular files are always ready
                            revents |= events & (POLLIN | POLLOUT);
                        }
                    }
                }
            }

            poll_buf[off + 6..off + 8].copy_from_slice(&revents.to_le_bytes());
            if revents != 0 {
                ready_count += 1;
            }
        }

        if ready_count > 0 || matches!(timeout_ms, Some(0)) {
            // Write back revents
            if nfds > 0 {
                fault_in_user_buffer(task, fds_ptr, poll_size, PageFaultAccessType::WRITE).await;
                let rc = unsafe {
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        fds_ptr as *mut u8,
                        poll_buf.as_ptr(),
                        poll_size,
                    )
                };
                if rc != 0 {
                    return Err(Errno::EFAULT);
                }
            }
            return Ok(ready_count);
        }

        // Check timeout
        if let Some(dl) = deadline {
            let now = crate::hal::rv64::timer::read_time_ms();
            if now >= dl {
                // Timed out, write back zero revents
                if nfds > 0 {
                    fault_in_user_buffer(task, fds_ptr, poll_size, PageFaultAccessType::WRITE)
                        .await;
                    let rc = unsafe {
                        crate::hal::rv64::copy_user::copy_user_chunk(
                            fds_ptr as *mut u8,
                            poll_buf.as_ptr(),
                            poll_size,
                        )
                    };
                    if rc != 0 {
                        return Err(Errno::EFAULT);
                    }
                }
                return Ok(0);
            }
        }

        // Check signals
        if task.signals.has_actionable_pending() {
            return Err(Errno::EINTR);
        }

        // Sleep briefly and retry
        crate::executor::sleep(10).await;
    }
}

// ---------------------------------------------------------------------------
// fcntl
// ---------------------------------------------------------------------------

/// sys_fcntl: file descriptor control.
fn sys_fcntl(task: &Arc<Task>, fd: u32, cmd: u32, _arg: usize) -> Result<usize, Errno> {
    const F_DUPFD: u32 = 0;
    const F_GETFD: u32 = 1;
    const F_SETFD: u32 = 2;
    const F_GETFL: u32 = 3;
    const F_SETFL: u32 = 4;
    const F_DUPFD_CLOEXEC: u32 = 1030;

    use crate::fs::fd_table::FdFlags;

    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let mut tab = task.fd_table.lock();
            let desc = Arc::clone(tab.get(fd).ok_or(Errno::EBADF)?);
            let flags = if cmd == F_DUPFD_CLOEXEC {
                FdFlags::CLOEXEC
            } else {
                FdFlags::empty()
            };
            // Find lowest fd >= arg
            let new_fd = tab.insert(desc, flags)?;
            Ok(new_fd as usize)
        }
        F_GETFD => {
            let tab = task.fd_table.lock();
            let flags = tab.get_flags(fd).ok_or(Errno::EBADF)?;
            Ok(if flags.contains(FdFlags::CLOEXEC) {
                1
            } else {
                0
            })
        }
        F_SETFD => {
            // We only support CLOEXEC (bit 0)
            // For now, accept silently — FdTable doesn't have set_flags, so stub it
            let tab = task.fd_table.lock();
            let _ = tab.get(fd).ok_or(Errno::EBADF)?;
            Ok(0)
        }
        F_GETFL => {
            let tab = task.fd_table.lock();
            let desc = tab.get(fd).ok_or(Errno::EBADF)?;
            let mut fl: usize = 0;
            if desc.flags.read && desc.flags.write {
                fl = 2; // O_RDWR
            } else if desc.flags.write {
                fl = 1; // O_WRONLY
            }
            // O_RDONLY = 0
            Ok(fl)
        }
        F_SETFL => {
            // Accept silently — we don't support O_NONBLOCK/O_APPEND yet
            let tab = task.fd_table.lock();
            let _ = tab.get(fd).ok_or(Errno::EBADF)?;
            Ok(0)
        }
        _ => Err(Errno::EINVAL),
    }
}

// ---------------------------------------------------------------------------
// chdir / getcwd
// ---------------------------------------------------------------------------

/// sys_chdir: change current working directory.
async fn sys_chdir_async(task: &Arc<Task>, pathname_ptr: usize) -> Result<(), Errno> {
    let raw_path = copyinstr(task, pathname_ptr, 256).await.ok_or(Errno::EFAULT)?;
    let path = absolutize_path(task, AT_FDCWD, &raw_path)?;
    let vnode = crate::fs::path::resolve(&path).await?;
    if vnode.vtype() != crate::fs::vnode::VnodeType::Directory {
        return Err(Errno::ENOTDIR);
    }
    *task.cwd.lock() = path;
    Ok(())
}

/// sys_getcwd: return current working directory.
fn sys_getcwd(
    task: &Arc<Task>,
    buf: usize,
    size: usize,
) -> Result<usize, Errno> {
    let cwd = task.cwd.lock().clone();
    let needed = cwd.len() + 1;
    if size < needed {
        return Err(Errno::ERANGE);
    }
    if buf == 0 {
        return Err(Errno::EINVAL);
    }
    let mut out = cwd.into_bytes();
    out.push(0);
    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            buf as *mut u8, out.as_ptr(), out.len(),
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// uname
// ---------------------------------------------------------------------------

/// sys_uname: fill struct utsname in user memory.
fn sys_uname(_task: &Arc<Task>, buf: usize) -> Result<(), Errno> {
    if buf == 0 {
        return Err(Errno::EFAULT);
    }
    // struct utsname: 6 fields × 65 bytes = 390 bytes
    const FIELD_LEN: usize = 65;
    let mut utsname = [0u8; FIELD_LEN * 6];

    fn write_field(buf: &mut [u8], offset: usize, s: &[u8]) {
        let end = s.len().min(64);
        buf[offset..offset + end].copy_from_slice(&s[..end]);
        // Already zero-filled, NUL terminator is implicit
    }

    write_field(&mut utsname, FIELD_LEN * 0, b"FreeBSD"); // sysname
    write_field(&mut utsname, FIELD_LEN * 1, b"chronix"); // nodename
    write_field(&mut utsname, FIELD_LEN * 2, b"0.1.0"); // release
    write_field(&mut utsname, FIELD_LEN * 3, b"chronix 0.1.0"); // version
    write_field(&mut utsname, FIELD_LEN * 4, b"riscv64"); // machine
    write_field(&mut utsname, FIELD_LEN * 5, b"(none)"); // domainname

    let rc = unsafe {
        crate::hal::rv64::copy_user::copy_user_chunk(
            buf as *mut u8,
            utsname.as_ptr(),
            utsname.len(),
        )
    };
    if rc != 0 {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// sys_ioctl: handle terminal ioctls for isatty() support.
async fn sys_ioctl_async(
    task: &Arc<Task>,
    fd: u32,
    request: usize,
    argp: usize,
) -> Result<i32, Errno> {
    use crate::fs::fd_table::{DeviceKind, FileObject};

    let is_console = {
        let tab = task.fd_table.lock();
        let desc = tab.get(fd).ok_or(Errno::EBADF)?;
        matches!(
            &desc.object,
            FileObject::Device(DeviceKind::ConsoleRead)
                | FileObject::Device(DeviceKind::ConsoleWrite)
        )
    };
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
            if argp != 0 {
                fault_in_user_buffer(task, argp, 60, PageFaultAccessType::WRITE).await;
                let mut termios = [0u32; 15]; // 60 bytes
                termios[0] = 0; // c_iflag
                termios[1] = 0; // c_oflag
                termios[2] = 0o000017; // c_cflag: CS8 | B38400
                termios[3] = 0o000012; // c_lflag: ICANON | ECHO
                let rc = unsafe {
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        argp as *mut u8,
                        termios.as_ptr() as *const u8,
                        60,
                    )
                };
                if rc != 0 {
                    return Err(Errno::EFAULT);
                }
            }
            Ok(0)
        }
        TIOCGWINSZ => {
            if argp != 0 {
                fault_in_user_buffer(task, argp, 8, PageFaultAccessType::WRITE).await;
                let winsize: [u16; 4] = [24, 80, 0, 0];
                let rc = unsafe {
                    crate::hal::rv64::copy_user::copy_user_chunk(
                        argp as *mut u8,
                        winsize.as_ptr() as *const u8,
                        8,
                    )
                };
                if rc != 0 {
                    return Err(Errno::EFAULT);
                }
            }
            Ok(0)
        }
        TCSETS | TCSETSW | TCSETSF => Ok(0),
        FIONBIO => Ok(0),
        _ => Err(Errno::ENOTTY),
    }
}

/// sys_pipe2: create a pipe and return two fds.
fn sys_pipe2(task: &Arc<Task>, pipefd_ptr: usize, flags: usize) -> Result<(), Errno> {
    use crate::fs::fd_table::{FdFlags, FileDescription, FileObject, OpenFlags};
    use crate::fs::pipe::Pipe;

    let pipe = Pipe::new();
    let cloexec = (flags & 0o2000000) != 0;
    let fd_flags = if cloexec {
        FdFlags::CLOEXEC
    } else {
        FdFlags::empty()
    };

    let read_desc =
        FileDescription::new(FileObject::PipeRead(Arc::clone(&pipe)), OpenFlags::RDONLY);
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
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                pipefd_ptr as *mut u8,
                fds.as_ptr() as *const u8,
                8,
            )
        };
        if rc != 0 {
            return Err(Errno::EFAULT);
        }
    }

    Ok(())
}

/// Normalize a path to an absolute canonical form.
/// Collapses duplicate '/', '.' and '..' components.
fn normalize_absolute_path(path: &str) -> String {
    let mut comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                let _ = comps.pop();
            }
            _ => comps.push(comp),
        }
    }

    if comps.is_empty() {
        return String::from("/");
    }

    let mut out = String::from("/");
    for (idx, comp) in comps.iter().enumerate() {
        if idx > 0 {
            out.push('/');
        }
        out.push_str(comp);
    }
    out
}

/// Convert a user-provided path to an absolute path with cwd/dirfd semantics.
fn absolutize_path(task: &Arc<Task>, dirfd: isize, raw_path: &str) -> Result<String, Errno> {
    if raw_path.is_empty() {
        return Err(Errno::ENOENT);
    }
    if raw_path.starts_with('/') {
        return Ok(normalize_absolute_path(raw_path));
    }

    // Relative path from cwd.
    if dirfd == AT_FDCWD {
        let cwd = task.cwd.lock().clone();
        let mut combined = String::new();
        if cwd == "/" {
            combined.push('/');
            combined.push_str(raw_path);
        } else {
            combined.push_str(&cwd);
            combined.push('/');
            combined.push_str(raw_path);
        }
        return Ok(normalize_absolute_path(&combined));
    }

    // Relative path from directory fd.
    if dirfd >= 0 {
        let base = {
            let tab = task.fd_table.lock();
            let desc = tab.get(dirfd as u32).ok_or(Errno::EBADF)?;
            match &desc.object {
                crate::fs::fd_table::FileObject::Vnode(v) => {
                    if v.vtype() != crate::fs::vnode::VnodeType::Directory {
                        return Err(Errno::ENOTDIR);
                    }
                    String::from(v.path())
                }
                _ => return Err(Errno::ENOTDIR),
            }
        };
        let mut combined = String::new();
        if base == "/" {
            combined.push('/');
            combined.push_str(raw_path);
        } else {
            combined.push_str(&base);
            combined.push('/');
            combined.push_str(raw_path);
        }
        return Ok(normalize_absolute_path(&combined));
    }

    Err(Errno::EINVAL)
}

/// sys_openat: resolve path and open file.
async fn sys_openat_async(
    task: &Arc<Task>,
    dirfd: isize,
    pathname_ptr: usize,
    flags: usize,
) -> Result<u32, Errno> {
    use crate::fs::fd_table::OpenFlags;

    // Read pathname from user memory using fault-safe copyinstr.
    let raw_path = copyinstr(task, pathname_ptr, 256).await.ok_or(Errno::EFAULT)?;
    let path_str = absolutize_path(task, dirfd, &raw_path)?;

    let open_flags = OpenFlags {
        read: true,
        write: (flags & 0x1) != 0 || (flags & 0x2) != 0,
    };

    crate::fs::syscalls::sys_open(&task.fd_table, &path_str, open_flags).await
}

/// Read a NUL-terminated string from user memory. Returns None on fault.
///
/// Pre-faults demand-paged user pages before copying so that copy_user_chunk
/// doesn't hit pcb_onfault on unmapped pages.
pub(crate) async fn copyinstr(task: &Arc<Task>, user_ptr: usize, max_len: usize) -> Option<String> {
    if user_ptr == 0 {
        return None;
    }
    // Pre-fault the first page; for short strings (pathnames) this is enough.
    // For strings that may span a page boundary we fault the second page too.
    let first_page_remaining = PAGE_SIZE - (user_ptr & (PAGE_SIZE - 1));
    let prefault_len = max_len.min(first_page_remaining + PAGE_SIZE);
    fault_in_user_buffer(task, user_ptr, prefault_len, PageFaultAccessType::READ).await;

    let mut buf = alloc::vec![0u8; max_len];
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
///
/// Pre-faults demand-paged user pages for both the pointer array and each
/// string before copying.
pub(crate) async fn copyin_argv(
    task: &Arc<Task>,
    user_argv: usize,
    max_count: usize,
    max_total: usize,
) -> alloc::vec::Vec<String> {
    let mut result = alloc::vec::Vec::new();
    if user_argv == 0 {
        return result;
    }
    let mut total = 0usize;
    for i in 0..max_count {
        let ptr_addr = user_argv + i * core::mem::size_of::<usize>();
        // Pre-fault the page containing this pointer
        fault_in_user_buffer(
            task,
            ptr_addr,
            core::mem::size_of::<usize>(),
            PageFaultAccessType::READ,
        )
        .await;
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
        if let Some(s) = copyinstr(task, str_ptr, 256).await {
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

/// SA_RESTART: check if the pending signal has SA_RESTART.
/// Returns true if the syscall should be restarted (caller must skip
/// sepc advance and ret_val assignment so the ecall re-executes with
/// original register state).
fn should_restart_syscall(task: &Arc<Task>) -> bool {
    let pending = task
        .signals
        .pending
        .load(core::sync::atomic::Ordering::Acquire);
    let blocked = task
        .signals
        .blocked
        .load(core::sync::atomic::Ordering::Relaxed);

    let mut unblockable = crate::proc::signal::SigSet::empty();
    unblockable
        .add(crate::proc::signal::Signal::new_unchecked(
            crate::proc::signal::SIGKILL,
        ))
        .add(crate::proc::signal::Signal::new_unchecked(
            crate::proc::signal::SIGSTOP,
        ));
    let deliverable = pending.intersect(blocked.union(unblockable).difference(blocked));

    if !deliverable.is_empty() {
        let bit = deliverable.as_u64().trailing_zeros() as u8;
        let sig = bit + 1;
        task.signals.is_restart(sig)
    } else {
        false
    }
}

/// Mark process as zombie and wake parent.
/// `wstatus` must be pre-encoded in Linux format:
///   normal exit:  (code << 8) & 0x7f00   (low 7 bits = 0)
///   signal kill:  signo & 0x7f           (low 7 bits = signal)
pub(crate) fn do_exit(task: &Arc<Task>, wstatus: crate::proc::exit_wait::WaitStatus) {
    klog!(
        proc,
        trace,
        "do_exit pid={} wstatus={:#x}",
        task.pid,
        wstatus.0
    );
    // Debug logging handled globally by Makefile levels now.
    task.exit_status
        .store(wstatus.0, core::sync::atomic::Ordering::Release);
    task.set_zombie();

    // Unregister from global task registry
    crate::proc::signal::unregister_task(task.pid);

    // Post SIGCHLD to parent
    if let Some(parent) = task.parent.upgrade() {
        parent.signals.post_signal(crate::proc::signal::SIGCHLD);

        // SA_NOCLDWAIT: auto-reap child (remove from parent's children list)
        let sigchld_action = {
            let actions = parent.signals.actions.lock();
            actions[(crate::proc::signal::SIGCHLD - 1) as usize]
        };
        if sigchld_action.flags & crate::proc::signal::SA_NOCLDWAIT != 0 {
            parent.children.lock().retain(|c| c.pid != task.pid);
        }

        // Wake parent's WaitChildFuture
        if let Some(waker) = parent.parent_waker.lock().take() {
            waker.wake();
        }
        // Wake parent's top-level waker for signal delivery
        if let Some(waker) = parent.top_level_waker.lock().take() {
            waker.wake();
        }
    }

    // In autotest flow, init exits after all test commands finish.
    // Power off QEMU so the judge can collect results.
    if task.pid == 1 {
        #[cfg(feature = "autotest")]
        {
            crate::hal::rv64::sbi::shutdown();
        }
    }
}

/// Wrapper future that activates the task's pmap on every `poll`.
/// This ensures that if the executor resumes an `.await` point (e.g. inside `sys_wait4_async`),
/// the correct user page table is loaded into `satp` before accessing user memory.
struct PmapWrapper<F> {
    task: Arc<Task>,
    inner: F,
}

impl<F: core::future::Future> core::future::Future for PmapWrapper<F> {
    type Output = F::Output;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        {
            let mut pmap = this.task.pmap.lock();
            crate::mm::pmap::pmap_activate(&mut pmap);
        }
        let inner = unsafe { core::pin::Pin::new_unchecked(&mut this.inner) };
        inner.poll(cx)
    }
}

/// Spawn a user task on the given CPU.
pub fn spawn_user_task(task: Arc<Task>, cpu: usize) {
    // Register in global task registry for kill/getpgid lookups
    crate::proc::signal::register_task(&task);

    let wrapper = PmapWrapper {
        task: Arc::clone(&task),
        inner: run_tasks(task),
    };

    let handle = spawn_kernel_task(wrapper, cpu);
    handle.detach();
}
