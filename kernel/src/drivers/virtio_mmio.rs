//! VirtIO MMIO transport register interface.
//!
//! Provides volatile read/write access to VirtIO MMIO registers
//! per VirtIO spec 1.2 (MMIO transport).

/// MMIO register offsets (VirtIO spec §4.2.2).
pub const MAGIC_VALUE: usize = 0x000;
pub const VERSION: usize = 0x004;
pub const DEVICE_ID: usize = 0x008;
pub const VENDOR_ID: usize = 0x00C;
pub const DEVICE_FEATURES: usize = 0x010;
pub const DEVICE_FEATURES_SEL: usize = 0x014;
pub const DRIVER_FEATURES: usize = 0x020;
pub const DRIVER_FEATURES_SEL: usize = 0x024;
pub const GUEST_PAGE_SIZE: usize = 0x028;
pub const QUEUE_SEL: usize = 0x030;
pub const QUEUE_NUM_MAX: usize = 0x034;
pub const QUEUE_NUM: usize = 0x038;
/// MMIO v1 (legacy) registers
pub const QUEUE_ALIGN: usize = 0x03C;
pub const QUEUE_PFN: usize = 0x040;
/// MMIO v2 (modern) registers
pub const QUEUE_READY: usize = 0x044;
pub const QUEUE_NOTIFY: usize = 0x050;
pub const INTERRUPT_STATUS: usize = 0x060;
pub const INTERRUPT_ACK: usize = 0x064;
pub const STATUS: usize = 0x070;
pub const QUEUE_DESC_LOW: usize = 0x080;
pub const QUEUE_DESC_HIGH: usize = 0x084;
pub const QUEUE_DRIVER_LOW: usize = 0x090;
pub const QUEUE_DRIVER_HIGH: usize = 0x094;
pub const QUEUE_DEVICE_LOW: usize = 0x0A0;
pub const QUEUE_DEVICE_HIGH: usize = 0x0A4;

/// VirtIO MMIO magic value.
pub const VIRTIO_MAGIC: u32 = 0x74726976; // "virt"

/// Device status bits.
pub const STATUS_ACKNOWLEDGE: u32 = 1;
pub const STATUS_DRIVER: u32 = 2;
pub const STATUS_FEATURES_OK: u32 = 8;
pub const STATUS_DRIVER_OK: u32 = 4;
pub const STATUS_FAILED: u32 = 128;

/// VirtIO device type IDs.
pub const DEVICE_TYPE_BLOCK: u32 = 2;

/// VirtIO MMIO transport handle.
pub struct VirtioMmio {
    base: usize,
}

impl VirtioMmio {
    /// Create a new MMIO transport at the given base address.
    pub const fn new(base: usize) -> Self {
        Self { base }
    }

    /// Read a 32-bit register at the given offset.
    #[inline]
    pub fn read(&self, offset: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    /// Write a 32-bit register at the given offset.
    #[inline]
    pub fn write(&self, offset: usize, val: u32) {
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, val) }
    }

    /// Base address of this transport.
    pub fn base(&self) -> usize {
        self.base
    }
}
