use core::arch::asm;

const SYS_WRITE: usize = 64;
const SYS_EXIT: usize = 93;
const SYS_EXIT_GROUP: usize = 94;
const SYS_SCHED_YIELD: usize = 124;
const SYS_GETPID: usize = 172;
const SYS_CLONE: usize = 220;
const SYS_EXECVE: usize = 221;
const SYS_WAIT4: usize = 260;
const SYS_REBOOT: usize = 142;

#[inline(always)]
fn syscall(id: usize, args: [usize; 6]) -> isize {
    #[cfg(target_arch = "riscv64")]
    {
        let mut ret: isize;
        unsafe {
            asm!(
                "ecall",
                inlateout("a0") args[0] => ret,
                in("a1") args[1],
                in("a2") args[2],
                in("a3") args[3],
                in("a4") args[4],
                in("a5") args[5],
                in("a7") id,
            );
        }
        return ret;
    }

    #[cfg(target_arch = "loongarch64")]
    {
        let mut a0 = args[0] as isize;
        unsafe {
            asm!(
                "syscall 0",
                inlateout("$a0") a0,
                in("$a1") args[1],
                in("$a2") args[2],
                in("$a3") args[3],
                in("$a4") args[4],
                in("$a5") args[5],
                in("$a7") id,
                lateout("$t0") _,
                lateout("$t1") _,
                lateout("$t2") _,
                lateout("$t3") _,
                lateout("$t4") _,
                lateout("$t5") _,
                lateout("$t6") _,
                lateout("$t7") _,
                lateout("$t8") _,
                options(nostack),
            );
        }
        return a0;
    }

    #[allow(unreachable_code)]
    let mut ret: isize;
    ret = -1;
    ret
}

pub fn write(fd: usize, buf: &[u8]) -> isize {
    syscall(SYS_WRITE, [fd, buf.as_ptr() as usize, buf.len(), 0, 0, 0])
}

pub fn fork() -> isize {
    syscall(SYS_CLONE, [0, 0, 0, 0, 0, 0])
}

pub fn execve(path: *const u8, argv: *const *const u8, envp: *const *const u8) -> isize {
    syscall(
        SYS_EXECVE,
        [path as usize, argv as usize, envp as usize, 0, 0, 0],
    )
}

pub fn waitpid(pid: isize, status: *mut i32, options: usize) -> isize {
    syscall(SYS_WAIT4, [pid as usize, status as usize, options, 0, 0, 0])
}

pub fn wait(status: &mut i32) -> isize {
    waitpid(-1, status as *mut i32, 0)
}

pub fn yield_() -> isize {
    syscall(SYS_SCHED_YIELD, [0, 0, 0, 0, 0, 0])
}

pub fn getpid() -> isize {
    syscall(SYS_GETPID, [0, 0, 0, 0, 0, 0])
}

pub fn shutdown() -> isize {
    syscall(SYS_REBOOT, [0, 0, 0, 0, 0, 0])
}

pub fn exit(code: i32) -> ! {
    let _ = syscall(SYS_EXIT, [code as usize, 0, 0, 0, 0, 0]);
    let _ = syscall(SYS_EXIT_GROUP, [code as usize, 0, 0, 0, 0, 0]);
    loop {
        let _ = yield_();
    }
}
