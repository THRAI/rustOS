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

        // Initialize trap infrastructure (stvec + STIE + SSIE)
        trap::init();

        // Arm the first timer interrupt (10ms interval)
        hal::rv64::timer::init();

        // Parse FDT to discover CPUs
        let (num_cpus, hartids) = hal::rv64::fdt::parse_cpus(dtb_ptr);

        // Initialize per-CPU data for hart 0
        let cpu0 = hal::rv64::fdt::hart_to_cpu(hartid).unwrap_or(0);
        executor::init_per_cpu(cpu0, hartid);
        unsafe { executor::per_cpu::set_tp(cpu0) };
        kprintln!("[kernel] per-cpu data initialized for hart {} (cpu {})", hartid, cpu0);

        // Spawn a test kernel task to prove the executor path works
        executor::spawn_kernel_task(async {
            kprintln!("hello from async future!");
        }, 0).detach();

        // Integration test: sleep future (should wake after ~100ms = 10 ticks)
        executor::spawn_kernel_task(async {
            executor::sleep(100).await;
            kprintln!("woke after 100ms!");
        }, 0).detach();

        // Boot secondary harts
        if num_cpus > 1 {
            hal::rv64::smp::boot_secondary_harts(num_cpus, &hartids, hartid);
        }

        // Spawn cross-CPU test task (if we have >1 CPU)
        if num_cpus > 1 {
            executor::spawn_kernel_task(async {
                kprintln!("hello from CPU 1");
            }, 1).detach();
        }

        // Integration test: cross-CPU wake (CPU 0 sleeps, then spawns on CPU 1)
        if num_cpus > 1 {
            executor::spawn_kernel_task(async {
                executor::sleep(50).await;
                executor::spawn_kernel_task(async {
                    kprintln!("cross-cpu wake on CPU 1");
                }, 1).detach();
            }, 0).detach();
        }

        // Register clobber test: verify trap save/restore
        register_clobber_test();

        // Enable global interrupts
        hal::rv64::irq::enable();
        kprintln!("[kernel] interrupts enabled, entering executor loop");

        // Enter the executor loop (never returns)
        executor::executor_loop();
    }

    // Secondary harts: park until SBI hart_start wakes them
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

/// Register clobber test: write known values to caller-saved registers,
/// wait for a timer IRQ (which saves/restores via __kernel_trap), then
/// verify the registers are intact. Tests trap entry/exit correctness.
fn register_clobber_test() {
    let ok: usize;
    unsafe {
        core::arch::asm!(
            // Write known values to t0-t6 (caller-saved temporaries)
            "li t0, 0xDEAD0000",
            "li t1, 0xDEAD0001",
            "li t2, 0xDEAD0002",
            "li t3, 0xDEAD0003",
            "li t4, 0xDEAD0004",
            "li t5, 0xDEAD0005",
            "li t6, 0xDEAD0006",
            // Enable interrupts and wait for a timer IRQ
            "csrsi sstatus, 0x2",  // SIE = 1
            "wfi",                  // wait for timer IRQ
            "csrci sstatus, 0x2",  // SIE = 0
            // Now check all values survived the trap
            "li {tmp}, 0",         // assume pass (0 = ok)
            "li {exp}, 0xDEAD0000",
            "bne t0, {exp}, 1f",
            "li {exp}, 0xDEAD0001",
            "bne t1, {exp}, 1f",
            "li {exp}, 0xDEAD0002",
            "bne t2, {exp}, 1f",
            "li {exp}, 0xDEAD0003",
            "bne t3, {exp}, 1f",
            "li {exp}, 0xDEAD0004",
            "bne t4, {exp}, 1f",
            "li {exp}, 0xDEAD0005",
            "bne t5, {exp}, 1f",
            "li {exp}, 0xDEAD0006",
            "bne t6, {exp}, 1f",
            "j 2f",                // all passed
            "1:",                  // fail
            "li {tmp}, 1",
            "2:",
            tmp = out(reg) ok,
            exp = out(reg) _,
            out("t0") _,
            out("t1") _,
            out("t2") _,
            out("t3") _,
            out("t4") _,
            out("t5") _,
            out("t6") _,
        );
    }
    if ok == 0 {
        kprintln!("register clobber PASS");
    } else {
        kprintln!("register clobber FAIL");
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
