use core::sync::atomic::{AtomicU64, Ordering};

pub const TICK_HZ: usize = 100;

const TIMER_PERIODIC: u64 = 1 << 62;
const TIMER_ENABLE: u64 = 1 << 61;
const TIMER_MASK: u64 = 0x0fff_ffff_ffff;
const TIMER_INTERVAL_TICKS: u64 = 1_000_000;

static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

#[inline]
fn write_timer_cfg(value: u64) {
    unsafe {
        core::arch::asm!("csrwr {}, 0x41", in(reg) value);
    }
}

#[inline]
fn clear_timer_irq() {
    unsafe {
        core::arch::asm!("csrwr {}, 0x44", in(reg) 1usize);
    }
}

pub fn init() {
    let cfg = TIMER_ENABLE | TIMER_PERIODIC | (TIMER_INTERVAL_TICKS & TIMER_MASK);
    write_timer_cfg(cfg);
}

pub fn handle_timer_irq() {
    clear_timer_irq();
    TICK_COUNT.fetch_add(1, Ordering::Relaxed);

    let pc = crate::executor::current();
    pc.timer_wheel.lock().advance();
    pc.needs_reschedule.store(true, Ordering::Release);
}

pub fn read_time_ms() -> u64 {
    read_time() as u64 / 1000
}

pub fn read_time() -> usize {
    let value: usize;
    unsafe {
        core::arch::asm!("rdtime.d {}, $zero", out(reg) value);
    }
    value
}
