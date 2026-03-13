//! UART 16550A driver with FDT-discovered base address.
//!
//! During early boot, the UART is initialized at the QEMU virt default
//! address (0x1000_0000). After FDT parsing, `reinit()` updates the base
//! address to the FDT-discovered value (which is the same on QEMU virt,
//! but may differ on real hardware).

use core::{
    fmt,
    sync::atomic::{AtomicUsize, Ordering},
};

/// UART base address. Starts with QEMU virt default; updated by `reinit()`.
static UART_BASE: AtomicUsize = AtomicUsize::new(0x1000_0000);

// 16550 register offsets
const RBR: usize = 0; // Receive Buffer Register (read)
const THR: usize = 0; // Transmit Holding Register (write)
const IER: usize = 1; // Interrupt Enable Register
const FCR: usize = 2; // FIFO Control Register (write)
const LCR: usize = 3; // Line Control Register
const LSR: usize = 5; // Line Status Register
const DLL: usize = 0; // Divisor Latch Low (when DLAB=1)
const DLH: usize = 1; // Divisor Latch High (when DLAB=1)

// LSR bits
const LSR_RX_READY: u8 = 1 << 0;
const LSR_TX_EMPTY: u8 = 1 << 5;

// LCR bits
const LCR_DLAB: u8 = 1 << 7;
const LCR_8N1: u8 = 0b11; // 8 data bits, no parity, 1 stop bit

pub struct Uart;

impl Uart {
    fn base() -> usize {
        UART_BASE.load(Ordering::Relaxed)
    }

    fn read_reg(offset: usize) -> u8 {
        let ptr = (Self::base() + offset) as *const u8;
        // SAFETY: MMIO register access at a known-valid hardware address.
        unsafe { core::ptr::read_volatile(ptr) }
    }

    fn write_reg(offset: usize, val: u8) {
        let ptr = (Self::base() + offset) as *mut u8;
        // SAFETY: MMIO register access at a known-valid hardware address.
        unsafe { core::ptr::write_volatile(ptr, val) }
    }
}

/// Initialize UART: set baud rate divisor, enable FIFO, 8N1.
pub fn init() {
    // Disable interrupts
    Uart::write_reg(IER, 0x00);
    // Enable DLAB to set baud rate
    Uart::write_reg(LCR, LCR_DLAB);
    // Set divisor to 1 (115200 baud with 1.8432 MHz clock -- QEMU ignores this)
    Uart::write_reg(DLL, 0x01);
    Uart::write_reg(DLH, 0x00);
    // 8N1, disable DLAB
    Uart::write_reg(LCR, LCR_8N1);
    // Enable FIFO, clear TX/RX, 14-byte threshold
    Uart::write_reg(FCR, 0xC7);
    // Enable receive interrupts (for later use)
    Uart::write_reg(IER, 0x01);
}

/// Re-initialize UART with an FDT-discovered base address.
///
/// Called after `parse_fdt()` to switch from the hardcoded default to the
/// FDT-discovered address. On QEMU virt this is a no-op (same address),
/// but on real hardware the address may differ.
pub fn reinit(base: usize) {
    let old = UART_BASE.swap(base, Ordering::Relaxed);
    if old != base {
        // Re-run init sequence at the new address
        init();
    }
}

/// Write a single byte, spin-waiting on THR empty.
pub fn putchar(c: u8) {
    while Uart::read_reg(LSR) & LSR_TX_EMPTY == 0 {
        core::hint::spin_loop();
    }
    Uart::write_reg(THR, c);
}

/// Non-blocking read from RBR. Returns None if no data available.
pub fn getchar() -> Option<u8> {
    if Uart::read_reg(LSR) & LSR_RX_READY != 0 {
        Some(Uart::read_reg(RBR))
    } else {
        None
    }
}

/// Wrapper for core::fmt::Write
pub struct UartWriter;

impl fmt::Write for UartWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                putchar(b'\r');
            }
            putchar(b);
        }
        Ok(())
    }
}
