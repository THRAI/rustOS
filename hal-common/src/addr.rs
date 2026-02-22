/// Page size constant
pub const PAGE_SIZE: usize = 4096;

/// Physical address newtype
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysAddr(pub usize);

impl PhysAddr {
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub const fn add(self, offset: usize) -> Self {
        Self(self.0 + offset)
    }

    pub const fn page_offset(self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }

    pub const fn page_align_down(self) -> Self {
        Self(self.0 & !(PAGE_SIZE - 1))
    }

    pub const fn page_align_up(self) -> Self {
        Self((self.0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1))
    }

    pub const fn is_page_aligned(self) -> bool {
        self.page_offset() == 0
    }
}

/// Virtual address newtype
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VirtAddr(pub usize);

impl VirtAddr {
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub const fn add(self, offset: usize) -> Self {
        Self(self.0 + offset)
    }

    pub const fn page_offset(self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }

    pub const fn page_align_down(self) -> Self {
        Self(self.0 & !(PAGE_SIZE - 1))
    }

    pub const fn page_align_up(self) -> Self {
        Self((self.0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1))
    }

    pub const fn is_page_aligned(self) -> bool {
        self.page_offset() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phys_addr_align() {
        let a = PhysAddr::new(0x1234);
        assert_eq!(a.page_align_down(), PhysAddr::new(0x1000));
        assert_eq!(a.page_align_up(), PhysAddr::new(0x2000));
        assert_eq!(a.page_offset(), 0x234);
        assert!(!a.is_page_aligned());
        assert!(PhysAddr::new(0x3000).is_page_aligned());
    }

    #[test]
    fn virt_addr_align() {
        let a = VirtAddr::new(0x2FFF);
        assert_eq!(a.page_align_down(), VirtAddr::new(0x2000));
        assert_eq!(a.page_align_up(), VirtAddr::new(0x3000));
        assert_eq!(a.page_offset(), 0xFFF);
    }

    #[test]
    fn addr_add() {
        assert_eq!(PhysAddr::new(0x1000).add(0x500), PhysAddr::new(0x1500));
        assert_eq!(VirtAddr::new(0x1000).add(0x500), VirtAddr::new(0x1500));
    }

    #[test]
    fn addr_ordering() {
        assert!(PhysAddr::new(0x1000) < PhysAddr::new(0x2000));
        assert!(VirtAddr::new(0x3000) > VirtAddr::new(0x2000));
    }

    #[test]
    fn page_aligned_addr_roundtrip() {
        let a = PhysAddr::new(0x4000);
        assert_eq!(a.page_align_down(), a);
        assert_eq!(a.page_align_up(), a);
    }
}
