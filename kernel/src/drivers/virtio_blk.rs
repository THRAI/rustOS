//! VirtIO-blk MMIO driver for QEMU virt machine.
//!
//! Discovers the block device by probing known MMIO addresses,
//! negotiates features, sets up a single virtqueue, and provides
//! synchronous read_sector/write_sector with adaptive polling.

use core::sync::atomic::{fence, Ordering};

use crate::{
    drivers::*,
    hal_common::{PhysAddr, PAGE_SIZE},
    mm::{alloc_raw_frame_sync, PageRole},
};

/// Align `val` up to `align` (must be power of 2).
const fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

/// Sector size in bytes.
const SECTOR_SIZE: usize = 512;

/// Maximum virtqueue size we'll use.
const QUEUE_SIZE_MAX: u16 = 128;

/// Spin iterations before falling back to WFI in adaptive polling.
const SPIN_ITERS: usize = 1000;

// VirtIO MMIO base addresses are now discovered from FDT via platform().virtio_mmio.

/// VirtIO block request types.
const VIRTIO_BLK_T_IN: u32 = 0; // read
const VIRTIO_BLK_T_OUT: u32 = 1; // write

/// Virtqueue descriptor flags.
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

const PCI_COMMON_DEVICE_FEATURE_SELECT: usize = 0x00;
const PCI_COMMON_DEVICE_FEATURE: usize = 0x04;
const PCI_COMMON_DRIVER_FEATURE_SELECT: usize = 0x08;
const PCI_COMMON_DRIVER_FEATURE: usize = 0x0c;
const PCI_COMMON_DEVICE_STATUS: usize = 0x14;
const PCI_COMMON_QUEUE_SELECT: usize = 0x16;
const PCI_COMMON_QUEUE_SIZE: usize = 0x18;
const PCI_COMMON_QUEUE_ENABLE: usize = 0x1c;
const PCI_COMMON_QUEUE_NOTIFY_OFF: usize = 0x1e;
const PCI_COMMON_QUEUE_DESC: usize = 0x20;
const PCI_COMMON_QUEUE_DRIVER: usize = 0x28;
const PCI_COMMON_QUEUE_DEVICE: usize = 0x30;

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
    transport: VirtioTransport,
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
    /// Physical address of a one-page DMA bounce buffer.
    data_bounce_pa: usize,
}

enum VirtioTransport {
    Mmio(VirtioMmio),
    Pci(VirtioPciTransport),
}

struct VirtioPciTransport {
    common_cfg_addr: usize,
    notify_addr: usize,
    device_cfg_addr: usize,
}

/// Global driver instance.
// SAFETY: SpinMutex (no IRQ disable). Currently safe because the VirtIO
// driver uses polled I/O only — there is no completion IRQ handler that
// could re-enter this lock. Must be upgraded to IrqSafeSpinLock if
// interrupt-driven I/O is added.
static VIRTIO_BLK: crate::hal_common::Once<crate::hal_common::SpinMutex<VirtioBlk, 0>> =
    crate::hal_common::Once::new();

impl VirtioBlk {
    /// Probe FDT-discovered VirtIO MMIO addresses and initialize the first block device found.
    fn probe_and_init() -> Option<Self> {
        let pi = crate::hal::platform();
        for dev in pi.virtio_mmio.iter().take(pi.virtio_count) {
            let base = match dev {
                Some(d) => d.base,
                None => continue,
            };
            let mmio = VirtioMmio::new(base);
            let magic = mmio.read(MAGIC_VALUE);
            if magic != VIRTIO_MAGIC {
                continue;
            }
            let dev_id = mmio.read(DEVICE_ID);
            if dev_id != DEVICE_TYPE_BLOCK {
                continue;
            }
            klog!(driver, info, "found block device at {:#x}", base);
            return Self::init_mmio_device(mmio);
        }

        if let Some(pci) = crate::drivers::probe_virtio_blk_pci() {
            let dev = pci.transport.device;
            klog!(
                driver,
                info,
                "found pci block device at {:02x}:{:02x}.{} common={:#x} notify={:#x}",
                dev.bus,
                dev.device,
                dev.function,
                pci.common_cfg_addr,
                pci.notify_cfg_addr
            );
            return Self::init_pci_device(pci);
        }
        None
    }

    /// Initialize a VirtIO-blk device (spec §3.1).
    fn init_mmio_device(mmio: VirtioMmio) -> Option<Self> {
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
            klog!(driver, error, "FEATURES_OK not set, aborting");
            mmio.write(STATUS, STATUS_FAILED);
            return None;
        }

        // MMIO v1: set guest page size (required before queue setup)
        mmio.write(GUEST_PAGE_SIZE, PAGE_SIZE as u32);

        // 5. Configure virtqueue 0 (MMIO v1 legacy layout)
        mmio.write(QUEUE_SEL, 0);
        let max_size = mmio.read(QUEUE_NUM_MAX) as u16;
        if max_size == 0 {
            klog!(driver, error, "queue 0 not available");
            return None;
        }
        let queue_size = max_size.min(QUEUE_SIZE_MAX);
        mmio.write(QUEUE_NUM, queue_size as u32);

        let qs = queue_size as usize;
        let desc_size = 16 * qs;
        let avail_size = 6 + 2 * qs;
        let queue_align = PAGE_SIZE;
        let used_offset = align_up(desc_size + avail_size, queue_align);
        let used_size = 6 + 8 * qs;
        let total_size = used_offset + used_size;

        let num_pages = align_up(total_size, PAGE_SIZE) / PAGE_SIZE;
        let order = num_pages.next_power_of_two().trailing_zeros() as usize;
        let base_frame = crate::mm::frame_alloc_contiguous(order).expect("virtio-blk: queue alloc");
        let base_pa = base_frame.as_usize();

        // Zero the entire region
        unsafe {
            core::ptr::write_bytes(
                PhysAddr::new(base_pa).into_kernel_vaddr().as_mut_ptr(),
                0,
                (1 << order) * PAGE_SIZE,
            );
        }

        let desc_pa = base_pa;
        let avail_pa = base_pa + desc_size;
        let used_pa = base_pa + used_offset;

        // Tell device: QUEUE_ALIGN and QUEUE_PFN
        mmio.write(QUEUE_ALIGN, queue_align as u32);
        mmio.write(QUEUE_PFN, (base_pa / PAGE_SIZE) as u32);

        // 6. Driver OK
        mmio.write(STATUS, status | STATUS_DRIVER_OK);

        // Read capacity from device config (offset 0x100 for MMIO v1)
        let cap_lo = mmio.read(0x100) as u64;
        let cap_hi = mmio.read(0x104) as u64;
        let capacity = cap_lo | (cap_hi << 32);

        // Allocate a persistent request header + status byte page
        let req_frame = alloc_raw_frame_sync(PageRole::DriverDma).expect("virtio-blk: req alloc");
        let req_hdr_pa = req_frame.as_usize();
        // Status byte at offset 16 (after the 16-byte header)
        let status_pa = req_hdr_pa + 16;
        unsafe {
            core::ptr::write_bytes(
                PhysAddr::new(req_hdr_pa).into_kernel_vaddr().as_mut_ptr(),
                0,
                PAGE_SIZE,
            );
        }

        let data_bounce_pa = alloc_raw_frame_sync(PageRole::DriverDma)
            .expect("virtio-blk: data bounce alloc")
            .as_usize();
        unsafe {
            core::ptr::write_bytes(
                PhysAddr::new(data_bounce_pa).into_kernel_vaddr().as_mut_ptr(),
                0,
                PAGE_SIZE,
            );
        }

        Some(VirtioBlk {
            transport: VirtioTransport::Mmio(mmio),
            queue_size,
            desc_pa,
            avail_pa,
            used_pa,
            next_desc: 0,
            last_used_idx: 0,
            capacity,
            req_hdr_pa,
            status_pa,
            data_bounce_pa,
        })
    }

    fn init_pci_device(probe: crate::drivers::VirtioBlkPciProbeInfo) -> Option<Self> {
        let transport = VirtioPciTransport {
            common_cfg_addr: probe.common_cfg_addr,
            notify_addr: probe.queue0_notify_addr,
            device_cfg_addr: probe.device_cfg_addr?,
        };

        transport.write_status(0);
        transport.write_status(STATUS_ACKNOWLEDGE);
        transport.write_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        let features = transport.read_device_features();
        let accepted = features & !(1 << 28) & !(1 << 29);
        transport.write_driver_features(accepted);

        let status = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
        transport.write_status(status);
        if transport.read_status() & STATUS_FEATURES_OK == 0 {
            klog!(driver, error, "virtio-pci FEATURES_OK not set, aborting");
            transport.write_status(STATUS_FAILED);
            return None;
        }

        let max_size = transport.queue_max_size(0);
        if max_size == 0 {
            klog!(driver, error, "virtio-pci queue 0 not available");
            return None;
        }
        let queue_size = max_size.min(QUEUE_SIZE_MAX);

        let qs = queue_size as usize;
        let desc_size = 16 * qs;
        let avail_size = 6 + 2 * qs;
        let used_offset = align_up(desc_size + avail_size, PAGE_SIZE);
        let used_size = 6 + 8 * qs;
        let total_size = used_offset + used_size;

        let num_pages = align_up(total_size, PAGE_SIZE) / PAGE_SIZE;
        let order = num_pages.next_power_of_two().trailing_zeros() as usize;
        let base_frame = crate::mm::frame_alloc_contiguous(order).expect("virtio-pci-blk: queue alloc");
        let base_pa = base_frame.as_usize();
        unsafe {
            core::ptr::write_bytes(
                PhysAddr::new(base_pa).into_kernel_vaddr().as_mut_ptr(),
                0,
                (1 << order) * PAGE_SIZE,
            );
        }

        let desc_pa = base_pa;
        let avail_pa = base_pa + desc_size;
        let used_pa = base_pa + used_offset;

        transport.queue_set(0, queue_size, desc_pa, avail_pa, used_pa);

        transport.write_status(status | STATUS_DRIVER_OK);

        let capacity = transport.read_blk_capacity();

        let req_frame = alloc_raw_frame_sync(PageRole::DriverDma).expect("virtio-pci-blk: req alloc");
        let req_hdr_pa = req_frame.as_usize();
        let status_pa = req_hdr_pa + 16;
        let data_bounce_pa = alloc_raw_frame_sync(PageRole::DriverDma)
            .expect("virtio-pci-blk: data bounce alloc")
            .as_usize();
        unsafe {
            core::ptr::write_bytes(
                PhysAddr::new(req_hdr_pa).into_kernel_vaddr().as_mut_ptr(),
                0,
                PAGE_SIZE,
            );
            core::ptr::write_bytes(
                PhysAddr::new(data_bounce_pa).into_kernel_vaddr().as_mut_ptr(),
                0,
                PAGE_SIZE,
            );
        }

        klog!(
            driver,
            info,
            "virtio-pci queue0 ready: common={:#x} notify={:#x} devcfg={:#x} desc_pa={:#x} avail_pa={:#x} used_pa={:#x} req_pa={:#x} status_pa={:#x} bounce_pa={:#x}",
            probe.common_cfg_addr,
            probe.queue0_notify_addr,
            transport.device_cfg_addr,
            desc_pa,
            avail_pa,
            used_pa,
            req_hdr_pa,
            status_pa,
            data_bounce_pa
        );

        Some(VirtioBlk {
            transport: VirtioTransport::Pci(transport),
            queue_size,
            desc_pa,
            avail_pa,
            used_pa,
            next_desc: 0,
            last_used_idx: 0,
            capacity,
            req_hdr_pa,
            status_pa,
            data_bounce_pa,
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
        self.do_request(
            VIRTIO_BLK_T_OUT,
            sector,
            buf.as_ptr() as *mut u8,
            SECTOR_SIZE,
        )
    }

    /// Read multiple contiguous sectors into a buffer.
    pub fn read_sectors(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), ()> {
        if !buf.len().is_multiple_of(SECTOR_SIZE) {
            return Err(());
        }
        let nsectors = buf.len() / SECTOR_SIZE;
        if sector + nsectors as u64 > self.capacity {
            return Err(());
        }
        for (idx, chunk) in buf.chunks_exact_mut(SECTOR_SIZE).enumerate() {
            let sector_buf: &mut [u8; SECTOR_SIZE] = chunk.try_into().map_err(|_| ())?;
            self.read_sector(sector + idx as u64, sector_buf)?;
        }
        Ok(())
    }

    /// Perform a single VirtIO-blk request (3-descriptor chain).
    fn do_request(
        &mut self,
        req_type: u32,
        sector: u64,
        data_ptr: *mut u8,
        data_len: usize,
    ) -> Result<(), ()> {
        if data_len > PAGE_SIZE {
            return Err(());
        }
        let desc_base = PhysAddr::new(self.desc_pa).into_kernel_vaddr().as_mut_ptr() as *mut VringDesc;
        let d0 = self.next_desc;
        let d1 = (d0 + 1) % self.queue_size;
        let d2 = (d1 + 1) % self.queue_size;
        let bounce_ptr = PhysAddr::new(self.data_bounce_pa).into_kernel_vaddr().as_mut_ptr();

        // Write request header
        let hdr = PhysAddr::new(self.req_hdr_pa).into_kernel_vaddr().as_mut_ptr()
            as *mut VirtioBlkReqHeader;
        unsafe {
            (*hdr).type_ = req_type;
            (*hdr).reserved = 0;
            (*hdr).sector = sector;
        }

        // Clear status byte
        let status_ptr = PhysAddr::new(self.status_pa).into_kernel_vaddr().as_mut_ptr();
        unsafe {
            core::ptr::write_volatile(status_ptr, 0xFF);
        }

        if req_type == VIRTIO_BLK_T_OUT {
            unsafe {
                core::ptr::copy_nonoverlapping(data_ptr as *const u8, bounce_ptr, data_len);
            }
        }

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
            d.addr = self.data_bounce_pa as u64;
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
        let avail = PhysAddr::new(self.avail_pa).into_kernel_vaddr().as_mut_ptr() as *mut VringAvail;
        let avail_idx = unsafe { core::ptr::read_volatile(&(*avail).idx) };
        let ring_entry =
            PhysAddr::new(self.avail_pa + 4 + (avail_idx % self.queue_size) as usize * 2)
                .into_kernel_vaddr()
                .as_mut_ptr() as *mut u16;
        unsafe {
            core::ptr::write_volatile(ring_entry, d0);
        }
        fence(Ordering::SeqCst);
        unsafe {
            core::ptr::write_volatile(&mut (*avail).idx, avail_idx.wrapping_add(1));
        }
        fence(Ordering::SeqCst);

        // Notify device
        self.transport.notify_queue(0);

        // Poll for completion. Enable SIE briefly so timer IRQs cause vCPU exits,
        // allowing QEMU to process the MMIO notification on the device model thread.
        let used = PhysAddr::new(self.used_pa).into_kernel_vaddr().as_mut_ptr() as *mut VringUsed;
        let used_slot = (self.last_used_idx % self.queue_size) as usize;
        let used_elem = PhysAddr::new(self.used_pa + 4 + used_slot * core::mem::size_of::<VringUsedElem>())
            .into_kernel_vaddr()
            .as_mut_ptr() as *mut VringUsedElem;
        unsafe {
            core::ptr::write_volatile(
                used_elem,
                VringUsedElem {
                    id: u32::MAX,
                    len: 0,
                },
            );
        }
        fence(Ordering::SeqCst);

        crate::klog!(
            sched,
            debug,
            "do_request: sector {} waiting for used.idx. cur={}, expected={}",
            sector,
            unsafe { (*used).idx },
            self.last_used_idx
        );
        let mut spins = 0;

        loop {
            fence(Ordering::SeqCst);
            let cur = unsafe { core::ptr::read_volatile(&(*used).idx) };
            if cur != self.last_used_idx {
                let elem = unsafe { core::ptr::read_volatile(used_elem as *const VringUsedElem) };
                let status = unsafe { core::ptr::read_volatile(status_ptr as *const u8) };
                if elem.id == d0 as u32 && status != 0xFF {
                    crate::klog!(
                        sched,
                        debug,
                        "do_request: sector {} DONE after {} spins elem.id={} len={} status={:#x}",
                        sector,
                        spins,
                        elem.id,
                        elem.len,
                        status
                    );
                    break;
                }
                crate::klog!(
                    sched,
                    debug,
                    "do_request: sector {} completion not ready yet: cur={} expected={} elem.id={} len={} status={:#x}",
                    sector,
                    cur,
                    self.last_used_idx,
                    elem.id,
                    elem.len,
                    status
                );
            }
            if spins % 1000000 == 0 && spins > 0 {
                crate::klog!(
                    sched,
                    debug,
                    "do_request: STILL SPINNING sector {} spins={}",
                    sector,
                    spins
                );
            }
            spins += 1;
            // Brief SIE window: lets pending IRQs fire, which causes a vCPU exit
            // and gives QEMU's device model a chance to process the virtqueue.
            crate::hal::local_irq_enable();
            core::hint::spin_loop();
            crate::hal::local_irq_disable();
            core::hint::spin_loop();
        }

        self.last_used_idx = self.last_used_idx.wrapping_add(1);
        self.next_desc = (d2 + 1) % self.queue_size;

        // Seeing used.idx advance means the device completed the descriptor
        // chain, but we still need an acquire barrier before reading the
        // status byte and bounce buffer contents from guest memory.
        fence(Ordering::SeqCst);

        // Check status
        let status = unsafe { core::ptr::read_volatile(status_ptr as *const u8) };
        if status == 0 {
            if req_type == VIRTIO_BLK_T_IN {
                unsafe {
                    core::ptr::copy_nonoverlapping(bounce_ptr, data_ptr, data_len);
                }
            }
            Ok(())
        } else {
            let used_idx = unsafe { core::ptr::read_volatile(&(*used).idx) };
            klog!(
                driver,
                error,
                "virtio-blk request failed: type={} sector={} status={:#x} used_idx={} last_used_idx={} desc_pa={:#x} avail_pa={:#x} used_pa={:#x}",
                req_type,
                sector,
                status,
                used_idx,
                self.last_used_idx,
                self.desc_pa,
                self.avail_pa,
                self.used_pa
            );
            Err(())
        }
    }

    /// Device capacity in sectors.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }
}

impl VirtioTransport {
    fn notify_queue(&self, queue: u16) {
        match self {
            Self::Mmio(mmio) => mmio.write(QUEUE_NOTIFY, queue as u32),
            Self::Pci(pci) => pci.notify_queue(queue),
        }
    }
}

impl VirtioPciTransport {
    #[inline]
    fn read_u8(&self, offset: usize) -> u8 {
        unsafe { core::ptr::read_volatile((self.common_cfg_addr + offset) as *const u8) }
    }

    #[inline]
    fn write_u8(&self, offset: usize, value: u8) {
        unsafe { core::ptr::write_volatile((self.common_cfg_addr + offset) as *mut u8, value) }
    }

    #[inline]
    fn read_u16(&self, offset: usize) -> u16 {
        unsafe { core::ptr::read_volatile((self.common_cfg_addr + offset) as *const u16) }
    }

    #[inline]
    fn write_u16(&self, offset: usize, value: u16) {
        unsafe { core::ptr::write_volatile((self.common_cfg_addr + offset) as *mut u16, value) }
    }

    #[inline]
    fn read_u32(&self, offset: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.common_cfg_addr + offset) as *const u32) }
    }

    #[inline]
    fn write_u32(&self, offset: usize, value: u32) {
        unsafe { core::ptr::write_volatile((self.common_cfg_addr + offset) as *mut u32, value) }
    }

    #[inline]
    fn write_u64(&self, offset: usize, value: u64) {
        unsafe { core::ptr::write_volatile((self.common_cfg_addr + offset) as *mut u64, value) }
    }

    fn read_device_features(&self) -> u64 {
        self.write_u32(PCI_COMMON_DEVICE_FEATURE_SELECT, 0);
        let lo = self.read_u32(PCI_COMMON_DEVICE_FEATURE) as u64;
        self.write_u32(PCI_COMMON_DEVICE_FEATURE_SELECT, 1);
        let hi = self.read_u32(PCI_COMMON_DEVICE_FEATURE) as u64;
        lo | (hi << 32)
    }

    fn write_driver_features(&self, features: u64) {
        self.write_u32(PCI_COMMON_DRIVER_FEATURE_SELECT, 0);
        self.write_u32(PCI_COMMON_DRIVER_FEATURE, features as u32);
        self.write_u32(PCI_COMMON_DRIVER_FEATURE_SELECT, 1);
        self.write_u32(PCI_COMMON_DRIVER_FEATURE, (features >> 32) as u32);
    }

    fn read_status(&self) -> u32 {
        self.read_u8(PCI_COMMON_DEVICE_STATUS) as u32
    }

    fn write_status(&self, status: u32) {
        self.write_u8(PCI_COMMON_DEVICE_STATUS, status as u8);
    }

    fn queue_max_size(&self, queue: u16) -> u16 {
        self.write_u16(PCI_COMMON_QUEUE_SELECT, queue);
        self.read_u16(PCI_COMMON_QUEUE_SIZE)
    }

    fn queue_set(&self, queue: u16, size: u16, desc: usize, avail: usize, used: usize) {
        self.write_u16(PCI_COMMON_QUEUE_SELECT, queue);
        self.write_u16(PCI_COMMON_QUEUE_SIZE, size);
        self.write_u64(PCI_COMMON_QUEUE_DESC, desc as u64);
        self.write_u64(PCI_COMMON_QUEUE_DRIVER, avail as u64);
        self.write_u64(PCI_COMMON_QUEUE_DEVICE, used as u64);
        self.write_u16(PCI_COMMON_QUEUE_ENABLE, 1);
    }

    fn notify_queue(&self, queue: u16) {
        unsafe { core::ptr::write_volatile(self.notify_addr as *mut u16, queue) }
    }

    fn read_blk_capacity(&self) -> u64 {
        unsafe { core::ptr::read_volatile(self.device_cfg_addr as *const u64) }
    }
}

/// Initialize the VirtIO-blk driver. Called from rust_main after frame allocator init.
pub fn try_init() -> bool {
    if VIRTIO_BLK.get().is_some() {
        return true;
    }

    let Some(blk) = VirtioBlk::probe_and_init() else {
        return false;
    };

    klog!(
        driver,
        info,
        "initialized, capacity = {} sectors",
        blk.capacity()
    );
    VIRTIO_BLK.call_once(|| crate::hal_common::SpinMutex::new(blk));
    true
}

/// Initialize the VirtIO-blk driver and panic if no block device is present.
pub fn init() {
    assert!(try_init(), "virtio-blk: no block device found");
}

/// Get a reference to the global VirtIO-blk driver.
pub fn get() -> &'static crate::hal_common::SpinMutex<VirtioBlk, 0> {
    VIRTIO_BLK.get().expect("virtio-blk not initialized")
}
