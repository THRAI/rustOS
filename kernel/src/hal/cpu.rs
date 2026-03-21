//! CPU-local HAL facade.

#[cfg(target_arch = "riscv64")]
#[inline(always)]
pub fn disable_local_irq_save() -> usize {
    super::rv64::irq::disable_and_save()
}

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub fn disable_local_irq_save() -> usize {
    super::la64::irq::disable_and_save()
}

#[cfg(target_arch = "riscv64")]
#[inline(always)]
pub fn restore_local_irq(saved: usize) {
    super::rv64::irq::restore(saved);
}

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub fn restore_local_irq(saved: usize) {
    super::la64::irq::restore(saved);
}

#[cfg(target_arch = "riscv64")]
#[inline(always)]
pub fn local_irq_enable() {
    super::rv64::irq::enable();
}

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub fn local_irq_enable() {
    super::la64::irq::enable();
}

#[cfg(target_arch = "riscv64")]
#[inline(always)]
pub fn local_irq_disable() {
    super::rv64::irq::disable();
}

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub fn local_irq_disable() {
    super::la64::irq::disable();
}

#[cfg(target_arch = "riscv64")]
#[inline(always)]
pub fn local_irq_is_enabled() -> bool {
    super::rv64::irq::is_enabled()
}

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub fn local_irq_is_enabled() -> bool {
    super::la64::irq::is_enabled()
}

#[cfg(target_arch = "riscv64")]
#[inline(always)]
pub fn idle_once() {
    unsafe {
        core::arch::asm!("wfi");
    }
}

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub fn idle_once() {
    super::la64::cpu::idle_once();
}

#[cfg(target_arch = "riscv64")]
#[inline(always)]
pub fn read_cpu_local_ptr() -> usize {
    let tp: usize;
    unsafe {
        core::arch::asm!("mv {}, tp", out(reg) tp);
    }
    tp
}

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub fn read_cpu_local_ptr() -> usize {
    super::la64::cpu::read_cpu_local_ptr()
}

#[cfg(target_arch = "riscv64")]
#[inline(always)]
pub unsafe fn write_cpu_local_ptr(ptr: usize) {
    unsafe {
        core::arch::asm!("mv tp, {}", in(reg) ptr);
    }
}

#[cfg(target_arch = "loongarch64")]
#[inline(always)]
pub unsafe fn write_cpu_local_ptr(ptr: usize) {
    unsafe {
        super::la64::cpu::write_cpu_local_ptr(ptr);
    }
}

#[cfg(all(feature = "qemu-test", target_arch = "riscv64"))]
pub fn register_clobber_self_test() -> bool {
    let failed: usize;
    unsafe {
        core::arch::asm!(
            "li t0, 0xDEAD0000",
            "li t1, 0xDEAD0001",
            "li t2, 0xDEAD0002",
            "li t3, 0xDEAD0003",
            "li t4, 0xDEAD0004",
            "li t5, 0xDEAD0005",
            "li t6, 0xDEAD0006",
            "csrsi sstatus, 0x2",
            "wfi",
            "csrci sstatus, 0x2",
            "li {tmp}, 0",
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
            "j 2f",
            "1:",
            "li {tmp}, 1",
            "2:",
            tmp = out(reg) failed,
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
    failed == 0
}

#[cfg(not(all(feature = "qemu-test", target_arch = "riscv64")))]
pub fn register_clobber_self_test() -> bool {
    true
}
