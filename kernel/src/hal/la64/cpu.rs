#[inline(always)]
pub fn idle_once() {
    unsafe {
        core::arch::asm!("idle 0");
    }
}

#[inline(always)]
pub fn read_cpu_local_ptr() -> usize {
    let tp: usize;
    unsafe {
        core::arch::asm!("move {}, $tp", out(reg) tp);
    }
    tp
}

#[inline(always)]
pub unsafe fn write_cpu_local_ptr(ptr: usize) {
    unsafe {
        core::arch::asm!("move $tp, {}", in(reg) ptr);
    }
}
