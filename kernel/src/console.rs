//! Console output via kprint!/kprintln! macros + IRQ-driven input buffer.
//!
//! Output uses a global spinlock so SMP output is not interleaved.
//! Input is a transparent byte pipe: UART IRQ pushes bytes, ConsoleReadFuture drains them.

use core::{
    fmt::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
    task::Waker,
};

use crate::{hal::uart, hal_common::IrqSafeSpinLock};

/// Global print lock — ensures only one hart writes at a time.
/// We use a raw AtomicBool instead of IrqSafeSpinLock to avoid
/// circular dependencies (print is used inside lock debug paths).
static PRINT_LOCK: AtomicBool = AtomicBool::new(false);

/// Acquire the print lock with IRQs disabled, run `f`, then release.
///
/// This is intentionally a raw AtomicBool (not IrqSafeSpinLock) because
/// `_print` is called from inside IrqSafeSpinLock debug paths — using
/// IrqSafeSpinLock here would create a recursive deadlock.
#[inline(always)]
fn with_print_lock<F: FnOnce()>(f: F) {
    // SAFETY: disable SIE bit in sstatus, saving previous value.
    let saved: usize;
    unsafe {
        core::arch::asm!("csrrci {}, sstatus, 0x2", out(reg) saved);
    }
    while PRINT_LOCK.swap(true, Ordering::Acquire) {
        core::hint::spin_loop();
    }
    f();
    PRINT_LOCK.store(false, Ordering::Release);
    // SAFETY: restore SIE bit if it was set before.
    if saved & 0x2 != 0 {
        unsafe {
            core::arch::asm!("csrsi sstatus, 0x2");
        }
    }
}

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
    with_print_lock(|| {
        let _ = LockedUartWriter.write_fmt(args);
    });
}

/// Write a single byte to the UART (with locking for atomic output).
pub fn putchar(c: u8) {
    with_print_lock(|| {
        uart::putchar(c);
    });
}

/// Write multiple bytes to the UART atomically (with locking).
pub fn putchars(bytes: &[u8]) {
    with_print_lock(|| {
        for &b in bytes {
            uart::putchar(b);
        }
    });
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
        for b in buf.iter_mut().take(n) {
            *b = self.data[self.head];
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
        if cfg!(any(not(any(feature="log-level-error", feature="log-level-warn", feature="log-level-info", feature="log-level-debug", feature="log-level-trace")), feature="log-level-error")) {
            $crate::kprintln!(concat!("[", stringify!($mod), "] ERROR: {}"), format_args!($($arg)*))
        }
    };
    ($mod:ident, warn, $($arg:tt)*) => {
        if cfg!(any(not(any(feature="log-level-error", feature="log-level-warn", feature="log-level-info", feature="log-level-debug", feature="log-level-trace")), feature="log-level-warn")) {
            $crate::_klog_if!($mod, "WARN: ", $($arg)*)
        }
    };
    ($mod:ident, info, $($arg:tt)*) => {
        if cfg!(any(not(any(feature="log-level-error", feature="log-level-warn", feature="log-level-info", feature="log-level-debug", feature="log-level-trace")), feature="log-level-info")) {
            $crate::_klog_if!($mod, "INFO: ", $($arg)*)
        }
    };
    ($mod:ident, debug, $($arg:tt)*) => {
        if cfg!(any(not(any(feature="log-level-error", feature="log-level-warn", feature="log-level-info", feature="log-level-debug", feature="log-level-trace")), feature="log-level-debug")) {
            $crate::_klog_if!($mod, "DEBUG: ", $($arg)*)
        }
    };
    ($mod:ident, trace, $($arg:tt)*) => {
        if cfg!(any(not(any(feature="log-level-error", feature="log-level-warn", feature="log-level-info", feature="log-level-debug", feature="log-level-trace")), feature="log-level-trace")) {
            $crate::_klog_if!($mod, "TRACE: ", $($arg)*)
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! _klog_if {
    (boot,    $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-boot")    { $crate::kprintln!(concat!("[boot] ", $lvl, "{}"),    format_args!($($arg)*)); } };
    (syscall, $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-syscall") { $crate::kprintln!(concat!("[syscall] ", $lvl, "{}"), format_args!($($arg)*)); } };
    (trap,    $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-trap")    { $crate::kprintln!(concat!("[trap] ", $lvl, "{}"),    format_args!($($arg)*)); } };
    (vm,      $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-vm")      { $crate::kprintln!(concat!("[vm] ", $lvl, "{}"),      format_args!($($arg)*)); } };
    (sched,   $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-sched")   { $crate::kprintln!(concat!("[sched] ", $lvl, "{}"),   format_args!($($arg)*)); } };
    (fs,      $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-fs")      { $crate::kprintln!(concat!("[fs] ", $lvl, "{}"),      format_args!($($arg)*)); } };
    (driver,  $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-driver")  { $crate::kprintln!(concat!("[driver] ", $lvl, "{}"),  format_args!($($arg)*)); } };
    (smp,     $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-smp")     { $crate::kprintln!(concat!("[smp] ", $lvl, "{}"),     format_args!($($arg)*)); } };
    (signal,  $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-signal")  { $crate::kprintln!(concat!("[signal] ", $lvl, "{}"),  format_args!($($arg)*)); } };
    (exec,    $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-exec")    { $crate::kprintln!(concat!("[exec] ", $lvl, "{}"),    format_args!($($arg)*)); } };
    (pipe,    $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-pipe")    { $crate::kprintln!(concat!("[pipe] ", $lvl, "{}"),    format_args!($($arg)*)); } };
    (proc,    $lvl:literal, $($arg:tt)*) => { if cfg!(feature = "log-proc")    { $crate::kprintln!(concat!("[proc] ", $lvl, "{}"),    format_args!($($arg)*)); } };
}
