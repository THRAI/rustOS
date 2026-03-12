use core::{
    fmt::Display,
    ops::{Add, AddAssign, Sub},
};

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
pub struct PhysAddr(pub(crate) usize);

/// Virtual address newtype
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VirtAddr(pub(crate) usize);

/// Page-granularity index. Context-dependent: may represent a PFN,
/// a virtual page number, or an object-internal page offset.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageNum(pub(crate) usize);

/// Backward-compatible alias for physical page numbers.
pub type PhysPageNum = PageNum;

/// Backward-compatible alias for virtual page numbers.
pub type VirtPageNum = PageNum;

impl PageNum {
    pub const fn new(n: usize) -> Self {
        Self(n)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    /// Convert a byte address/size to a page number (truncating / floor division).
    pub const fn from_bytes_floor(bytes: usize) -> Self {
        Self(bytes / PAGE_SIZE)
    }

    /// Convert a byte count to a page count (rounding up / ceil division).
    pub const fn from_bytes_ceil(bytes: usize) -> Self {
        Self((bytes + PAGE_SIZE - 1) / PAGE_SIZE)
    }

    /// Convert this page number back to a byte address/size.
    pub const fn to_bytes(self) -> usize {
        self.0 * PAGE_SIZE
    }

    /// Compute the in-page offset for a byte address.
    pub const fn page_offset_of(bytes: usize) -> usize {
        bytes & (PAGE_SIZE - 1)
    }
}

impl Display for PageNum {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<usize> for PageNum {
    fn from(val: usize) -> Self {
        PageNum(val)
    }
}

implement_affine_space!(PhysAddr);
implement_affine_space!(VirtAddr);
implement_affine_space!(PageNum);

impl PhysAddr {
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    /// Convert to a page number (floor / truncating).
    pub const fn page_num(self) -> PageNum {
        PageNum(self.0 / PAGE_SIZE)
    }

    pub fn floor_page(&self) -> PageNum {
        PageNum(self.0 / PAGE_SIZE)
    }
    pub fn ceil_page(&self) -> PageNum {
        if self.0 == 0 {
            return PageNum(0);
        }
        PageNum((self.0 - 1 + PAGE_SIZE) / PAGE_SIZE)
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

    pub fn into_kernel_vaddr(self) -> VirtAddr {
        // Identity-mapped in kernel
        VirtAddr(self.0)
    }

    pub fn as_slice<'a>(self) -> &'a [u8] {
        self.into_kernel_vaddr().as_page_slice()
    }
}

impl From<PhysAddr> for usize {
    fn from(value: PhysAddr) -> Self {
        value.0
    }
}

impl core::fmt::LowerHex for PhysAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PA 0x{:x}", self.0)
    }
}

impl Display for PhysAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PA 0x{:x}", self.0)
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

    /// Convert to a page number (floor / truncating).
    pub const fn page_num(self) -> PageNum {
        PageNum(self.0 / PAGE_SIZE)
    }

    pub const fn page_offset(self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }
    /// Round down to the nearest page boundary (same semantics as
    /// `PhysAddr::page_align_down`).
    pub const fn page_align_down(self) -> Self {
        Self(self.0 & !(PAGE_SIZE - 1))
    }

    /// Round up to the nearest page boundary (same semantics as
    /// `PhysAddr::page_align_up`).
    pub const fn page_align_up(self) -> Self {
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

// ---------------------------------------------------------------------------
// Address Range Types
// ---------------------------------------------------------------------------

/// A half-open virtual address range `[start, end)`.
///
/// Enforces `start <= end` at construction. Provides typed methods for
/// splitting, overlap detection, and iteration that replace raw `u64`
/// start/end pairs throughout the VM layer.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct VirtAddrRange {
    start: VirtAddr,
    end: VirtAddr,
}

impl VirtAddrRange {
    /// Create a new range `[start, end)`. Panics if `start > end`.
    pub fn new(start: VirtAddr, end: VirtAddr) -> Self {
        debug_assert!(
            start.as_usize() <= end.as_usize(),
            "VirtAddrRange: start ({:#x}) > end ({:#x})",
            start.as_usize(),
            end.as_usize()
        );
        Self { start, end }
    }

    /// Create a range from raw usize values.
    pub fn from_raw(start: usize, end: usize) -> Self {
        Self::new(VirtAddr::new(start), VirtAddr::new(end))
    }

    pub const fn start(self) -> VirtAddr {
        self.start
    }

    pub const fn end(self) -> VirtAddr {
        self.end
    }

    /// Byte length of the range.
    pub fn len(self) -> usize {
        self.end - self.start
    }

    /// Number of pages spanned (ceiling).
    pub fn page_count(self) -> usize {
        (self.len() + PAGE_SIZE - 1) / PAGE_SIZE
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Check if both start and end are page-aligned.
    pub fn is_page_aligned(self) -> bool {
        self.start.is_page_aligned() && self.end.is_page_aligned()
    }

    /// Check if `addr` falls within `[start, end)`.
    pub fn contains(self, addr: VirtAddr) -> bool {
        addr >= self.start && addr < self.end
    }

    /// Check if two ranges have any overlap.
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// Split `[start, end)` at `mid` into `[start, mid)` and `[mid, end)`.
    ///
    /// Panics if `mid` is outside `[start, end]`.
    pub fn split_at(self, mid: VirtAddr) -> (Self, Self) {
        debug_assert!(
            mid >= self.start && mid <= self.end,
            "split_at: mid ({:#x}) outside range [{:#x}, {:#x})",
            mid.as_usize(),
            self.start.as_usize(),
            self.end.as_usize()
        );
        (Self::new(self.start, mid), Self::new(mid, self.end))
    }

    /// Iterate over page-aligned addresses within this range.
    pub fn iter_pages(self) -> VirtAddrRangePageIter {
        VirtAddrRangePageIter {
            current: self.start.page_align_down(),
            end: self.end,
        }
    }
}

impl Display for VirtAddrRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "[{:#x}, {:#x})",
            self.start.as_usize(),
            self.end.as_usize()
        )
    }
}

/// Iterator over page-aligned virtual addresses within a range.
pub struct VirtAddrRangePageIter {
    current: VirtAddr,
    end: VirtAddr,
}

impl Iterator for VirtAddrRangePageIter {
    type Item = VirtAddr;
    fn next(&mut self) -> Option<VirtAddr> {
        if self.current >= self.end {
            return None;
        }
        let va = self.current;
        self.current = self.current + PAGE_SIZE;
        Some(va)
    }
}

/// A half-open physical address range `[start, end)`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct PhysAddrRange {
    start: PhysAddr,
    end: PhysAddr,
}

impl PhysAddrRange {
    /// Create a new range `[start, end)`. Panics if `start > end`.
    pub fn new(start: PhysAddr, end: PhysAddr) -> Self {
        debug_assert!(
            start.as_usize() <= end.as_usize(),
            "PhysAddrRange: start ({:#x}) > end ({:#x})",
            start.as_usize(),
            end.as_usize()
        );
        Self { start, end }
    }

    /// Create a range from raw usize values.
    pub fn from_raw(start: usize, end: usize) -> Self {
        Self::new(PhysAddr::new(start), PhysAddr::new(end))
    }

    pub const fn start(self) -> PhysAddr {
        self.start
    }

    pub const fn end(self) -> PhysAddr {
        self.end
    }

    /// Byte length of the range.
    pub fn len(self) -> usize {
        self.end - self.start
    }

    /// Number of pages spanned (ceiling).
    pub fn page_count(self) -> usize {
        (self.len() + PAGE_SIZE - 1) / PAGE_SIZE
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Check if both start and end are page-aligned.
    pub fn is_page_aligned(self) -> bool {
        self.start.is_page_aligned() && self.end.is_page_aligned()
    }

    /// Check if `addr` falls within `[start, end)`.
    pub fn contains(self, addr: PhysAddr) -> bool {
        addr >= self.start && addr < self.end
    }

    /// Check if two ranges have any overlap.
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

impl Display for PhysAddrRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "PA[{:#x}, {:#x})",
            self.start.as_usize(),
            self.end.as_usize()
        )
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
            let ptr = (self.base.0 + self.offset) as *mut u8;
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
        assert_eq!(a.page_align_down(), VirtAddr::new(0x2000));
        assert_eq!(a.page_align_up(), VirtAddr::new(0x3000));
        assert_eq!(a.page_offset(), 0xFFF);
    }

    #[test]
    fn page_aligned_addr_roundtrip() {
        let a = PhysAddr::new(0x4000);
        assert_eq!(a.page_align_down(), a);
        assert_eq!(a.page_align_up(), a);
    }

    #[test]
    fn page_num_conversions() {
        assert_eq!(PageNum::from_bytes_floor(4095), PageNum(0));
        assert_eq!(PageNum::from_bytes_floor(4096), PageNum(1));
        assert_eq!(PageNum::from_bytes_floor(8192), PageNum(2));
        assert_eq!(PageNum::from_bytes_ceil(1), PageNum(1));
        assert_eq!(PageNum::from_bytes_ceil(4096), PageNum(1));
        assert_eq!(PageNum::from_bytes_ceil(4097), PageNum(2));
        assert_eq!(PageNum::new(3).to_bytes(), 3 * PAGE_SIZE);
        assert_eq!(PageNum::page_offset_of(0x1234), 0x234);
        assert_eq!(PhysAddr::new(0x3000).page_num(), PageNum(3));
        assert_eq!(VirtAddr::new(0x5FFF).page_num(), PageNum(5));
    }
}
