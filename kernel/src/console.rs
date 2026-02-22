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
