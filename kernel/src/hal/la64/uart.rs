use core::sync::atomic::{AtomicUsize, Ordering};

static UART_BASE: AtomicUsize = AtomicUsize::new(0x1fe0_01e0);

const UART_RX: usize = 0x0;
const UART_TX: usize = 0x0;
const UART_IER: usize = 0x1;
const UART_FCR: usize = 0x2;
const UART_LCR: usize = 0x3;
const UART_LSR: usize = 0x5;
const UART_DLL: usize = 0x0;
const UART_DLM: usize = 0x1;

const LCR_DLAB: u8 = 1 << 7;
const LCR_8N1: u8 = 0b11;
const LSR_DATA_READY: u8 = 1 << 0;
const LSR_THR_EMPTY: u8 = 1 << 5;

#[inline]
fn base() -> usize {
    UART_BASE.load(Ordering::Relaxed)
}

#[inline]
fn read_reg(offset: usize) -> u8 {
    let ptr = (base() + offset) as *const u8;
    unsafe { core::ptr::read_volatile(ptr) }
}

#[inline]
fn write_reg(offset: usize, value: u8) {
    let ptr = (base() + offset) as *mut u8;
    unsafe { core::ptr::write_volatile(ptr, value) }
}

pub fn init() {
    write_reg(UART_IER, 0x00);
    write_reg(UART_LCR, LCR_DLAB);
    write_reg(UART_DLL, 0x01);
    write_reg(UART_DLM, 0x00);
    write_reg(UART_LCR, LCR_8N1);
    write_reg(UART_FCR, 0x07);
}

pub fn reinit(base: usize) {
    let old = UART_BASE.swap(base, Ordering::Relaxed);
    if old != base {
        init();
    }
}

pub fn putchar(c: u8) {
    while read_reg(UART_LSR) & LSR_THR_EMPTY == 0 {
        core::hint::spin_loop();
    }
    write_reg(UART_TX, c);
}

pub fn getchar() -> Option<u8> {
    if read_reg(UART_LSR) & LSR_DATA_READY != 0 {
        Some(read_reg(UART_RX))
    } else {
        None
    }
}
