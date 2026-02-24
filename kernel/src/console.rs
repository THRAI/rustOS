//! Console output via kprint!/kprintln! macros.
//!
//! Uses a global spinlock so SMP output is not interleaved.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};
use crate::hal::rv64::uart;

/// Global print lock — ensures only one hart writes at a time.
/// We use a raw AtomicBool instead of IrqSafeSpinLock to avoid
/// circular dependencies (print is used inside lock debug paths).
static PRINT_LOCK: AtomicBool = AtomicBool::new(false);

struct LockedUartWriter;

impl Write for LockedUartWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                uart::putchar(b'\r');
            }
            uart::putchar(b);
        }
        Ok(())
    }
}

pub fn _print(args: fmt::Arguments) {
    // Disable IRQs on this hart
    let saved: usize;
    unsafe {
        core::arch::asm!("csrrci {}, sstatus, 0x2", out(reg) saved);
    }
    // Acquire cross-hart spinlock
    while PRINT_LOCK.swap(true, Ordering::Acquire) {
        core::hint::spin_loop();
    }
    let _ = LockedUartWriter.write_fmt(args);
    // Release
    PRINT_LOCK.store(false, Ordering::Release);
    // Restore IRQs
    if saved & 0x2 != 0 {
        unsafe {
            core::arch::asm!("csrsi sstatus, 0x2");
        }
    }
}

/// Write a single byte to the UART (no locking, for syscall write path).
pub fn putchar(c: u8) {
    uart::putchar(c);
}

#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        $crate::console::_print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! kprintln {
    () => { $crate::kprint!("\n") };
    ($($arg:tt)*) => {
        $crate::console::_print(format_args!("{}\n", format_args!($($arg)*)))
    };
}

/// Structured kernel log macro.
///
/// `klog!(module, level, "fmt", args...)`
///
/// - `error`: always compiled in
/// - `info` / `debug`: only compiled when the module's `log-<module>` feature is enabled
#[macro_export]
macro_rules! klog {
    ($mod:ident, error, $($arg:tt)*) => {
        $crate::kprintln!(concat!("[", stringify!($mod), "] {}"), format_args!($($arg)*))
    };
    ($mod:ident, $lvl:ident, $($arg:tt)*) => {
        $crate::_klog_if!($mod, $($arg)*)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! _klog_if {
    (boot,    $($arg:tt)*) => { #[cfg(feature = "log-boot")]    { $crate::kprintln!(concat!("[boot] {}"),    format_args!($($arg)*)); } };
    (syscall, $($arg:tt)*) => { #[cfg(feature = "log-syscall")] { $crate::kprintln!(concat!("[syscall] {}"), format_args!($($arg)*)); } };
    (trap,    $($arg:tt)*) => { #[cfg(feature = "log-trap")]    { $crate::kprintln!(concat!("[trap] {}"),    format_args!($($arg)*)); } };
    (vm,      $($arg:tt)*) => { #[cfg(feature = "log-vm")]      { $crate::kprintln!(concat!("[vm] {}"),      format_args!($($arg)*)); } };
    (sched,   $($arg:tt)*) => { #[cfg(feature = "log-sched")]   { $crate::kprintln!(concat!("[sched] {}"),   format_args!($($arg)*)); } };
    (fs,      $($arg:tt)*) => { #[cfg(feature = "log-fs")]      { $crate::kprintln!(concat!("[fs] {}"),      format_args!($($arg)*)); } };
    (driver,  $($arg:tt)*) => { #[cfg(feature = "log-driver")]  { $crate::kprintln!(concat!("[driver] {}"),  format_args!($($arg)*)); } };
    (smp,     $($arg:tt)*) => { #[cfg(feature = "log-smp")]     { $crate::kprintln!(concat!("[smp] {}"),     format_args!($($arg)*)); } };
}
