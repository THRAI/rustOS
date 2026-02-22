#![no_std]
#![no_main]

use core::arch::global_asm;

mod hal;
#[macro_use]
mod console;

// Include boot assembly
global_asm!(include_str!("hal/rv64/boot.S"));

/// Entry point called from boot.S
/// a0 = hartid, a1 = dtb_ptr
#[no_mangle]
pub extern "C" fn rust_main(hartid: usize, dtb_ptr: usize) -> ! {
    if hartid == 0 {
        hal::rv64::uart::init();
        kprintln!("hello world");
        kprintln!("[kernel] hart {} booting, dtb @ {:#x}", hartid, dtb_ptr);

        // Exercise IRQ enable/disable to verify compilation
        let was_enabled = hal::rv64::irq::is_enabled();
        kprintln!("[kernel] IRQ enabled before test: {}", was_enabled);

        hal::rv64::irq::enable();
        let enabled = hal::rv64::irq::is_enabled();
        kprintln!("[kernel] IRQ enabled after enable(): {}", enabled);

        let saved = hal::rv64::irq::disable_and_save();
        let disabled = hal::rv64::irq::is_enabled();
        kprintln!("[kernel] IRQ enabled after disable_and_save(): {}", disabled);

        hal::rv64::irq::restore(saved);
        let restored = hal::rv64::irq::is_enabled();
        kprintln!("[kernel] IRQ enabled after restore(): {}", restored);

        kprintln!("[kernel] boot complete, entering wfi loop");
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
