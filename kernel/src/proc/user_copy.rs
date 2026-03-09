//! User memory access helpers.
//!
//! Provides safe wrappers for copying data between kernel and user space,
//! with automatic page fault handling.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use crate::hal_common::{PAGE_SIZE, VirtAddr};

use crate::mm::vm::fault::PageFaultAccessType;
use crate::proc::task::Task;

/// Pre-fault all user pages covering [user_ptr, user_ptr+len).
///
/// This ensures pages are mapped before copy_user_chunk is called,
/// avoiding pcb_onfault traps on unmapped pages.
pub async fn fault_in_user_buffer(
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
        let _ = crate::mm::vm::fault_async::resolve_user_fault(task, VirtAddr::new(va), access).await;
        va += PAGE_SIZE;
    }
}

/// Read a NUL-terminated string from user memory. Returns None on fault.
///
/// Pre-faults demand-paged user pages before copying so that copy_user_chunk
/// doesn't hit pcb_onfault on unmapped pages.
pub async fn copyinstr(task: &Arc<Task>, user_ptr: usize, max_len: usize) -> Option<String> {
    if user_ptr == 0 {
        return None;
    }
    let mut out = Vec::new();
    let mut cur_page = usize::MAX;

    for i in 0..max_len {
        let src = user_ptr + i;
        let page = src & !(PAGE_SIZE - 1);
        if page != cur_page {
            // Fault in one byte on each newly touched page.
            fault_in_user_buffer(task, src, 1, PageFaultAccessType::READ).await;
            cur_page = page;
        }

        let mut ch = 0u8;
        let rc = unsafe {
            crate::hal::rv64::copy_user::copy_user_chunk(
                &mut ch as *mut u8,
                src as *const u8,
                1,
            )
        };
        if rc != 0 {
            return None;
        }
        if ch == 0 {
            return Some(unsafe { String::from_utf8_unchecked(out) });
        }
        out.push(ch);
    }

    None
}

/// Read a NULL-terminated array of string pointers from user memory.
/// Returns a Vec of Strings. Stops at NULL pointer or max_count.
///
/// Pre-faults demand-paged user pages for both the pointer array and each
/// string before copying.
pub async fn copyin_argv(
    task: &Arc<Task>,
    user_argv: usize,
    max_count: usize,
    max_total: usize,
) -> Vec<String> {
    let mut result = Vec::new();
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

/// Mark process as zombie and wake parent.
///
/// `wstatus` must be pre-encoded in Linux format:
///   normal exit:  (code << 8) & 0x7f00   (low 7 bits = 0)
///   signal kill:  signo & 0x7f           (low 7 bits = signal)
pub fn do_exit(task: &Arc<Task>, wstatus: crate::proc::exit_wait::WaitStatus) {
    crate::klog!(
        proc,
        trace,
        "do_exit pid={} wstatus={:#x}",
        task.pid,
        wstatus.0
    );
    task.exit_status
        .store(wstatus.0, core::sync::atomic::Ordering::Release);
    task.set_zombie();
    task.release_zombie_resources();

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
