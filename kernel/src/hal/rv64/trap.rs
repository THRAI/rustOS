//! rv64 trap setup and dispatch.

use core::sync::atomic::Ordering;
use hal_common::TrapFrame;

// Interrupt bit in scause (bit 63 on rv64)
const SCAUSE_INTERRUPT: usize = 1 << 63;

// Interrupt cause codes
const IRQ_S_SOFTWARE: usize = 1;
const IRQ_S_TIMER: usize = 5;
const IRQ_S_EXTERNAL: usize = 9;

// Exception cause codes
const EXC_LOAD_ACCESS_FAULT: usize = 5;
const EXC_STORE_ACCESS_FAULT: usize = 7;
const EXC_ECALL_U: usize = 8;
const EXC_INST_PAGE_FAULT: usize = 12;
const EXC_LOAD_PAGE_FAULT: usize = 13;
const EXC_STORE_PAGE_FAULT: usize = 15;

extern "C" {
    fn __kernel_trap();
    fn __user_trap();
}

/// Initialize trap infrastructure: set stvec, enable timer + software interrupts in sie.
pub fn init() {
    set_kernel_trap_entry();
    // Enable S-mode timer interrupt (STIE = bit 5), software interrupt (SSIE = bit 1),
    // and external interrupt (SEIE = bit 9) in sie.
    unsafe {
        core::arch::asm!("csrs sie, {}", in(reg) (1usize << 5) | (1usize << 1) | (1usize << 9));
    }
    klog!(trap, info, "stvec set, STIE+SSIE enabled");
}

/// Point stvec to __kernel_trap (Direct mode).
pub fn set_kernel_trap_entry() {
    unsafe {
        core::arch::asm!(
            "csrw stvec, {}",
            in(reg) __kernel_trap as usize,
        );
    }
}

/// Point stvec to __user_trap (stub for Phase 3).
pub fn set_user_trap_entry() {
    unsafe {
        core::arch::asm!(
            "csrw stvec, {}",
            in(reg) __user_trap as usize,
        );
    }
}

/// Rust trap handler called from __kernel_trap asm.
#[no_mangle]
pub extern "C" fn kernel_trap_handler(frame: &mut TrapFrame) {
    let scause = frame.scause();
    let is_interrupt = scause & SCAUSE_INTERRUPT != 0;
    let code = scause & !SCAUSE_INTERRUPT;

    if is_interrupt {
        match code {
            IRQ_S_TIMER => {
                super::timer::handle_timer_irq();
            }
            IRQ_S_EXTERNAL => {
                handle_external_irq();
            }
            IRQ_S_SOFTWARE => {
                super::ipi::handle_ipi();
            }
            _ => {
                panic!(
                    "[trap] unhandled interrupt: code={}, sepc={:#x}",
                    code,
                    frame.pc()
                );
            }
        }
    } else {
        match code {
            EXC_ECALL_U => {
                // Advance past ecall instruction before dispatch
                frame.set_pc(frame.pc() + 4);
                // Syscall dispatch: a7 = syscall number, a0-a5 = args
                let syscall_num = frame.arg(7);
                dispatch_syscall(frame, syscall_num);
            }
            EXC_LOAD_ACCESS_FAULT | EXC_STORE_ACCESS_FAULT |
            EXC_INST_PAGE_FAULT | EXC_LOAD_PAGE_FAULT | EXC_STORE_PAGE_FAULT => {
                // Check pcb_onfault for exception fixup (copy_user path)
                let pc = crate::executor::per_cpu::current();
                let onfault = pc.pcb_onfault.load(Ordering::Relaxed);
                if onfault != 0 {
                    // Redirect to landing pad: set sepc to onfault address
                    frame.set_pc(onfault);
                    // Clear onfault (landing pad also clears, but belt-and-suspenders)
                    pc.pcb_onfault.store(0, Ordering::Relaxed);
                    return;
                }
                // No fixup — real fault, panic
                panic!(
                    "[trap] page fault: cause={}, stval={:#x}, sepc={:#x}",
                    code,
                    frame.stval(),
                    frame.pc()
                );
            }
            _ => {
                panic!(
                    "[trap] unhandled exception: cause={}, stval={:#x}, sepc={:#x}",
                    code,
                    frame.stval(),
                    frame.pc()
                );
            }
        }
    }
}

/// Stub: external interrupt handler (expanded in later phases).
fn handle_external_irq() {
    let hart = crate::executor::per_cpu::current().hartid;
    // FreeBSD-style claim loop: handle all pending IRQs per trap entry
    loop {
        let irq = super::plic::claim(hart);
        if irq == 0 {
            break;
        }
        if irq == super::plic::UART_IRQ {
            // Drain entire UART FIFO (handles interrupt coalescing)
            while let Some(ch) = super::uart::getchar() {
                crate::console::console_irq_push(ch);
            }
        }
        super::plic::complete(hart, irq);
    }
}

// ---------------------------------------------------------------------------
// Syscall dispatch — Linux-compatible rv64 syscall numbers
// ---------------------------------------------------------------------------

// File I/O
const SYS_GETCWD: usize = 17;
const SYS_DUP: usize = 23;
const SYS_DUP3: usize = 24;
const SYS_FCNTL: usize = 25;
const SYS_IOCTL: usize = 29;
const SYS_MKDIRAT: usize = 34;
const SYS_UNLINKAT: usize = 35;
const SYS_LINKAT: usize = 37;
const SYS_OPENAT: usize = 56;
const SYS_CLOSE: usize = 57;
const SYS_PIPE2: usize = 59;
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
const SYS_KILL: usize = 129;
const SYS_SIGACTION: usize = 134;
const SYS_SIGPROCMASK: usize = 135;
const SYS_TIMES: usize = 153;
const SYS_UNAME: usize = 160;
const SYS_GETPID: usize = 172;
const SYS_GETPPID: usize = 173;
const SYS_GETUID: usize = 174;
const SYS_GETEUID: usize = 175;
const SYS_GETGID: usize = 176;
const SYS_GETEGID: usize = 177;
const SYS_BRK: usize = 214;
const SYS_MUNMAP: usize = 215;
const SYS_CLONE: usize = 220;
const SYS_EXECVE: usize = 221;
const SYS_MMAP: usize = 222;
const SYS_MPROTECT: usize = 226;
const SYS_WAIT4: usize = 260;

/// Dispatch ecall from U-mode. a7 = syscall number, a0-a5 = args.
/// Returns the syscall result in a0 (frame.x[10]).
pub fn dispatch_syscall(frame: &mut TrapFrame, syscall_num: usize) {
    let a0 = frame.arg(0);
    let a1 = frame.arg(1);
    let a2 = frame.arg(2);
    let _a3 = frame.arg(3);
    let _a4 = frame.arg(4);
    let _a5 = frame.arg(5);

    let ret: usize = match syscall_num {
        SYS_GETCWD => {
            // Stub: write "/" to user buffer
            if a1 >= 2 {
                let buf = a0 as *mut u8;
                unsafe {
                    buf.write(b'/');
                    buf.add(1).write(0);
                }
                a0 // return buf pointer on success
            } else {
                (-34isize) as usize // ERANGE
            }
        }
        SYS_DUP => {
            // Stub: return same fd (minimal dup)
            a0
        }
        SYS_DUP3 => {
            // Stub: return newfd
            a1
        }
        SYS_FCNTL => 0,   // stub success
        SYS_IOCTL => 0,   // stub success
        SYS_MKDIRAT | SYS_UNLINKAT | SYS_LINKAT => {
            (-38isize) as usize // ENOSYS — read-only FS
        }
        SYS_OPENAT => {
            // a0=dirfd, a1=pathname, a2=flags, a3=mode
            // Minimal: treat as open(pathname, flags)
            // The actual async open is handled by the executor task;
            // here we just record the syscall args and return -EAGAIN
            // to signal the executor to handle it asynchronously.
            // For synchronous stub: return fd 3+ or error
            klog!(syscall, debug, "openat(dirfd={}, path={:#x}, flags={:#x})", a0 as isize, a1, a2);
            (-38isize) as usize // handled async by executor
        }
        SYS_CLOSE => {
            // a0 = fd
            0 // stub success
        }
        SYS_PIPE2 => {
            (-38isize) as usize // ENOSYS for now
        }
        SYS_GETDENTS64 => {
            (-38isize) as usize // ENOSYS for now
        }
        SYS_LSEEK => {
            0 // stub: return 0
        }
        SYS_READ => {
            // a0=fd, a1=buf, a2=count — async, handled by executor
            klog!(syscall, debug, "read(fd={}, buf={:#x}, count={})", a0, a1, a2);
            (-38isize) as usize // handled async by executor
        }
        SYS_WRITE => {
            // a0=fd, a1=buf, a2=count
            // For stdout/stderr (fd 1,2): write to UART directly
            if a0 == 1 || a0 == 2 {
                let buf = a1 as *const u8;
                let len = a2;
                for i in 0..len {
                    let ch = unsafe { buf.add(i).read() };
                    crate::console::putchar(ch);
                }
                len
            } else {
                (-38isize) as usize // ENOSYS for other fds
            }
        }
        SYS_WRITEV => {
            // a0=fd, a1=iov, a2=iovcnt
            // For stdout/stderr: iterate iovec and write each
            if a0 == 1 || a0 == 2 {
                let iov_base = a1;
                let iovcnt = a2;
                let mut total = 0usize;
                for i in 0..iovcnt {
                    let iov_ptr = (iov_base + i * 16) as *const usize;
                    let base = unsafe { iov_ptr.read() } as *const u8;
                    let len = unsafe { iov_ptr.add(1).read() };
                    for j in 0..len {
                        let ch = unsafe { base.add(j).read() };
                        crate::console::putchar(ch);
                    }
                    total += len;
                }
                total
            } else {
                (-38isize) as usize
            }
        }
        SYS_FSTAT | SYS_FSTATAT => {
            // Minimal stat: zero out the stat buffer
            if a1 != 0 {
                let buf = a1 as *mut u8;
                unsafe {
                    core::ptr::write_bytes(buf, 0, 128); // zero first 128 bytes
                }
            }
            0
        }
        SYS_EXIT | SYS_EXIT_GROUP => {
            let code = a0 as i32;
            klog!(syscall, debug, "exit({})", code);
            // Signal executor via special return value
            // The executor checks for this and handles task termination
            frame.set_ret_val(code as usize);
            return; // Don't overwrite a0
        }
        SYS_SET_TID_ADDRESS => {
            // Return current tid (pid)
            1 // stub: return pid 1
        }
        SYS_CLOCK_GETTIME => {
            // a0=clockid, a1=timespec ptr
            // Stub: write zeros
            if a1 != 0 {
                let ts = a1 as *mut u64;
                unsafe {
                    ts.write(0);          // tv_sec
                    ts.add(1).write(0);   // tv_nsec
                }
            }
            0
        }
        SYS_SCHED_YIELD => 0,
        SYS_KILL => 0,          // stub
        SYS_SIGACTION => 0,     // stub
        SYS_SIGPROCMASK => 0,   // stub
        SYS_TIMES => {
            // a0 = tms ptr; stub: zero it out
            if a0 != 0 {
                let tms = a0 as *mut u64;
                unsafe {
                    core::ptr::write_bytes(tms, 0, 4); // 4 fields
                }
            }
            0
        }
        SYS_UNAME => {
            // a0 = utsname ptr (65-byte fields)
            if a0 != 0 {
                let buf = a0 as *mut u8;
                unsafe {
                    core::ptr::write_bytes(buf, 0, 65 * 6);
                    // sysname
                    let sysname = b"Linux\0";
                    core::ptr::copy_nonoverlapping(sysname.as_ptr(), buf, sysname.len());
                    // nodename
                    let nodename = b"chronix\0";
                    core::ptr::copy_nonoverlapping(nodename.as_ptr(), buf.add(65), nodename.len());
                    // release
                    let release = b"5.10.0\0";
                    core::ptr::copy_nonoverlapping(release.as_ptr(), buf.add(130), release.len());
                    // version
                    let version = b"#1\0";
                    core::ptr::copy_nonoverlapping(version.as_ptr(), buf.add(195), version.len());
                    // machine
                    let machine = b"riscv64\0";
                    core::ptr::copy_nonoverlapping(machine.as_ptr(), buf.add(260), machine.len());
                }
            }
            0
        }
        SYS_GETPID => 1,    // stub: always pid 1 for init
        SYS_GETPPID => 0,   // init has no parent
        SYS_GETUID | SYS_GETEUID | SYS_GETGID | SYS_GETEGID => 0,
        SYS_BRK => {
            // a0 = new brk address (0 = query current)
            // Stub: return current brk (just return the requested address)
            if a0 == 0 {
                0x0000_0010_0000_0000usize // default heap start
            } else {
                a0 // accept any brk
            }
        }
        SYS_MUNMAP => 0,    // stub success
        SYS_CLONE => {
            // Stub: return 0 (child) — real fork handled by executor
            klog!(syscall, debug, "clone(flags={:#x})", a0);
            (-38isize) as usize // handled async by executor
        }
        SYS_EXECVE => {
            // a0=pathname, a1=argv, a2=envp — async, handled by executor
            klog!(syscall, debug, "execve(path={:#x})", a0);
            (-38isize) as usize // handled async by executor
        }
        SYS_MMAP => {
            // a0=addr, a1=len, a2=prot, a3=flags, a4=fd, a5=offset
            // Stub: anonymous mmap returns a fixed address range
            let len = a1;
            if len == 0 {
                (-22isize) as usize // EINVAL
            } else {
                // Return a simple bump-allocated address in user space
                static MMAP_NEXT: core::sync::atomic::AtomicUsize =
                    core::sync::atomic::AtomicUsize::new(0x0000_0020_0000_0000);
                let addr = MMAP_NEXT.fetch_add(
                    (len + 0xFFF) & !0xFFF,
                    core::sync::atomic::Ordering::Relaxed,
                );
                addr
            }
        }
        SYS_MPROTECT => 0,  // stub success
        SYS_WAIT4 => {
            // a0=pid, a1=wstatus, a2=options — async, handled by executor
            klog!(syscall, debug, "wait4(pid={})", a0 as isize);
            (-38isize) as usize // handled async by executor
        }
        _ => {
            klog!(syscall, info, "unimplemented syscall {}", syscall_num);
            (-38isize) as usize // ENOSYS
        }
    };

    frame.set_ret_val(ret);
}
