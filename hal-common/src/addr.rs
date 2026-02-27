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

    /// Interprets this physical address as a pointer to a `PAGE_SIZE` byte slice.
    /// # Safety
    /// The caller must ensure that the physical address is mapped and valid
    /// for reading (e.g. through a direct-map physical mapping window).
    #[inline]
    pub unsafe fn as_slice<'a>(self) -> &'a [u8] {
        core::slice::from_raw_parts(self.0 as *const u8, PAGE_SIZE)
    }

    /// Interprets this physical address as a pointer to a mutable `PAGE_SIZE` byte slice.
    /// # Safety
    /// The caller must ensure that the physical address is mapped and valid
    /// for writing.
    #[inline]
    pub unsafe fn as_mut_slice<'a>(self) -> &'a mut [u8] {
        core::slice::from_raw_parts_mut(self.0 as *mut u8, PAGE_SIZE)
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

/// A strongly-typed cursor for safely laying out sequential data within
/// a single physical page (e.g., building user stacks during `execve`).
#[derive(Debug)]
pub struct PageCursor {
    base: PhysAddr,
    offset: usize,
}

impl PageCursor {
    /// Create a new cursor starting at humanity-defined offset within the page.
    pub fn new(base: PhysAddr, initial_offset: usize) -> Option<Self> {
        if initial_offset > PAGE_SIZE {
            return None;
        }
        Some(Self {
            base,
            offset: initial_offset,
        })
    }

    /// Align the current offset down to the specified alignment (must be power of two).
    pub fn align_down(&mut self, align: usize) {
        debug_assert!(align.is_power_of_two());
        self.offset &= !(align - 1);
    }

    /// Allocate `size` bytes, progressing downwards (like a stack).
    /// Returns the active slice region if space permits.
    pub fn alloc_down_bytes(&mut self, size: usize) -> Option<&mut [u8]> {
        if self.offset < size {
            return None;
        }
        self.offset -= size;
        unsafe {
            let ptr = (self.base.as_usize() + self.offset) as *mut u8;
            Some(core::slice::from_raw_parts_mut(ptr, size))
        }
    }

    /// Write a `usize` progressing downwards.
    pub fn push_usize(&mut self, val: usize) -> Option<()> {
        let size = core::mem::size_of::<usize>();
        let slice = self.alloc_down_bytes(size)?;
        slice.copy_from_slice(&val.to_ne_bytes());
        Some(())
    }

    /// Determine the current virtual equivalent address given a target virtual base.
    pub fn current_va(&self, vbase: VirtAddr) -> VirtAddr {
        VirtAddr::new(vbase.as_usize() + self.offset)
    }

    pub fn current_offset(&self) -> usize {
        self.offset
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
