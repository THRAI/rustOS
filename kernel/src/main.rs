#![no_std]
#![no_main]

use core::arch::global_asm;

mod hal;

// Include boot assembly
global_asm!(include_str!("hal/rv64/boot.S"));

/// Minimal UART write for early boot -- QEMU virt UART at 0x1000_0000
fn uart_putchar(c: u8) {
    let uart_base = 0x1000_0000 as *mut u8;
    unsafe {
        core::ptr::write_volatile(uart_base, c);
    }
}

fn uart_puts(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            uart_putchar(b'\r');
        }
        uart_putchar(b);
    }
}

/// Entry point called from boot.S
/// a0 = hartid, a1 = dtb_ptr
#[no_mangle]
pub extern "C" fn rust_main(hartid: usize, _dtb_ptr: usize) -> ! {
    if hartid == 0 {
        uart_puts("hello world\n");
    }

    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}
