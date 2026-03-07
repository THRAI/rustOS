use core::{fmt::Display, ops::{Add, AddAssign, Sub}};

/// Page size constant
pub const PAGE_SIZE: usize = 4096;
pub const PAGE_SIZE_BITS: usize = 12;

macro_rules! implement_affine_space {
    ($type_name:ident) => {
        impl Add<usize> for $type_name {
            type Output = Self;
            fn add(self, rhs: usize) -> Self {
                Self(self.0 + rhs)
            }
        }
        impl AddAssign<usize> for $type_name {
            fn add_assign(&mut self, rhs: usize) {
                self.0 += rhs;
            }
        }
        impl Sub<$type_name> for $type_name {
            type Output = usize;
            fn sub(self, rhs: $type_name) -> usize {
                assert!(self.0 >= rhs.0, "Sub underflow in affine space");
                self.0 - rhs.0
            }
        }
        impl Sub<usize> for $type_name {
            type Output = Self;
            fn sub(self, rhs: usize) -> Self {
                Self(self.0 - rhs)
            }
        }
    };
}

/// Physical address newtype
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysAddr(pub usize);

/// Virtual address newtype
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VirtAddr(pub usize);

/// Physical page number
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysPageNum(pub usize);

/// Virtual page number
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VirtPageNum(pub usize);

impl VirtPageNum {
    pub const fn new(n: usize) -> Self {
        Self(n)
    }
    pub const fn as_usize(self) -> usize {
        self.0
    }
    pub const fn from_usize_unaligned(addr: usize) -> Self {
        Self(addr / PAGE_SIZE)
    }
}

impl Display for VirtPageNum {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<usize> for VirtPageNum {
    fn from(val: usize) -> Self {
        VirtPageNum(val)
    }
}

implement_affine_space!(PhysAddr);
implement_affine_space!(VirtAddr);
implement_affine_space!(PhysPageNum);
implement_affine_space!(VirtPageNum);

impl PhysAddr {
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }
    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub fn floor_page(&self) -> PhysPageNum {
        PhysPageNum(self.0 / PAGE_SIZE)
    }
    pub fn ceil_page(&self) -> PhysPageNum {
        if self.0 == 0 {
            return PhysPageNum(0);
        }
        PhysPageNum((self.0 - 1 + PAGE_SIZE) / PAGE_SIZE)
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
    pub fn is_aligned(&self) -> bool {
        self.page_offset() == 0
    }

    pub fn into_kernel_vaddr(self) -> VirtAddr {
        // Identity-mapped in kernel
        VirtAddr(self.0)
    }
}

impl Display for PhysAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PA 0x{:x}", self.0)
    }
}

impl From<PhysPageNum> for PhysAddr {
    fn from(v: PhysPageNum) -> Self {
        Self(v.0 * PAGE_SIZE)
    }
}

impl VirtAddr {
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }
    pub const fn new_page_aligned_down(addr: usize) -> Self {
        Self(addr & !(PAGE_SIZE - 1))
    }
    pub const fn new_page_aligned_up(addr: usize) -> Self {
        Self((addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1))
    }
    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub const fn page_offset(self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }
    pub const fn current_page_head(self) -> Self {
        Self(self.0 & !(PAGE_SIZE - 1))
    }

    pub const fn next_page_head(self) -> Self {
        Self((self.0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1))
    }

    pub const fn is_page_aligned(self) -> bool {
        self.page_offset() == 0
    }

    pub fn as_ptr(self) -> *const u8 {
        self.0 as *const u8
    }

    pub fn as_mut_ptr(self) -> *mut u8 {
        self.0 as *mut u8
    }

    pub fn as_page_slice<'a>(self) -> &'a [u8] {
        unsafe { core::slice::from_raw_parts(self.as_ptr(), PAGE_SIZE) }
    }
}


impl From<VirtPageNum> for VirtAddr {
    fn from(v: VirtPageNum) -> Self {
        Self(v.0 * PAGE_SIZE)
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
    pub fn new(base: PhysAddr, initial_offset: usize) -> Option<Self> {
        if initial_offset > PAGE_SIZE {
            return None;
        }
        Some(Self {
            base,
            offset: initial_offset,
        })
    }

    pub fn align_down(&mut self, align: usize) {
        debug_assert!(align.is_power_of_two());
        self.offset &= !(align - 1);
    }

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

    pub fn push_usize(&mut self, val: usize) -> Option<()> {
        let size = core::mem::size_of::<usize>();
        let slice = self.alloc_down_bytes(size)?;
        slice.copy_from_slice(&val.to_ne_bytes());
        Some(())
    }

    pub fn current_va(&self, vbase: VirtAddr) -> VirtAddr {
        VirtAddr::new(vbase.as_usize() + self.offset)
    }

    pub fn current_offset(&self) -> usize {
        self.offset
    }
}

#[cfg(all(test, not(target_os = "none")))]
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
        assert_eq!(a.current_page_head(), VirtAddr::new(0x2000));
        assert_eq!(a.next_page_head(), VirtAddr::new(0x3000));
        assert_eq!(a.page_offset(), 0xFFF);
    }

    #[test]
    fn page_aligned_addr_roundtrip() {
        let a = PhysAddr::new(0x4000);
        assert_eq!(a.page_align_down(), a);
        assert_eq!(a.page_align_up(), a);
    }
}
