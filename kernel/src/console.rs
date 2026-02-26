//! Console output via kprint!/kprintln! macros + IRQ-driven input buffer.
//!
//! Output uses a global spinlock so SMP output is not interleaved.
//! Input is a transparent byte pipe: UART IRQ pushes bytes, ConsoleReadFuture drains them.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::Waker;
use crate::hal::rv64::uart;
use hal_common::IrqSafeSpinLock;

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

// ---------------------------------------------------------------------------
// Console input buffer — transparent byte pipe (raw mode)
// ---------------------------------------------------------------------------

const CONSOLE_BUF_SIZE: usize = 256;

struct ConsoleInputBuffer {
    data: [u8; CONSOLE_BUF_SIZE],
    head: usize,
    tail: usize,
    len: usize,
    waker: Option<Waker>,
}

impl ConsoleInputBuffer {
    const fn new() -> Self {
        Self {
            data: [0; CONSOLE_BUF_SIZE],
            head: 0,
            tail: 0,
            len: 0,
            waker: None,
        }
    }

    fn push(&mut self, ch: u8) {
        if self.len < CONSOLE_BUF_SIZE {
            self.data[self.tail] = ch;
            self.tail = (self.tail + 1) % CONSOLE_BUF_SIZE;
            self.len += 1;
        }
        // Drop byte on overflow (no flow control)
    }

    fn drain(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.len);
        for i in 0..n {
            buf[i] = self.data[self.head];
            self.head = (self.head + 1) % CONSOLE_BUF_SIZE;
        }
        self.len -= n;
        n
    }
}

static CONSOLE_INPUT: IrqSafeSpinLock<ConsoleInputBuffer> =
    IrqSafeSpinLock::new(ConsoleInputBuffer::new());

/// Called from UART IRQ handler — push one byte, wake reader if parked.
pub fn console_irq_push(ch: u8) {
    let mut buf = CONSOLE_INPUT.lock();
    buf.push(ch);
    if let Some(w) = buf.waker.take() {
        w.wake();
    }
}

/// Drain available bytes from console buffer. Returns 0 if empty.
pub fn console_read(out: &mut [u8]) -> usize {
    let mut buf = CONSOLE_INPUT.lock();
    buf.drain(out)
}

/// Register a waker to be woken when input arrives.
pub fn console_register_waker(waker: &Waker) {
    let mut buf = CONSOLE_INPUT.lock();
    buf.waker = Some(waker.clone());
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
    (signal,  $($arg:tt)*) => { #[cfg(feature = "log-signal")]  { $crate::kprintln!(concat!("[signal] {}"),  format_args!($($arg)*)); } };
    (exec,    $($arg:tt)*) => { #[cfg(feature = "log-exec")]    { $crate::kprintln!(concat!("[exec] {}"),    format_args!($($arg)*)); } };
    (pipe,    $($arg:tt)*) => { #[cfg(feature = "log-pipe")]    { $crate::kprintln!(concat!("[pipe] {}"),    format_args!($($arg)*)); } };
    (proc,    $($arg:tt)*) => { #[cfg(feature = "log-proc")]    { $crate::kprintln!(concat!("[proc] {}"),    format_args!($($arg)*)); } };
}
