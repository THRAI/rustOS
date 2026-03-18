#![no_std]

use core::arch::global_asm;

mod console;
mod syscall;

#[cfg(target_arch = "riscv64")]
global_asm!(include_str!("entry-rv64.S"));

#[cfg(target_arch = "loongarch64")]
global_asm!(include_str!("entry-la64.S"));

pub use syscall::{execve, exit, fork, getpid, shutdown, wait, waitpid, write, yield_};

pub fn print(args: core::fmt::Arguments<'_>) {
    console::print(args);
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::print(format_args!("\n"))
    };
    ($fmt:literal $(, $($arg:tt)+)?) => {
        $crate::print(format_args!(concat!($fmt, "\n") $(, $($arg)+)?))
    };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("panic: {}", info);
    exit(1)
}
