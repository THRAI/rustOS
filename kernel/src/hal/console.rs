//! Console HAL facade.

#[inline]
pub fn init() {
    #[cfg(target_arch = "riscv64")]
    super::rv64::uart::init();
    #[cfg(target_arch = "loongarch64")]
    super::la64::uart::init();
}

#[inline]
pub fn reinit(base: usize) {
    #[cfg(target_arch = "riscv64")]
    super::rv64::uart::reinit(base);
    #[cfg(target_arch = "loongarch64")]
    super::la64::uart::reinit(base);
}

#[inline]
pub fn putchar(c: u8) {
    #[cfg(target_arch = "riscv64")]
    super::rv64::uart::putchar(c);
    #[cfg(target_arch = "loongarch64")]
    super::la64::uart::putchar(c);
}

#[inline]
pub fn getchar() -> Option<u8> {
    #[cfg(target_arch = "riscv64")]
    {
        return super::rv64::uart::getchar();
    }
    #[cfg(target_arch = "loongarch64")]
    {
        return super::la64::uart::getchar();
    }
}
