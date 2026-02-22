#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;

mod alloc_early;
mod hal;
#[macro_use]
mod console;
mod trap;

// Include boot assembly
global_asm!(include_str!("hal/rv64/boot.S"));
// Include trap assembly
global_asm!(include_str!("hal/rv64/trap.S"));

/// Entry point called from boot.S
/// a0 = hartid, a1 = dtb_ptr
#[no_mangle]
pub extern "C" fn rust_main(hartid: usize, dtb_ptr: usize) -> ! {
    if hartid == 0 {
        hal::rv64::uart::init();
        kprintln!("hello world");
        kprintln!("[kernel] hart {} booting, dtb @ {:#x}", hartid, dtb_ptr);

        // Initialize trap infrastructure (stvec + STIE)
        trap::init();

        // Enable global interrupts
        hal::rv64::irq::enable();
        kprintln!("[kernel] interrupts enabled, entering wfi loop");
    }

    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    kprintln!("[PANIC] {}", info);
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}
