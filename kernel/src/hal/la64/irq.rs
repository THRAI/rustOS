#[inline(always)]
pub fn disable_and_save() -> usize {
    0
}

#[inline(always)]
pub fn restore(_saved: usize) {}

#[inline(always)]
pub fn enable() {}

#[inline(always)]
pub fn disable() {}

#[inline(always)]
pub fn is_enabled() -> bool {
    false
}
