//! VirtIO PCI transport probing for LoongArch QEMU virt.
//!
//! This is not a full data-path implementation yet. It validates that a
//! modern virtio-blk PCI function exposes the expected capability windows and
//! reads the common configuration registers needed for later queue setup.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    drivers::{find_virtio_blk_pci_transport, VirtioBlkPciInfo},
    hal::platform,
};

const PCI_COMMAND_OFFSET: usize = 0x04;
const PCI_COMMAND_MEMORY_SPACE: u16 = 1 << 1;
const PCI_COMMAND_BUS_MASTER: u16 = 1 << 2;
const PCI_BAR0_OFFSET: usize = 0x10;

const VIRTIO_PCI_COMMON_DFSELECT: usize = 0x00;
const VIRTIO_PCI_COMMON_DF: usize = 0x04;
const VIRTIO_PCI_COMMON_NUM_QUEUES: usize = 0x12;
const VIRTIO_PCI_COMMON_QUEUE_SELECT: usize = 0x16;
const VIRTIO_PCI_COMMON_QUEUE_SIZE: usize = 0x18;
const VIRTIO_PCI_COMMON_QUEUE_NOTIFY_OFF: usize = 0x1e;

static PCI_MMIO_ALLOC_CURSOR: AtomicUsize = AtomicUsize::new(0);

#[derive(Copy, Clone, Debug)]
pub struct VirtioBlkPciProbeInfo {
    pub transport: VirtioBlkPciInfo,
    pub common_cfg_addr: usize,
    pub notify_cfg_addr: usize,
    pub queue0_notify_addr: usize,
    pub notify_off_multiplier: u32,
    pub isr_cfg_addr: Option<usize>,
    pub device_cfg_addr: Option<usize>,
    pub device_features: u64,
    pub num_queues: u16,
    pub queue0_max_size: u16,
    pub queue0_notify_off: u16,
}

#[inline]
fn ecam_cfg_addr(ecam_base: usize, bus: u8, device: u8, function: u8, offset: usize) -> usize {
    ecam_base
        + ((bus as usize) << 20)
        + ((device as usize) << 15)
        + ((function as usize) << 12)
        + offset
}

#[inline]
fn read_cfg_aligned_u32(ecam_base: usize, bus: u8, device: u8, function: u8, offset: usize) -> u32 {
    let addr = ecam_cfg_addr(ecam_base, bus, device, function, offset & !0x3);
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

#[inline]
fn read_cfg_u16(ecam_base: usize, bus: u8, device: u8, function: u8, offset: usize) -> u16 {
    let word = read_cfg_aligned_u32(ecam_base, bus, device, function, offset);
    ((word >> ((offset & 0x2) * 8)) & 0xffff) as u16
}

#[inline]
fn write_cfg_u16(ecam_base: usize, bus: u8, device: u8, function: u8, offset: usize, val: u16) {
    let aligned = offset & !0x3;
    let shift = (offset & 0x2) * 8;
    let addr = ecam_cfg_addr(ecam_base, bus, device, function, aligned);
    let mut word = unsafe { core::ptr::read_volatile(addr as *const u32) };
    word &= !(0xffff << shift);
    word |= (val as u32) << shift;
    unsafe { core::ptr::write_volatile(addr as *mut u32, word) }
}

#[inline]
fn write_cfg_u32(ecam_base: usize, bus: u8, device: u8, function: u8, offset: usize, val: u32) {
    let addr = ecam_cfg_addr(ecam_base, bus, device, function, offset & !0x3);
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

#[inline]
fn read_mmio_u16(base: usize, offset: usize) -> u16 {
    unsafe { core::ptr::read_volatile((base + offset) as *const u16) }
}

#[inline]
fn read_mmio_u32(base: usize, offset: usize) -> u32 {
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

#[inline]
fn write_mmio_u16(base: usize, offset: usize, val: u16) {
    unsafe { core::ptr::write_volatile((base + offset) as *mut u16, val) }
}

#[inline]
fn write_mmio_u32(base: usize, offset: usize, val: u32) {
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, val) }
}

#[inline]
const fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

fn alloc_pci_mmio_window(size: usize, align: usize) -> Option<usize> {
    let pi = platform();
    if pi.pci_mmio_base == 0 || pi.pci_mmio_size == 0 || size == 0 {
        return None;
    }

    let window_start = pi.pci_mmio_base;
    let window_end = pi.pci_mmio_base.checked_add(pi.pci_mmio_size)?;
    let align = align.max(0x1000);
    let mut current = PCI_MMIO_ALLOC_CURSOR.load(Ordering::Relaxed);
    loop {
        let start = if current == 0 { window_start } else { current };
        let base = align_up(start.max(window_start), align);
        let next = base.checked_add(size)?;
        if next > window_end {
            return None;
        }
        match PCI_MMIO_ALLOC_CURSOR.compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(base),
            Err(observed) => current = observed,
        }
    }
}

fn required_bar_size(transport: &VirtioBlkPciInfo, bar_index: u8) -> Option<(usize, bool)> {
    if bar_index as usize >= transport.bars.len() {
        return None;
    }

    let mut required = 0usize;
    for cap in [
        transport.common_cfg,
        transport.notify_cfg,
        transport.isr_cfg,
        transport.device_cfg,
    ]
    .into_iter()
    .flatten()
    {
        if cap.bar != bar_index {
            continue;
        }
        let end = cap.offset as usize + cap.length as usize;
        required = required.max(end);
    }

    if required == 0 {
        return None;
    }

    let size = required.next_power_of_two().max(0x1000);
    Some((size, transport.bars[bar_index as usize].is_64))
}

fn program_bar(transport: &VirtioBlkPciInfo, bar_index: u8, addr: u64, is_64: bool) {
    let pi = platform();
    let dev = transport.device;
    let bar_offset = PCI_BAR0_OFFSET + bar_index as usize * 4;
    let orig_low = read_cfg_aligned_u32(pi.pci_ecam_base, dev.bus, dev.device, dev.function, bar_offset);
    let type_bits = orig_low & 0xf;
    write_cfg_u32(
        pi.pci_ecam_base,
        dev.bus,
        dev.device,
        dev.function,
        bar_offset,
        ((addr as u32) & !0xf) | type_bits,
    );
    if is_64 {
        write_cfg_u32(
            pi.pci_ecam_base,
            dev.bus,
            dev.device,
            dev.function,
            bar_offset + 4,
            (addr >> 32) as u32,
        );
    }
}

fn assign_unconfigured_bars(transport: &VirtioBlkPciInfo) -> bool {
    let mut bars_to_assign = [u8::MAX; 4];
    let mut bar_count = 0usize;

    for cap in [
        transport.common_cfg,
        transport.notify_cfg,
        transport.isr_cfg,
        transport.device_cfg,
    ]
    .into_iter()
    .flatten()
    {
        if cap.mmio_addr.is_some() {
            continue;
        }
        if bars_to_assign[..bar_count].contains(&cap.bar) {
            continue;
        }
        if bar_count < bars_to_assign.len() {
            bars_to_assign[bar_count] = cap.bar;
            bar_count += 1;
        }
    }

    if bar_count == 0 {
        return true;
    }

    for &bar in bars_to_assign[..bar_count].iter() {
        let Some((size, is_64)) = required_bar_size(transport, bar) else {
            return false;
        };
        let Some(base) = alloc_pci_mmio_window(size as usize, size as usize) else {
            return false;
        };
        program_bar(transport, bar, base as u64, is_64);
    }
    true
}

fn enable_pci_memory_and_bus_master(transport: &VirtioBlkPciInfo) {
    let pi = platform();
    if !pi.has_pci_host || pi.pci_ecam_base == 0 {
        return;
    }

    let dev = transport.device;
    let command = read_cfg_u16(
        pi.pci_ecam_base,
        dev.bus,
        dev.device,
        dev.function,
        PCI_COMMAND_OFFSET,
    );
    let enabled = command | PCI_COMMAND_MEMORY_SPACE | PCI_COMMAND_BUS_MASTER;
    if enabled != command {
        write_cfg_u16(
            pi.pci_ecam_base,
            dev.bus,
            dev.device,
            dev.function,
            PCI_COMMAND_OFFSET,
            enabled,
        );
    }
}

pub fn probe_virtio_blk_pci() -> Option<VirtioBlkPciProbeInfo> {
    let mut transport = find_virtio_blk_pci_transport()?;
    let need_bar_assignment = transport
        .common_cfg
        .map(|cap| cap.mmio_addr.is_none())
        .unwrap_or(false)
        || transport
            .notify_cfg
            .map(|cap| cap.mmio_addr.is_none())
            .unwrap_or(false);
    if need_bar_assignment && assign_unconfigured_bars(&transport) {
        transport = find_virtio_blk_pci_transport()?;
    }

    let common_cfg = transport.common_cfg?;
    let notify_cfg = transport.notify_cfg?;
    let common_cfg_addr = common_cfg.mmio_addr?;
    let notify_cfg_addr = notify_cfg.mmio_addr?;

    enable_pci_memory_and_bus_master(&transport);

    write_mmio_u32(common_cfg_addr, VIRTIO_PCI_COMMON_DFSELECT, 0);
    let features_lo = read_mmio_u32(common_cfg_addr, VIRTIO_PCI_COMMON_DF);
    write_mmio_u32(common_cfg_addr, VIRTIO_PCI_COMMON_DFSELECT, 1);
    let features_hi = read_mmio_u32(common_cfg_addr, VIRTIO_PCI_COMMON_DF);

    let num_queues = read_mmio_u16(common_cfg_addr, VIRTIO_PCI_COMMON_NUM_QUEUES);
    write_mmio_u16(common_cfg_addr, VIRTIO_PCI_COMMON_QUEUE_SELECT, 0);
    let queue0_max_size = read_mmio_u16(common_cfg_addr, VIRTIO_PCI_COMMON_QUEUE_SIZE);
    let queue0_notify_off = read_mmio_u16(common_cfg_addr, VIRTIO_PCI_COMMON_QUEUE_NOTIFY_OFF);
    let queue0_notify_addr =
        notify_cfg_addr + queue0_notify_off as usize * notify_cfg.notify_off_multiplier as usize;

    Some(VirtioBlkPciProbeInfo {
        transport,
        common_cfg_addr,
        notify_cfg_addr,
        queue0_notify_addr,
        notify_off_multiplier: notify_cfg.notify_off_multiplier,
        isr_cfg_addr: transport.isr_cfg.and_then(|cap| cap.mmio_addr),
        device_cfg_addr: transport.device_cfg.and_then(|cap| cap.mmio_addr),
        device_features: ((features_hi as u64) << 32) | features_lo as u64,
        num_queues,
        queue0_max_size,
        queue0_notify_off,
    })
}
