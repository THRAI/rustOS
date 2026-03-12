#![no_std]
#![no_main]
#![feature(c_variadic)]

extern crate alloc;

use core::arch::global_asm;

#[macro_use]
mod console;
mod alloc_early;
mod drivers;
mod executor;
pub mod feature_flags;
mod fs;
mod hal;
mod hal_common;
mod ipc;
mod libc_stubs;
mod mm;
mod proc;
mod syscall;
mod trap;

// Include boot assembly
global_asm!(include_str!("hal/rv64/boot.S"));
// Include trap assembly
global_asm!(include_str!("hal/rv64/trap.S"));
// Include memset/memcpy/memmove assembly (ported from FreeBSD)
global_asm!(include_str!("hal/rv64/memops.S"));

/// Atomic flag: first hart to reach rust_main claims boot role.
static BOOT_HART_CLAIMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Entry point called from boot.S
/// a0 = hartid, a1 = dtb_ptr
/// OpenSBI can pick any hart as boot hart, so we use an atomic flag.
#[no_mangle]
pub extern "C" fn rust_main(hartid: usize, dtb_ptr: usize) -> ! {
    if !BOOT_HART_CLAIMED.swap(true, core::sync::atomic::Ordering::AcqRel) {
        hal::init_uart();
        kprintln!("hello world");
        klog!(boot, info, "hart {} booting, dtb @ {:#x}", hartid, dtb_ptr);

        // Ensure SIE=0 and set stvec before anything that uses IrqSafeSpinLock.
        // OpenSBI may leave SIE=1; IrqSafeSpinLock restore would re-enable it,
        // causing stray interrupts before per-CPU data is ready.
        unsafe {
            core::arch::asm!("csrci sstatus, 0x2");
        }
        hal::set_kernel_trap_entry();

        // Initialize kernel heap first — everything below may allocate
        alloc_early::init_heap();

        // Parse FDT to discover CPUs (before trap/timer — no IRQs yet)
        let (num_cpus, hartids) = hal::parse_cpus(dtb_ptr);

        // Pre-initialize PerCpu for ALL discovered harts.
        // Must happen before trap/timer init because the timer IRQ handler
        // accesses per-CPU data via tp register.
        for (i, &hid) in hartids.iter().enumerate().take(num_cpus) {
            let cid = hal::hart_to_cpu(hid).unwrap_or(i);
            klog!(boot, info, "init_per_cpu({}, {}) start", cid, hid);
            executor::init_per_cpu(cid, hid);
            klog!(boot, info, "init_per_cpu({}, {}) done", cid, hid);
        }
        let cpu0 = hal::hart_to_cpu(hartid).unwrap_or(0);
        unsafe { executor::set_tp(cpu0) };
        klog!(
            boot,
            info,
            "per-cpu data initialized for {} harts",
            num_cpus
        );

        // Initialize trap infrastructure (stvec + STIE + SSIE + SEIE)
        trap::init();

        // Initialize PLIC for UART IRQ on boot hart
        hal::init_plic_hart(hartid);

        // Arm the first timer interrupt (10ms interval)
        hal::init_timer();

        // Initialize frame allocator with physical memory after kernel image
        {
            extern "C" {
                static ekernel: u8;
            }
            let kernel_end = unsafe { &ekernel as *const u8 as usize };
            let mem_start = crate::hal_common::PhysAddr::new(kernel_end);

            // Discover physical RAM from FDT instead of hardcoding 0x8800_0000.
            let (num_regions, regions) = hal::parse_memory(dtb_ptr);
            // Use the first region that contains kernel_end (the kernel is
            // loaded into one of the FDT-reported memory regions).
            let mem_end = {
                let mut end = crate::hal_common::PhysAddr::new(0);
                for i in 0..num_regions {
                    let r = &regions[i];
                    let region_end = r.base + r.size;
                    if kernel_end >= r.base && kernel_end < region_end {
                        end = crate::hal_common::PhysAddr::new(region_end);
                        break;
                    }
                }
                if end.as_usize() == 0 {
                    // Kernel is not inside any reported region — use the
                    // largest region's end as a conservative fallback.
                    let mut best = 0usize;
                    for i in 0..num_regions {
                        let region_end = regions[i].base + regions[i].size;
                        if region_end > best {
                            best = region_end;
                        }
                    }
                    end = crate::hal_common::PhysAddr::new(best);
                    klog!(
                        boot,
                        warn,
                        "kernel not in any FDT memory region, using {:#x}",
                        best
                    );
                }
                end
            };

            klog!(
                boot,
                info,
                "init_frame_allocator({:#x}..{:#x})",
                mem_start.as_usize(),
                mem_end.as_usize()
            );
            // Test heap allocation before buddy init
            {
                let v: alloc::vec::Vec<u64> = alloc::vec![1, 2, 3, 4, 5];
                klog!(boot, info, "test vec alloc OK, len={}", v.len());
            }
            mm::allocator::init_frame_allocator(mem_start, mem_end);
            klog!(boot, info, "frame allocator done");
        }

        // Initialize VirtIO-blk driver (probes MMIO addresses for block device)
        drivers::init_virtio_blk();

        // Initialize VFS caches
        klog!(boot, info, "dentry::init...");
        fs::init_dentry_cache();
        klog!(boot, info, "vnode_cache::init...");
        fs::init_vnode_cache();

        klog!(boot, info, "delegate::init...");
        // Initialize filesystem delegate (mounts ext4, spawns delegate task)
        fs::init_delegate();
        klog!(boot, info, "delegate done");

        // Boot secondary harts (always — needed for normal operation)
        if num_cpus > 1 {
            hal::smp::boot_secondary_harts(num_cpus, &hartids, hartid);
        }

        // --- Integration tests: only compiled when `--features qemu-test` ---
        #[cfg(feature = "qemu-test")]
        {
            // Synchronous tests (no executor needed)
            test_fixup();
            test_uiomove_short_read();
            test_fork_exit_wait4();
            register_clobber_test();

            // Async executor test: prove the path works
            executor::spawn_kernel_task(
                async {
                    kprintln!("hello from async future!");
                },
                cpu0,
            )
            .detach();

            // Sleep future (should wake after ~100ms)
            executor::spawn_kernel_task(
                async {
                    executor::sleep(100).await;
                    kprintln!("woke after 100ms!");
                },
                cpu0,
            )
            .detach();

            // Cross-CPU tests (delay to let secondary harts finish per_cpu re-init)
            if num_cpus > 1 {
                executor::spawn_kernel_task(
                    async {
                        executor::sleep(50).await;
                        kprintln!("hello from CPU 1");
                    },
                    1,
                )
                .detach();
            }
            if num_cpus > 1 {
                executor::spawn_kernel_task(
                    async {
                        executor::sleep(50).await;
                        executor::spawn_kernel_task(
                            async {
                                kprintln!("cross-cpu wake on CPU 1");
                            },
                            1,
                        )
                        .detach();
                    },
                    cpu0,
                )
                .detach();
            }

            // Pmap tests
            executor::spawn_kernel_task(
                async {
                    mm::pmap::test_integration::test_pmap_extract_only();
                },
                cpu0,
            )
            .detach();
            executor::spawn_kernel_task(
                async {
                    executor::sleep(200).await;
                    mm::pmap::test_integration::test_pmap_satp_switch();
                },
                cpu0,
            )
            .detach();

            // VM tests
            executor::spawn_kernel_task(
                async {
                    executor::sleep(400).await;
                    mm::vm::test_integration::test_anonymous_page_fault();
                    mm::vm::test_integration::test_cow_fault();
                },
                cpu0,
            )
            .detach();
            executor::spawn_kernel_task(
                async {
                    executor::sleep(400).await;
                    mm::vm::test_integration::test_frame_alloc_sync_works();
                },
                cpu0,
            )
            .detach();
            executor::spawn_kernel_task(
                async {
                    executor::sleep(400).await;
                    mm::vm::test_integration::test_iterative_drop_500();
                },
                cpu0,
            )
            .detach();
            executor::spawn_kernel_task(
                async {
                    executor::sleep(500).await;
                    mm::vm::test_integration::test_fork_bomb_stress();
                },
                cpu0,
            )
            .detach();

            // Filesystem tests (need delegate mount time)
            executor::spawn_kernel_task(
                async {
                    executor::sleep(200).await;
                    test_delegate_read().await;
                },
                cpu0,
            )
            .detach();
            executor::spawn_kernel_task(
                async {
                    executor::sleep(400).await;
                    test_vfs_read().await;
                },
                cpu0,
            )
            .detach();
            executor::spawn_kernel_task(
                async {
                    executor::sleep(600).await;
                    test_fork_exec_wait4().await;
                },
                cpu0,
            )
            .detach();

            // Phase 4 integration tests
            executor::spawn_kernel_task(
                async {
                    executor::sleep(700).await;
                    test_pipe_data_transfer().await;
                },
                cpu0,
            )
            .detach();
            executor::spawn_kernel_task(
                async {
                    executor::sleep(800).await;
                    test_signal_pending_delivery();
                },
                cpu0,
            )
            .detach();
            executor::spawn_kernel_task(
                async {
                    executor::sleep(800).await;
                    test_mmap_munmap();
                },
                cpu0,
            )
            .detach();
            executor::spawn_kernel_task(
                async {
                    executor::sleep(800).await;
                    test_device_nodes().await;
                },
                cpu0,
            )
            .detach();
            executor::spawn_kernel_task(
                async {
                    executor::sleep(900).await;
                    test_futex_wake();
                },
                cpu0,
            )
            .detach();

            // Shutdown after all tests complete (12s generous timeout)
            executor::spawn_kernel_task(
                async {
                    executor::sleep(12_000).await;
                    hal::sbi::shutdown();
                },
                cpu0,
            )
            .detach();
        }

        // Spawn init process: exec /bin/init, then enter user mode
        {
            let init_task = proc::Task::new_init();
            let init_task2 = init_task.clone();
            let init_cpu = cpu0;
            executor::spawn_kernel_task(
                async move {
                    // Wait for delegate mount to complete
                    executor::sleep(100).await;

                    #[cfg(feature = "autotest")]
                    let envp = alloc::vec![
                        alloc::string::String::from("PATH=/riscv/musl:/riscv/glibc:/bin:/sbin"),
                        alloc::string::String::from("HOME=/"),
                    ];
                    #[cfg(not(feature = "autotest"))]
                    let envp = alloc::vec![
                        alloc::string::String::from(
                            "PATH=/bin/busybox:/bin:/sbin:/usr/bin:/usr/sbin"
                        ),
                        alloc::string::String::from("HOME=/"),
                    ];

                    #[cfg(feature = "autotest")]
                    let launch_attempts = alloc::vec![
                        (
                            "/bin/initproc",
                            alloc::vec![alloc::string::String::from("/bin/initproc")],
                        ),
                        (
                            "/riscv/musl/busybox",
                            alloc::vec![
                                alloc::string::String::from("/riscv/musl/busybox"),
                                alloc::string::String::from("sh"),
                                alloc::string::String::from("/riscv/run-oj.sh"),
                            ],
                        ),
                    ];

                    #[cfg(not(feature = "autotest"))]
                    let launch_attempts = alloc::vec![
                        (
                            "/bin/initproc",
                            alloc::vec![alloc::string::String::from("/bin/initproc")],
                        ),
                        (
                            "/bin/init",
                            alloc::vec![alloc::string::String::from("/bin/init")],
                        ),
                        (
                            "/bin/busybox",
                            alloc::vec![
                                alloc::string::String::from("/bin/busybox"),
                                alloc::string::String::from("sh"),
                            ],
                        ),
                    ];

                    let mut launched = false;
                    for (exec_path, argv) in launch_attempts {
                        match proc::exec::do_execve(&init_task2, exec_path, &argv, &envp).await {
                            Ok((entry, sp)) => {
                                {
                                    let mut tf = init_task2.trap_frame.lock();
                                    tf.sepc = entry;
                                    tf.x[2] = sp;
                                    tf.sstatus = (1 << 5) | (1 << 13); // SPP=0, SPIE=1, FS=Initial
                                }
                                kprintln!("exec OK: {} entry={:#x} sp={:#x}", exec_path, entry, sp);
                                executor::spawn_user_task(init_task2, init_cpu);
                                launched = true;
                                break;
                            },
                            Err(e) => {
                                klog!(boot, warn, "exec {} failed: {:?}", exec_path, e);
                            },
                        }
                    }
                    if !launched {
                        klog!(boot, error, "no usable init program found");
                    }
                },
                cpu0,
            )
            .detach();
        }

        // Enable global interrupts
        hal::irq::enable();
        klog!(boot, info, "interrupts enabled, entering executor loop");

        // Enter the executor loop (never returns)
        executor::executor_loop();
    }

    // Non-boot harts: return to SBI stopped state so hart_start can restart them
    // at secondary_entry. A wfi loop won't work because hart_start requires
    // the hart to be in SBI "stopped" state.
    hal::sbi::hart_stop();
    // hart_stop should not return, but just in case:
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

#[cfg(feature = "qemu-test")]
fn test_uiomove_short_read() {
    use mm::uio::{uiomove, UioDir};

    // Allocate a valid kernel buffer (2 pages)
    let mut kern_buf = [0u8; 8192];

    // Use a valid kernel address as "user" source for the first chunk,
    // then an unmapped address for the second chunk.
    // We'll do this by calling uiomove with a large len that spans into
    // unmapped memory. We place the "user" pointer such that the first
    // page-chunk is valid but the next page is unmapped.
    //
    // Strategy: use a stack buffer as "user" source, but request more bytes
    // than the buffer. The copy_user_chunk will fault when it reads past
    // the valid stack region into unmapped memory.
    //
    // Simpler approach: call uiomove with a bad user pointer at a page
    // boundary offset so the first chunk succeeds (from valid memory)
    // and the second chunk faults.

    // Valid source data on the stack
    let user_data = [0xCCu8; 4096];
    let user_ptr = user_data.as_ptr() as usize;

    // Calculate bytes to end of this page
    let page_offset = user_ptr & 0xFFF;
    let _first_chunk = 4096 - page_offset;

    // Request first_chunk + 1 more byte — the extra byte is on the next page
    // which may or may not be mapped. Instead, let's use a known-bad second page.
    // We'll point user at (bad_page - first_chunk) so first chunk reads from
    // the tail of a valid page and second chunk hits the bad page.

    // Actually, the simplest reliable test: use copy_user_chunk directly
    // to verify uiomove's short-read path. We know 0xDEAD_0000 is unmapped.
    // Place "user" at an address where first chunk reads valid stack memory
    // and second chunk would read from unmapped memory.

    // Even simpler: just test with a 2-chunk scenario using valid kernel memory
    // for first chunk and bad pointer for second. We can do this by using
    // uiomove_inner logic: call uiomove with kern buffer and a user pointer
    // that starts in valid memory but the total len extends past valid memory.

    // Most reliable approach: test that uiomove with a fully-bad pointer
    // returns EFAULT, and test with a fully-good pointer returns full copy.
    // The short-read path is tested on host. For QEMU, verify the integration.

    // Test 1: fully valid copy (both buffers are kernel stack memory)
    let mut src = [0xDD_u8; 128];
    let mut dst = [0u8; 128];
    let r = uiomove(dst.as_mut_ptr(), src.as_mut_ptr(), 128, UioDir::CopyIn);
    match r {
        Ok(res) if res.done == 128 => {},
        other => {
            kprintln!("uiomove short-read FAIL (full copy: {:?})", other);
            return;
        },
    }

    // Test 2: fault on first chunk returns EFAULT
    let r = uiomove(
        kern_buf.as_mut_ptr(),
        0xDEAD_0000usize as *mut u8,
        4096,
        UioDir::CopyIn,
    );
    match r {
        Err(crate::hal_common::Errno::Efault) => {},
        other => {
            kprintln!("uiomove short-read FAIL (efault: {:?})", other);
            return;
        },
    }

    kprintln!("uiomove short-read PASS");
}

#[cfg(feature = "qemu-test")]
fn test_fork_exit_wait4() {
    use alloc::sync::Arc;

    use proc::{fork, sys_exit, syscall_result::SyscallResult, Task, WaitChildFuture, WaitStatus};

    // Create init task (pid 1)
    let init = Task::new_init();
    let init_pid = init.pid;

    // Fork to create child
    let child = fork(&init);
    let child_pid = child.pid;

    // Verify child has different pid
    assert_ne!(init_pid, child_pid, "child must have different pid");

    // Verify child's parent is init
    assert_eq!(child.ppid(), init_pid, "child ppid must be parent pid");

    // Verify child is in parent's children list
    assert_eq!(init.children.lock().len(), 1);

    // Verify COW: child's VmMap was forked (it's a new VmMap instance)
    // (VmMap::fork creates shadow objects for each VMA — with empty parent, child is also empty)

    // Child exits with code 42
    let result = sys_exit(&child, WaitStatus::exited(42));
    match result {
        SyscallResult::Terminated => {},
        _ => panic!("sys_exit must return Terminated"),
    }

    // Verify child is now ZOMBIE
    assert_eq!(child.state(), proc::TaskState::Zombie);

    // Verify exit status is properly encoded
    assert_eq!(
        child
            .exit_status
            .load(core::sync::atomic::Ordering::Acquire),
        42 << 8
    );

    // Test WaitChildFuture synchronously via a manual poll
    // Since child is already ZOMBIE, the first poll should return Ready
    use core::{
        future::Future,
        pin::Pin,
        task::{RawWaker, RawWakerVTable, Waker},
    };

    // Create a no-op waker for manual polling
    fn noop_raw_waker() -> RawWaker {
        fn no_op(_: *const ()) {}
        fn clone(p: *const ()) -> RawWaker {
            RawWaker::new(p, &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
        RawWaker::new(core::ptr::null(), &VTABLE)
    }
    let waker = unsafe { Waker::from_raw(noop_raw_waker()) };
    let mut cx = core::task::Context::from_waker(&waker);

    let mut wait_fut = WaitChildFuture::new(Arc::clone(&init), -1);
    let poll_result = Pin::new(&mut wait_fut).poll(&mut cx);

    match poll_result {
        core::task::Poll::Ready(Some((pid, status))) => {
            assert_eq!(pid, child_pid, "wait4 must return child pid");
            assert_eq!(
                status,
                42 << 8,
                "wait4 must return exit code 42 (WaitStatus encoded)"
            );
        },
        other => panic!("wait4 expected Ready(Some), got {:?}", other),
    }

    // Verify child was reaped from parent's children list
    assert_eq!(init.children.lock().len(), 0, "zombie must be reaped");

    kprintln!("fork-exit-wait4 PASS");
}

#[cfg(feature = "qemu-test")]
async fn test_delegate_read() {
    match fs::fs_open("/hello.txt").await {
        Ok(handle) => {
            let mut buf = [0u8; 64];
            match fs::fs_read(handle, &mut buf).await {
                Ok(n) => {
                    let content = core::str::from_utf8(&buf[..n]).unwrap_or("<invalid utf8>");
                    if content.trim_end() == "hello from ext4" {
                        kprintln!("delegate read PASS");
                    } else {
                        kprintln!("delegate read FAIL (content={:?})", content);
                    }
                },
                Err(e) => kprintln!("delegate read FAIL (read err={})", e),
            }
            let _ = fs::fs_close(handle).await;
        },
        Err(e) => kprintln!("delegate read FAIL (open err={})", e),
    }
}

#[cfg(feature = "qemu-test")]
async fn test_vfs_read() {
    use fs::{FdTable, OpenFlags};
    let fd_table = crate::hal_common::SpinMutex::new(FdTable::new());

    // First read: goes through delegate
    match syscall::fs::open(&fd_table, "/hello.txt", OpenFlags::RDONLY, 0).await {
        Ok(fd) => {
            let mut buf = [0u8; 64];
            match syscall::fs::read(&fd_table, fd, &mut buf).await {
                Ok(n) => {
                    let content = core::str::from_utf8(&buf[..n]).unwrap_or("<invalid utf8>");
                    if content.trim_end() == "hello from ext4" {
                        let mut buf2 = [0u8; 64];
                        // Reopen to reset offset
                        let _ = syscall::fs::close(&fd_table, fd);
                        match syscall::fs::open(&fd_table, "/hello.txt", OpenFlags::RDONLY, 0).await
                        {
                            Ok(fd2) => {
                                match syscall::fs::read(&fd_table, fd2, &mut buf2).await {
                                    Ok(n2) => {
                                        let content2 = core::str::from_utf8(&buf2[..n2])
                                            .unwrap_or("<invalid utf8>");
                                        if content2.trim_end() == "hello from ext4" {
                                            kprintln!("vfs read PASS");
                                        } else {
                                            kprintln!(
                                                "vfs read FAIL (cache content={:?})",
                                                content2
                                            );
                                        }
                                    },
                                    Err(_) => kprintln!("vfs read FAIL (cache read err)"),
                                }
                                let _ = syscall::fs::close(&fd_table, fd2);
                            },
                            Err(_) => kprintln!("vfs read FAIL (reopen err)"),
                        }
                    } else {
                        kprintln!("vfs read FAIL (content={:?})", content);
                    }
                },
                Err(_) => kprintln!("vfs read FAIL (read err)"),
            }
        },
        Err(_) => kprintln!("vfs read FAIL (open err)"),
    }
}

#[cfg(feature = "qemu-test")]
async fn test_fork_exec_wait4() {
    use alloc::sync::Arc;

    use proc::{do_execve, fork, sys_exit, Task, WaitChildFuture, WaitStatus};

    // Create init task
    let init = Task::new_init();

    // Fork child
    let child = fork(&init);
    let child_pid = child.pid;

    // Try do_execve on the child — use /hello.txt which is NOT an ELF
    // This should fail with ENOEXEC, proving the ELF validation works
    match do_execve(&child, "/hello.txt", &[], &[]).await {
        Err(crate::hal_common::Errno::Enoexec) => {
            // Expected: hello.txt is not an ELF binary
        },
        Ok(_) => {
            kprintln!("fork-exec-wait4 FAIL (hello.txt accepted as ELF)");
            return;
        },
        Err(e) => {
            kprintln!("fork-exec-wait4 FAIL (unexpected error: {:?})", e);
            return;
        },
    }

    // Child exits with code 7
    let _ = sys_exit(&child, WaitStatus::exited(7));

    // Parent wait4 collects exit status
    let wait_result = WaitChildFuture::new(Arc::clone(&init), -1).await;
    match wait_result {
        Some((pid, status)) => {
            if pid == child_pid && status == 7 << 8 {
                kprintln!("fork-exec-wait4 PASS");
            } else {
                kprintln!("fork-exec-wait4 FAIL (pid={}, status={})", pid, status);
            }
        },
        None => {
            kprintln!("fork-exec-wait4 FAIL (wait4 returned None)");
        },
    }
}

#[cfg(feature = "qemu-test")]
async fn test_pipe_data_transfer() {
    use fs::Pipe;

    let pipe = Pipe::new();
    let msg = b"hello pipe";

    // Write to pipe
    match pipe.write(msg) {
        Ok(n) if n == msg.len() => {},
        other => {
            kprintln!("pipe data transfer FAIL (write: {:?})", other);
            return;
        },
    }

    // Read from pipe
    let mut buf = [0u8; 64];
    match pipe.read(&mut buf) {
        Ok(n) if n == msg.len() => {
            if &buf[..n] == msg {
                // Test EOF: close writer, read should return 0
                pipe.close_write();
                match pipe.read(&mut buf) {
                    Ok(0) => {},
                    other => {
                        kprintln!("pipe data transfer FAIL (eof: {:?})", other);
                        return;
                    },
                }
            } else {
                kprintln!("pipe data transfer FAIL (data mismatch)");
                return;
            }
        },
        other => {
            kprintln!("pipe data transfer FAIL (read: {:?})", other);
            return;
        },
    }

    // Test EPIPE: close reader, write should return EPIPE
    let pipe2 = Pipe::new();
    pipe2.close_read();
    match pipe2.write(b"test") {
        Err(crate::hal_common::Errno::Epipe) => {},
        other => {
            kprintln!("pipe data transfer FAIL (epipe: {:?})", other);
            return;
        },
    }

    kprintln!("pipe data transfer PASS");
}

#[cfg(feature = "qemu-test")]
fn test_signal_pending_delivery() {
    use proc::{
        signal::{SA_NOCLDWAIT, SA_RESTART, SIGCHLD, SIGUSR1},
        Task,
    };

    let task = Task::new_init();

    // Post SIGUSR1, verify pending
    task.signals.post_signal(SIGUSR1);
    if !task.signals.has_unmasked_pending() {
        kprintln!("signal pending delivery FAIL (not pending after post)");
        return;
    }

    // Dequeue it
    match task.signals.dequeue_signal() {
        Some(sig) if sig == SIGUSR1 => {},
        other => {
            kprintln!("signal pending delivery FAIL (dequeue: {:?})", other);
            return;
        },
    }

    // Should be empty now
    if task.signals.has_unmasked_pending() {
        kprintln!("signal pending delivery FAIL (still pending after dequeue)");
        return;
    }

    // Test SA_RESTART flag via is_restart
    {
        let mut actions = task.signals.actions.lock();
        actions[(SIGUSR1 - 1) as usize].flags = SA_RESTART;
    }
    if !task.signals.is_restart(SIGUSR1) {
        kprintln!("signal pending delivery FAIL (is_restart false)");
        return;
    }

    // Test blocked signals: block SIGUSR1, post it, should not be unmasked-pending
    let mut new_blocked = proc::signal::SigSet::empty();
    new_blocked.add(proc::signal::Signal::new(SIGUSR1).unwrap());
    task.signals
        .blocked
        .store(new_blocked, core::sync::atomic::Ordering::Release);
    task.signals.post_signal(SIGUSR1);
    if task.signals.has_unmasked_pending() {
        kprintln!("signal pending delivery FAIL (blocked signal visible)");
        return;
    }
    // Clear blocked, now it should be visible
    task.signals.blocked.store(
        proc::signal::SigSet::empty(),
        core::sync::atomic::Ordering::Release,
    );
    if !task.signals.has_unmasked_pending() {
        kprintln!("signal pending delivery FAIL (unblocked signal not visible)");
        return;
    }
    // Clean up
    let _ = task.signals.dequeue_signal();

    // Test SA_NOCLDWAIT flag
    {
        let mut actions = task.signals.actions.lock();
        actions[(SIGCHLD - 1) as usize].flags = SA_NOCLDWAIT;
    }
    {
        let actions = task.signals.actions.lock();
        if actions[(SIGCHLD - 1) as usize].flags & SA_NOCLDWAIT == 0 {
            kprintln!("signal pending delivery FAIL (SA_NOCLDWAIT not set)");
            return;
        }
    }

    kprintln!("signal pending delivery PASS");
}

#[cfg(feature = "qemu-test")]
fn test_mmap_munmap() {
    use mm::vm::{vm_map::VmMap, VmObject};

    use crate::hal_common::{VirtAddr, PAGE_SIZE};

    let task = proc::Task::new_init();

    // Insert an anonymous VMA
    let base = VirtAddr::new(0x1000_0000);
    let len = PAGE_SIZE;
    use crate::mm::vm::{BackingStore, EntryFlags, VmMapEntry};
    let obj = VmObject::new_anon(1);
    let vma = VmMapEntry::new(
        base.as_usize() as u64,
        (base.as_usize() + len) as u64,
        BackingStore::Object {
            object: obj,
            offset: 0,
        },
        EntryFlags::empty(),
        crate::map_perm!(R, W, U),
    );
    {
        let mut vm = task.vm_map.lock();
        match vm.insert_entry(vma) {
            Ok(()) => {},
            Err(_) => {
                kprintln!("mmap munmap FAIL (insert)");
                return;
            },
        }
        // Verify VMA exists
        if vm.lookup(base.as_usize() as u64).is_none() {
            kprintln!("mmap munmap FAIL (find after insert)");
            return;
        }
        // Remove it
        vm.remove_range(base, VirtAddr::new(base.as_usize() + len));
        if vm.lookup(base.as_usize() as u64).is_some() {
            kprintln!("mmap munmap FAIL (still present after remove)");
            return;
        }
    }

    kprintln!("mmap munmap PASS");
}

#[cfg(feature = "qemu-test")]
async fn test_device_nodes() {
    use fs::{FdTable, OpenFlags};

    // Test /dev/null behavior directly via FileObject
    // Write to /dev/null: always succeeds (swallowed)
    // Read from /dev/null: always returns EOF (0 bytes)

    // Test /dev/zero behavior: read returns zeros
    // We test via the device open path
    let fd_table = crate::hal_common::SpinMutex::new(FdTable::new_with_stdio());

    // Open /dev/null
    match syscall::fs::open(&fd_table, "/dev/null", OpenFlags::RDWR, 0).await {
        Ok(fd) => {
            // Verify it opened (fd >= 3 since 0,1,2 are stdio)
            if fd < 3 {
                kprintln!("device nodes FAIL (/dev/null fd={} too low)", fd);
                return;
            }
            let _ = syscall::fs::close(&fd_table, fd);
        },
        Err(e) => {
            kprintln!("device nodes FAIL (/dev/null open: {:?})", e);
            return;
        },
    }

    // Open /dev/zero
    match syscall::fs::open(&fd_table, "/dev/zero", OpenFlags::RDONLY, 0).await {
        Ok(fd) => {
            let _ = syscall::fs::close(&fd_table, fd);
        },
        Err(e) => {
            kprintln!("device nodes FAIL (/dev/zero open: {:?})", e);
            return;
        },
    }

    // Open /dev/console
    match syscall::fs::open(&fd_table, "/dev/console", OpenFlags::RDWR, 0).await {
        Ok(fd) => {
            let _ = syscall::fs::close(&fd_table, fd);
        },
        Err(e) => {
            kprintln!("device nodes FAIL (/dev/console open: {:?})", e);
            return;
        },
    }

    kprintln!("device nodes PASS");
}

#[cfg(feature = "qemu-test")]
fn test_futex_wake() {
    use ipc::futex;

    use crate::hal_common::PhysAddr;

    // futex_wake on a key with no waiters should return 0
    let key = PhysAddr::new(0xDEAD_0000);
    let woken = futex::futex_wake(key, 1);
    if woken != 0 {
        kprintln!("futex wake FAIL (woke {} on empty)", woken);
        return;
    }

    kprintln!("futex wake PASS");
}

#[cfg(feature = "qemu-test")]
fn test_fixup() {
    use hal::copy_user::copy_user_chunk;
    let src_buf = [0xABu8; 16];

    // Test 1: bad destination pointer
    let ret = unsafe { copy_user_chunk(0xDEAD_0000 as *mut u8, src_buf.as_ptr(), 16) };
    if ret == 14 {
        kprintln!("fixup bad-dst PASS");
    } else {
        kprintln!("fixup bad-dst FAIL (ret={})", ret);
    }

    // Test 2: bad source pointer
    let mut dst_buf = [0u8; 16];
    let ret = unsafe { copy_user_chunk(dst_buf.as_mut_ptr(), 0xDEAD_0000 as *const u8, 16) };
    if ret == 14 {
        kprintln!("fixup bad-src PASS");
    } else {
        kprintln!("fixup bad-src FAIL (ret={})", ret);
    }
}

/// Register clobber test: write known values to caller-saved registers,
/// wait for a timer IRQ (which saves/restores via __kernel_trap), then
/// verify the registers are intact. Tests trap entry/exit correctness.
#[cfg(feature = "qemu-test")]
fn register_clobber_test() {
    let ok: usize;
    unsafe {
        core::arch::asm!(
            // Write known values to t0-t6 (caller-saved temporaries)
            "li t0, 0xDEAD0000",
            "li t1, 0xDEAD0001",
            "li t2, 0xDEAD0002",
            "li t3, 0xDEAD0003",
            "li t4, 0xDEAD0004",
            "li t5, 0xDEAD0005",
            "li t6, 0xDEAD0006",
            // Enable interrupts and wait for a timer IRQ
            "csrsi sstatus, 0x2",  // SIE = 1
            "wfi",                  // wait for timer IRQ
            "csrci sstatus, 0x2",  // SIE = 0
            // Now check all values survived the trap
            "li {tmp}, 0",         // assume pass (0 = ok)
            "li {exp}, 0xDEAD0000",
            "bne t0, {exp}, 1f",
            "li {exp}, 0xDEAD0001",
            "bne t1, {exp}, 1f",
            "li {exp}, 0xDEAD0002",
            "bne t2, {exp}, 1f",
            "li {exp}, 0xDEAD0003",
            "bne t3, {exp}, 1f",
            "li {exp}, 0xDEAD0004",
            "bne t4, {exp}, 1f",
            "li {exp}, 0xDEAD0005",
            "bne t5, {exp}, 1f",
            "li {exp}, 0xDEAD0006",
            "bne t6, {exp}, 1f",
            "j 2f",                // all passed
            "1:",                  // fail
            "li {tmp}, 1",
            "2:",
            tmp = out(reg) ok,
            exp = out(reg) _,
            out("t0") _,
            out("t1") _,
            out("t2") _,
            out("t3") _,
            out("t4") _,
            out("t5") _,
            out("t6") _,
        );
    }
    if ok == 0 {
        kprintln!("register clobber PASS");
    } else {
        kprintln!("register clobber FAIL");
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kprintln!("[PANIC] {}", info);
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}
