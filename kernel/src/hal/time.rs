//! Time HAL facade.

#[inline]
pub fn init_this_cpu_timer() {
    #[cfg(target_arch = "riscv64")]
    super::rv64::timer::init();
    #[cfg(target_arch = "loongarch64")]
    super::la64::time::init();
}

#[inline]
pub fn handle_timer_irq() {
    #[cfg(target_arch = "riscv64")]
    super::rv64::timer::handle_timer_irq();
    #[cfg(target_arch = "loongarch64")]
    super::la64::time::handle_timer_irq();
}

#[inline]
pub fn monotonic_ms() -> u64 {
    #[cfg(target_arch = "riscv64")]
    {
        return super::rv64::timer::read_time_ms();
    }
    #[cfg(target_arch = "loongarch64")]
    {
        return super::la64::time::read_time_ms();
    }
}

#[inline]
pub fn monotonic_ns() -> u64 {
    #[cfg(target_arch = "riscv64")]
    {
        let ticks = super::rv64::timer::read_time() as u128;
        let hz = super::rv64::timer::TICK_HZ as u128;
        ((ticks * 1_000_000_000u128) / hz) as u64
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let ticks = super::la64::time::read_time() as u128;
        let hz = super::la64::time::TICK_HZ as u128;
        ((ticks * 1_000_000_000u128) / hz) as u64
    }
}
