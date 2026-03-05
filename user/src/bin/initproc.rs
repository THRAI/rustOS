#![no_std]
#![no_main]

#[macro_use]
extern crate user_lib;

use user_lib::{execve, fork, getpid, wait, yield_};

#[cfg(not(feature = "autotest"))]
fn run_init_payload() -> i32 {
    let path = b"/bin/busybox\0";
    let arg0 = b"busybox\0";
    let arg1 = b"sh\0";
    let env0 = b"PATH=/bin:/sbin:/usr/bin:/usr/sbin\0";
    let env1 = b"HOME=/\0";

    let argv = [
        arg0.as_ptr(),
        arg1.as_ptr(),
        core::ptr::null(),
    ];
    let envp = [
        env0.as_ptr(),
        env1.as_ptr(),
        core::ptr::null(),
    ];

    execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr()) as i32
}

#[cfg(feature = "autotest")]
fn run_init_payload() -> i32 {
    let path = b"/riscv/musl/busybox\0";
    let arg0 = b"busybox\0";
    let arg1 = b"sh\0";
    let arg2 = b"/riscv/run-oj.sh\0";
    let env0 = b"PATH=/riscv/musl:/riscv/glibc:/bin:/sbin\0";
    let env1 = b"HOME=/\0";

    let argv = [
        arg0.as_ptr(),
        arg1.as_ptr(),
        arg2.as_ptr(),
        core::ptr::null(),
    ];
    let envp = [
        env0.as_ptr(),
        env1.as_ptr(),
        core::ptr::null(),
    ];

    execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr()) as i32
}

#[no_mangle]
fn main() -> i32 {
    println!("[initproc] started pid={}", getpid());

    let pid = fork();
    if pid == 0 {
        let ret = run_init_payload();
        println!("[initproc] exec failed: {}", ret);
        return 127;
    }

    loop {
        let mut status = 0i32;
        let child = wait(&mut status);
        if child > 0 {
            println!(
                "[initproc] released child pid={} status={:#x}",
                child,
                status
            );
            #[cfg(feature = "autotest")]
            {
                println!("[initproc] autotest finished, shutting down");
                let _ = user_lib::shutdown();
                return 0;
            }
        } else {
            let _ = yield_();
        }
    }
}
