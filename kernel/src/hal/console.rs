//! Console HAL facade.

#[inline]
pub fn init() {
    super::rv64::uart::init();
}

#[inline]
pub fn reinit(base: usize) {
    super::rv64::uart::reinit(base);
}

#[inline]
pub fn putchar(c: u8) {
    super::rv64::uart::putchar(c);
}

#[inline]
pub fn getchar() -> Option<u8> {
    super::rv64::uart::getchar()
}
