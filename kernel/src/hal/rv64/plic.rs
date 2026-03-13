//! PLIC (Platform-Level Interrupt Controller) driver with FDT-discovered addresses.
//!
//! Base address and UART IRQ number are read from `platform()` (populated
//! by `parse_fdt()`). Falls back to QEMU virt defaults if FDT parsing
//! hasn't run yet (should not happen in normal boot).

use super::fdt::platform;

fn plic_base() -> usize {
    platform().plic_base
}

fn uart_irq() -> u32 {
    platform().uart_irq
}

/// Return the UART IRQ number (for use in trap handler).
pub fn get_uart_irq() -> u32 {
    uart_irq()
}

fn read_reg(addr: usize) -> u32 {
    // SAFETY: MMIO register access at an FDT-discovered hardware address.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

fn write_reg(addr: usize, val: u32) {
    // SAFETY: MMIO register access at an FDT-discovered hardware address.
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

/// S-mode context ID for a given hart.
fn context(hart: usize) -> usize {
    2 * hart + 1
}

/// Set priority for an IRQ source.
fn set_priority(irq: u32, priority: u32) {
    write_reg(plic_base() + (irq as usize) * 4, priority);
}

/// Enable an IRQ for S-mode on the given hart.
fn enable_irq(hart: usize, irq: u32) {
    let ctx = context(hart);
    let addr = plic_base() + 0x2000 + ctx * 0x80 + ((irq / 32) as usize) * 4;
    let val = read_reg(addr);
    write_reg(addr, val | (1 << (irq % 32)));
}

/// Set the priority threshold for S-mode on the given hart.
fn set_threshold(hart: usize, threshold: u32) {
    let ctx = context(hart);
    write_reg(plic_base() + 0x20_0000 + ctx * 0x1000, threshold);
}

/// Claim the highest-priority pending IRQ. Returns 0 if none.
pub fn claim(hart: usize) -> u32 {
    let ctx = context(hart);
    read_reg(plic_base() + 0x20_0004 + ctx * 0x1000)
}

/// Signal completion of an IRQ.
pub fn complete(hart: usize, irq: u32) {
    let ctx = context(hart);
    write_reg(plic_base() + 0x20_0004 + ctx * 0x1000, irq);
}

/// Initialize PLIC for UART IRQ on the given hart.
pub fn init_hart(hart: usize) {
    set_priority(uart_irq(), 1);
    enable_irq(hart, uart_irq());
    set_threshold(hart, 0);
}
