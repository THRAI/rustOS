const LOONGARCH_IOCSR_IPI_CLEAR: usize = 0x100c;
const LOONGARCH_IOCSR_IPI_SEND: usize = 0x1040;
const IOCSR_IPI_SEND_CPU_SHIFT: usize = 16;
const IOCSR_IPI_SEND_BLOCKING: u32 = 1 << 31;
const IPI_ACTION_SCHED: u32 = 1;

#[inline]
fn iocsr_write_u32(addr: usize, value: u32) {
    unsafe {
        core::arch::asm!("iocsrwr.w {},{}", in(reg) value, in(reg) addr, options(nostack));
    }
}

#[inline]
fn clear_ipi_action(action: u32) {
    iocsr_write_u32(LOONGARCH_IOCSR_IPI_CLEAR, action);
}

fn send_ipi_action(boot_id: usize, action: u32) {
    for bit in 0..32 {
        if (action & (1u32 << bit)) == 0 {
            continue;
        }
        let value = IOCSR_IPI_SEND_BLOCKING | ((boot_id as u32) << IOCSR_IPI_SEND_CPU_SHIFT) | bit;
        iocsr_write_u32(LOONGARCH_IOCSR_IPI_SEND, value);
    }
}

pub fn send_ipi(target_cpu: usize) {
    let current_cpu = crate::executor::current().cpu_id;
    if target_cpu == current_cpu {
        handle_ipi();
        return;
    }
    let boot_id = crate::hal::platform::cpu_to_boot_id(target_cpu);
    send_ipi_action(boot_id, IPI_ACTION_SCHED);
}

pub fn handle_ipi() {
    clear_ipi_action(IPI_ACTION_SCHED);
    let cpu_id = crate::executor::current().cpu_id;
    if crate::mm::has_pending(cpu_id) {
        crate::hal::handle_shootdown_ipi(cpu_id);
    }
}
