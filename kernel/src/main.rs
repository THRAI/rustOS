#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;

mod alloc_early;
mod executor;
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

        // Arm the first timer interrupt (10ms interval)
        hal::rv64::timer::init();

        // Initialize per-CPU data for hart 0
        // Phase 1: hartid == cpu_id for the boot hart
        executor::init_per_cpu(0, hartid);
        unsafe { executor::per_cpu::set_tp(0) };
        kprintln!("[kernel] per-cpu data initialized for hart {}", hartid);

        // Spawn a test kernel task to prove the executor path works
        executor::spawn_kernel_task(async {
            kprintln!("hello from async future!");
        }, 0).detach();

        // Enable global interrupts
        hal::rv64::irq::enable();
        kprintln!("[kernel] interrupts enabled, entering executor loop");

        // Enter the executor loop (never returns)
        executor::executor_loop();
    }

    // Secondary harts: park until SMP boot (Plan 04)
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
