//! Console output via kprint!/kprintln! macros.
//!
//! Uses the UART driver with IRQ-safe locking for concurrent access.

use core::fmt::{self, Write};
use crate::hal::rv64::uart;

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
    // Disable IRQs during print to prevent interleaved output
    let saved: usize;
    unsafe {
        core::arch::asm!("csrrci {}, sstatus, 0x2", out(reg) saved);
    }
    let _ = LockedUartWriter.write_fmt(args);
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
