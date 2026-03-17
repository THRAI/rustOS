#[inline(always)]
pub fn idle_once() {
    core::hint::spin_loop();
}

#[inline(always)]
pub fn read_cpu_local_ptr() -> usize {
    0
}

#[inline(always)]
pub unsafe fn write_cpu_local_ptr(_ptr: usize) {}
