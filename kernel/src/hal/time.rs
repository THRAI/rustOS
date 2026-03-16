//! Time HAL facade.

#[inline]
pub fn init_this_cpu_timer() {
    super::rv64::timer::init();
}

#[inline]
pub fn handle_timer_irq() {
    super::rv64::timer::handle_timer_irq();
}

#[inline]
pub fn monotonic_ms() -> u64 {
    super::rv64::timer::read_time_ms()
}

#[inline]
pub fn monotonic_ns() -> u64 {
    let ticks = super::rv64::timer::read_time() as u128;
    let hz = super::rv64::timer::TICK_HZ as u128;
    ((ticks * 1_000_000_000u128) / hz) as u64
}
