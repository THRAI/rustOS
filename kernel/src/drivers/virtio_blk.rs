//! VirtIO-blk MMIO driver for QEMU virt machine.
//!
//! Discovers the block device by probing known MMIO addresses,
//! negotiates features, sets up a single virtqueue, and provides
//! synchronous read_sector/write_sector with adaptive polling.

use crate::drivers::virtio_mmio::*;
use crate::kprintln;
use crate::mm::allocator::frame_alloc_sync;
use core::sync::atomic::{fence, Ordering};
use hal_common::PAGE_SIZE;

/// Sector size in bytes.
const SECTOR_SIZE: usize = 512;

/// Maximum virtqueue size we'll use.
const QUEUE_SIZE_MAX: u16 = 128;

/// Spin iterations before falling back to WFI in adaptive polling.
const SPIN_ITERS: usize = 1000;

/// QEMU virt machine VirtIO MMIO base addresses (8 devices, 0x10001000..0x10008000).
const VIRTIO_MMIO_BASES: [usize; 8] = [
    0x10008000, 0x10007000, 0x10006000, 0x10005000,
    0x10004000, 0x10003000, 0x10002000, 0x10001000,
];

/// VirtIO block request types.
const VIRTIO_BLK_T_IN: u32 = 0;  // read
const VIRTIO_BLK_T_OUT: u32 = 1; // write

/// Virtqueue descriptor flags.
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

/// Virtqueue descriptor (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VringDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Available ring header.
#[repr(C)]
struct VringAvail {
    flags: u16,
    idx: u16,
    // ring[queue_size] follows
}

/// Used ring header.
#[repr(C)]
struct VringUsed {
    flags: u16,
    idx: u16,
    // ring[queue_size] of VringUsedElem follows
}

/// Used ring element.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VringUsedElem {
    id: u32,
    len: u32,
}

/// VirtIO block request header (16 bytes).
#[repr(C)]
struct VirtioBlkReqHeader {
    type_: u32,
    reserved: u32,
    sector: u64,
}

/// VirtIO-blk driver instance.
pub struct VirtioBlk {
    mmio: VirtioMmio,
    queue_size: u16,
    /// Physical address of descriptor table.
    desc_pa: usize,
    /// Physical address of available ring.
    avail_pa: usize,
    /// Physical address of used ring.
    used_pa: usize,
    /// Next descriptor index to use.
    next_desc: u16,
    /// Last seen used ring index.
    last_used_idx: u16,
    /// Device capacity in sectors.
    capacity: u64,
    /// Physical address of the request header (reused).
    req_hdr_pa: usize,
    /// Physical address of the status byte (reused).
    status_pa: usize,
}

/// Global driver instance.
static VIRTIO_BLK: spin::Once<spin::Mutex<VirtioBlk>> = spin::Once::new();

impl VirtioBlk {
    /// Probe MMIO addresses and initialize the first block device found.
    fn probe_and_init() -> Option<Self> {
        for &base in &VIRTIO_MMIO_BASES {
            let mmio = VirtioMmio::new(base);
            let magic = mmio.read(MAGIC_VALUE);
            if magic != VIRTIO_MAGIC {
                continue;
            }
            let dev_id = mmio.read(DEVICE_ID);
            if dev_id != DEVICE_TYPE_BLOCK {
                continue;
            }
            kprintln!("[virtio-blk] found block device at {:#x}", base);
            return Self::init_device(mmio);
        }
        None
    }

    /// Initialize a VirtIO-blk device (spec §3.1).
    fn init_device(mmio: VirtioMmio) -> Option<Self> {
        // 1. Reset
        mmio.write(STATUS, 0);

        // 2. Acknowledge + Driver
        mmio.write(STATUS, STATUS_ACKNOWLEDGE);
        mmio.write(STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        // 3. Read features (word 0) — accept all for now
        mmio.write(DEVICE_FEATURES_SEL, 0);
        let features = mmio.read(DEVICE_FEATURES);
        mmio.write(DRIVER_FEATURES_SEL, 0);
        // Accept features but clear VIRTIO_F_EVENT_IDX (bit 29) and
        // VIRTIO_F_INDIRECT_DESC (bit 28) since we don't support them
        let accepted = features & !(1 << 28) & !(1 << 29);
        mmio.write(DRIVER_FEATURES, accepted);

        // 4. Features OK
        let status = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
        mmio.write(STATUS, status);
        let readback = mmio.read(STATUS);
        if readback & STATUS_FEATURES_OK == 0 {
            kprintln!("[virtio-blk] FEATURES_OK not set, aborting");
            mmio.write(STATUS, STATUS_FAILED);
            return None;
        }

        // 5. Configure virtqueue 0
        mmio.write(QUEUE_SEL, 0);
        let max_size = mmio.read(QUEUE_NUM_MAX) as u16;
        if max_size == 0 {
            kprintln!("[virtio-blk] queue 0 not available");
            return None;
        }
        let queue_size = max_size.min(QUEUE_SIZE_MAX);
        mmio.write(QUEUE_NUM, queue_size as u32);

        // Allocate descriptor table, available ring, used ring
        // Desc table: 16 bytes * queue_size
        // Avail ring: 4 + 2*queue_size + 2 (pad)
        // Used ring: 4 + 8*queue_size + 2 (pad)
        // We allocate pages for each.
        let desc_frame = frame_alloc_sync().expect("virtio-blk: desc alloc");
        let avail_frame = frame_alloc_sync().expect("virtio-blk: avail alloc");
        let used_frame = frame_alloc_sync().expect("virtio-blk: used alloc");

        let desc_pa = desc_frame.as_usize();
        let avail_pa = avail_frame.as_usize();
        let used_pa = used_frame.as_usize();

        // Zero the pages
        unsafe {
            core::ptr::write_bytes(desc_pa as *mut u8, 0, PAGE_SIZE);
            core::ptr::write_bytes(avail_pa as *mut u8, 0, PAGE_SIZE);
            core::ptr::write_bytes(used_pa as *mut u8, 0, PAGE_SIZE);
        }

        // Write queue addresses to device
        mmio.write(QUEUE_DESC_LOW, desc_pa as u32);
        mmio.write(QUEUE_DESC_HIGH, (desc_pa >> 32) as u32);
        mmio.write(QUEUE_DRIVER_LOW, avail_pa as u32);
        mmio.write(QUEUE_DRIVER_HIGH, (avail_pa >> 32) as u32);
        mmio.write(QUEUE_DEVICE_LOW, used_pa as u32);
        mmio.write(QUEUE_DEVICE_HIGH, (used_pa >> 32) as u32);

        mmio.write(QUEUE_READY, 1);

        // 6. Driver OK
        mmio.write(STATUS, status | STATUS_DRIVER_OK);

        // Read capacity from device config (offset 0x100 for MMIO v2)
        let cap_lo = mmio.read(0x100) as u64;
        let cap_hi = mmio.read(0x104) as u64;
        let capacity = cap_lo | (cap_hi << 32);

        // Allocate a persistent request header + status byte page
        let req_frame = frame_alloc_sync().expect("virtio-blk: req alloc");
        let req_hdr_pa = req_frame.as_usize();
        // Status byte at offset 16 (after the 16-byte header)
        let status_pa = req_hdr_pa + 16;

        Some(VirtioBlk {
            mmio,
            queue_size,
            desc_pa,
            avail_pa,
            used_pa,
            next_desc: 0,
            last_used_idx: 0,
            capacity,
            req_hdr_pa,
            status_pa,
        })
    }

    /// Read a 512-byte sector from the device.
    pub fn read_sector(&mut self, sector: u64, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), ()> {
        if sector >= self.capacity {
            return Err(());
        }
        self.do_request(VIRTIO_BLK_T_IN, sector, buf.as_mut_ptr(), SECTOR_SIZE)
    }

    /// Write a 512-byte sector to the device.
    pub fn write_sector(&mut self, sector: u64, buf: &[u8; SECTOR_SIZE]) -> Result<(), ()> {
        if sector >= self.capacity {
            return Err(());
        }
        self.do_request(VIRTIO_BLK_T_OUT, sector, buf.as_ptr() as *mut u8, SECTOR_SIZE)
    }

    /// Read multiple contiguous sectors into a buffer.
    pub fn read_sectors(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), ()> {
        let nsectors = buf.len() / SECTOR_SIZE;
        if sector + nsectors as u64 > self.capacity {
            return Err(());
        }
        self.do_request(VIRTIO_BLK_T_IN, sector, buf.as_mut_ptr(), buf.len())
    }

    /// Perform a single VirtIO-blk request (3-descriptor chain).
    fn do_request(
        &mut self,
        req_type: u32,
        sector: u64,
        data_ptr: *mut u8,
        data_len: usize,
    ) -> Result<(), ()> {
        let desc_base = self.desc_pa as *mut VringDesc;
        let d0 = self.next_desc;
        let d1 = (d0 + 1) % self.queue_size;
        let d2 = (d1 + 1) % self.queue_size;

        // Write request header
        let hdr = self.req_hdr_pa as *mut VirtioBlkReqHeader;
        unsafe {
            (*hdr).type_ = req_type;
            (*hdr).reserved = 0;
            (*hdr).sector = sector;
        }

        // Clear status byte
        unsafe { *(self.status_pa as *mut u8) = 0xFF; }

        // Descriptor 0: request header (device-readable)
        unsafe {
            let d = &mut *desc_base.add(d0 as usize);
            d.addr = self.req_hdr_pa as u64;
            d.len = 16;
            d.flags = VRING_DESC_F_NEXT;
            d.next = d1;
        }

        // Descriptor 1: data buffer
        let data_flags = if req_type == VIRTIO_BLK_T_IN {
            VRING_DESC_F_NEXT | VRING_DESC_F_WRITE // device writes to buf
        } else {
            VRING_DESC_F_NEXT // device reads from buf
        };
        unsafe {
            let d = &mut *desc_base.add(d1 as usize);
            d.addr = data_ptr as u64;
            d.len = data_len as u32;
            d.flags = data_flags;
            d.next = d2;
        }

        // Descriptor 2: status byte (device-writable)
        unsafe {
            let d = &mut *desc_base.add(d2 as usize);
            d.addr = self.status_pa as u64;
            d.len = 1;
            d.flags = VRING_DESC_F_WRITE;
            d.next = 0;
        }

        // Add to available ring
        fence(Ordering::SeqCst);
        let avail = self.avail_pa as *mut VringAvail;
        let avail_idx = unsafe { core::ptr::read_volatile(&(*avail).idx) };
        let ring_entry = self.avail_pa + 4 + (avail_idx % self.queue_size) as usize * 2;
        unsafe { core::ptr::write_volatile(ring_entry as *mut u16, d0); }
        fence(Ordering::SeqCst);
        unsafe { core::ptr::write_volatile(&mut (*avail).idx, avail_idx.wrapping_add(1)); }
        fence(Ordering::SeqCst);

        // Notify device
        self.mmio.write(QUEUE_NOTIFY, 0);

        // Adaptive polling: spin then WFI
        let used = self.used_pa as *mut VringUsed;
        let target_idx = self.last_used_idx.wrapping_add(1);

        for _ in 0..SPIN_ITERS {
            fence(Ordering::SeqCst);
            let cur = unsafe { core::ptr::read_volatile(&(*used).idx) };
            if cur != self.last_used_idx {
                break;
            }
            core::hint::spin_loop();
        }

        // WFI fallback
        loop {
            fence(Ordering::SeqCst);
            let cur = unsafe { core::ptr::read_volatile(&(*used).idx) };
            if cur != self.last_used_idx {
                break;
            }
            unsafe { core::arch::asm!("wfi"); }
        }

        self.last_used_idx = target_idx;
        self.next_desc = (d2 + 1) % self.queue_size;

        // Check status
        let status = unsafe { *(self.status_pa as *const u8) };
        if status == 0 {
            Ok(())
        } else {
            Err(())
        }
    }

    /// Device capacity in sectors.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }
}

/// Initialize the VirtIO-blk driver. Called from rust_main after frame allocator init.
pub fn init() {
    VIRTIO_BLK.call_once(|| {
        let blk = VirtioBlk::probe_and_init().expect("virtio-blk: no block device found");
        kprintln!("[virtio-blk] initialized, capacity = {} sectors", blk.capacity());
        spin::Mutex::new(blk)
    });
}

/// Get a reference to the global VirtIO-blk driver.
pub fn get() -> &'static spin::Mutex<VirtioBlk> {
    VIRTIO_BLK.get().expect("virtio-blk not initialized")
}
