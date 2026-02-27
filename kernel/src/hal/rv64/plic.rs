//! PLIC (Platform-Level Interrupt Controller) driver for QEMU virt (rv64).
//!
//! QEMU virt PLIC base: 0x0C00_0000. UART IRQ = 10.
//! Context = 2 * hartid + 1 (S-mode for each hart).

const PLIC_BASE: usize = 0x0C00_0000;

/// UART IRQ number on QEMU virt.
pub const UART_IRQ: u32 = 10;

fn read_reg(addr: usize) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

fn write_reg(addr: usize, val: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

/// S-mode context ID for a given hart.
fn context(hart: usize) -> usize {
    2 * hart + 1
}

/// Set priority for an IRQ source.
fn set_priority(irq: u32, priority: u32) {
    write_reg(PLIC_BASE + (irq as usize) * 4, priority);
}

/// Enable an IRQ for S-mode on the given hart.
fn enable_irq(hart: usize, irq: u32) {
    let ctx = context(hart);
    let addr = PLIC_BASE + 0x2000 + ctx * 0x80 + ((irq / 32) as usize) * 4;
    let val = read_reg(addr);
    write_reg(addr, val | (1 << (irq % 32)));
}

/// Set the priority threshold for S-mode on the given hart.
fn set_threshold(hart: usize, threshold: u32) {
    let ctx = context(hart);
    write_reg(PLIC_BASE + 0x20_0000 + ctx * 0x1000, threshold);
}

/// Claim the highest-priority pending IRQ. Returns 0 if none.
pub fn claim(hart: usize) -> u32 {
    let ctx = context(hart);
    read_reg(PLIC_BASE + 0x20_0004 + ctx * 0x1000)
}

/// Signal completion of an IRQ.
pub fn complete(hart: usize, irq: u32) {
    let ctx = context(hart);
    write_reg(PLIC_BASE + 0x20_0004 + ctx * 0x1000, irq);
}

/// Initialize PLIC for UART IRQ on the given hart.
pub fn init_hart(hart: usize) {
    set_priority(UART_IRQ, 1);
    enable_irq(hart, UART_IRQ);
    set_threshold(hart, 0);
}
