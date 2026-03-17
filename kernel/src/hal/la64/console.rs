#[inline]
pub fn init() {
    super::uart::init();
}

#[inline]
pub fn reinit(base: usize) {
    super::uart::reinit(base);
}

#[inline]
pub fn putchar(c: u8) {
    super::uart::putchar(c);
}

#[inline]
pub fn getchar() -> Option<u8> {
    super::uart::getchar()
}
