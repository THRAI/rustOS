//! SMP boot: start secondary harts via SBI HSM.
//!
//! Each secondary hart enters secondary_entry (asm), sets up its stack,
//! then calls secondary_rust_main which initializes PerCpu, trap vectors,
//! timer, and enters the executor loop.

use super::sbi;
use super::fdt;

/// Boot all secondary harts. Called by hart 0 after its own init is complete.
pub fn boot_secondary_harts(num_cpus: usize, hartids: &[usize], boot_hartid: usize) {
    for i in 0..num_cpus {
        let hartid = hartids[i];
        if hartid == boot_hartid {
            continue; // skip boot hart
        }
        let cpu_id = fdt::hart_to_cpu(hartid).unwrap();
        klog!(smp, info, "starting hart {} (cpu {})", hartid, cpu_id);

        let ret = sbi::hart_start(
            hartid,
            secondary_entry as usize,
            cpu_id, // passed as a0 (opaque) to the entry point
        );
        if ret.error != 0 {
            klog!(smp, error, "hart_start for hart {} failed: error={}", hartid, ret.error);
        }
    }
}

/// Assembly entry point for secondary harts.
/// SBI starts the hart here with a0 = opaque (logical cpu_id).
/// We set up the stack and jump to secondary_rust_main.
#[unsafe(naked)]
unsafe extern "C" fn secondary_entry() -> ! {
    core::arch::naked_asm!(
        // a0 = cpu_id (opaque from hart_start)
        // sp = boot_stack_top - cpu_id * 65536
        "mv     t0, a0",           // t0 = cpu_id
        "la     sp, boot_stack_top",
        "slli   t1, t0, 16",       // t1 = cpu_id * 65536
        "sub    sp, sp, t1",
        // sscratch = 0 (kernel mode indicator)
        "csrw   sscratch, zero",
        // Call secondary_rust_main(cpu_id)
        // a0 still has cpu_id
        "mv     a0, t0",
        "call   {secondary_rust_main}",
        // Should never return
        "1: wfi",
        "j 1b",
        secondary_rust_main = sym secondary_rust_main,
    )
}

/// Rust entry for secondary harts. Called from secondary_entry asm.
#[no_mangle]
extern "C" fn secondary_rust_main(cpu_id: usize) -> ! {
    // Look up physical hartid from cpu_id
    let hartid = fdt::cpu_to_hart(cpu_id);
    klog!(smp, info, "hart {} (cpu {}) starting", hartid, cpu_id);

    // Initialize PerCpu for this CPU
    crate::executor::init_per_cpu(cpu_id, hartid);
    unsafe { crate::executor::per_cpu::set_tp(cpu_id) };

    // Set up trap vectors
    super::trap::init();

    // Arm timer
    super::timer::init();

    // Enable S-mode software interrupt (SSIE = bit 1) for IPI
    unsafe {
        core::arch::asm!("csrs sie, {}", in(reg) 1usize << 1);
    }

    // Enable global interrupts
    super::irq::enable();

    klog!(smp, info, "hart {} (cpu {}) entering executor loop", hartid, cpu_id);

    // Enter the executor loop (never returns)
    crate::executor::executor_loop()
}
