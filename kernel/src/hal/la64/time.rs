use core::sync::atomic::{AtomicU64, Ordering};

pub const COUNTER_HZ: usize = 100_000_000;
pub const TICK_HZ: usize = COUNTER_HZ;
const TIMER_IRQ_HZ: usize = 100;

const TCFG_EN: u64 = 1 << 0;
const TCFG_PERIODIC: u64 = 1 << 1;
const TCFG_INITVAL_SHIFT: u64 = 2;
const TCFG_INITVAL_MASK: u64 = (1u64 << 48) - 1;
const TIMER_INTERVAL_TICKS: u64 = (COUNTER_HZ / TIMER_IRQ_HZ) as u64;

static TICK_COUNT: AtomicU64 = AtomicU64::new(0);
static TIMER_HANDLER_ENTER_COUNT: AtomicU64 = AtomicU64::new(0);

#[inline]
fn write_timer_cfg(value: u64) {
    unsafe {
        core::arch::asm!("csrwr {}, 0x41", in(reg) value);
    }
}

#[inline]
fn read_timer_cfg() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!("csrrd {}, 0x41", out(reg) value);
    }
    value
}

#[inline]
fn read_estat() -> usize {
    let value: usize;
    unsafe {
        core::arch::asm!("csrrd {}, 0x5", out(reg) value);
    }
    value
}

#[inline]
fn clear_timer_irq() {
    unsafe {
        core::arch::asm!("csrwr {}, 0x44", in(reg) 1usize);
    }
}

#[inline]
fn write_timer_init_value(delta_ticks: u64) {
    let init_val = if delta_ticks < 4 {
        4
    } else {
        (delta_ticks + 3) & !3
    };
    let init_field = ((init_val >> TCFG_INITVAL_SHIFT) & TCFG_INITVAL_MASK) << TCFG_INITVAL_SHIFT;
    write_timer_cfg(init_field);
    clear_timer_irq();

    let mut cfg = read_timer_cfg();
    cfg |= TCFG_EN;
    write_timer_cfg(cfg);

    cfg = read_timer_cfg();
    cfg |= TCFG_PERIODIC;
    write_timer_cfg(cfg);
}

#[inline]
pub fn set_next_trigger() {
    write_timer_init_value(TIMER_INTERVAL_TICKS);
}

pub fn init() {
    clear_timer_irq();
    set_next_trigger();
    clear_timer_irq();
    let (tcfg, estat) = debug_snapshot();
    crate::kprintln!("la64 timer init: tcfg={:#x} estat={:#x}", tcfg, estat);
}

pub fn handle_timer_irq() {
    let enter = TIMER_HANDLER_ENTER_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if enter == 1 {
        crate::kprintln!("la64 timer handler entered");
    }
    clear_timer_irq();
    let tick = TICK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if enter == 1 {
        crate::kprintln!("la64 timer interrupt online");
    } else if enter % 100 == 0 {
        crate::kprintln!("la64 timer enters={} ticks={}", enter, tick);
    }
    set_next_trigger();

    let pc = crate::executor::current();
    pc.timer_wheel.lock().advance();
    pc.needs_reschedule.store(true, Ordering::Release);
}

#[inline]
pub fn tick_count() -> u64 {
    TICK_COUNT.load(Ordering::Relaxed)
}

#[inline]
pub fn timer_handler_enter_count() -> u64 {
    TIMER_HANDLER_ENTER_COUNT.load(Ordering::Relaxed)
}

#[inline]
pub fn debug_snapshot() -> (u64, usize) {
    (read_timer_cfg(), read_estat())
}

pub fn read_time_ms() -> u64 {
    (read_time() as u64).saturating_mul(1_000) / (COUNTER_HZ as u64)
}

pub fn read_time() -> usize {
    let value: usize;
    unsafe {
        core::arch::asm!("rdtime.d {}, $zero", out(reg) value);
    }
    value
}
