use crate::hal::platform;

const LA64_MMIO_DMW_BASE: usize = 0x8000_0000_0000_0000;

const PCI_VENDOR_QEMU_VIRTIO: u16 = 0x1af4;
const PCI_DEVICE_VIRTIO_BLOCK_LEGACY: u16 = 0x1001;
const PCI_DEVICE_VIRTIO_BLOCK_MODERN: u16 = 0x1042;

const PCI_STATUS_CAP_LIST: u16 = 1 << 4;
const PCI_CAP_PTR_OFFSET: usize = 0x34;
const PCI_STATUS_OFFSET: usize = 0x06;
const PCI_BAR0_OFFSET: usize = 0x10;
const PCI_CAP_ID_VENDOR: u8 = 0x09;

const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

#[derive(Copy, Clone, Debug, Default)]
pub struct PciBarInfo {
    pub addr: u64,
    pub is_io: bool,
    pub is_64: bool,
}

#[derive(Copy, Clone, Debug)]
pub struct PciDeviceInfo {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision_id: u8,
    pub bar0: u32,
}

#[derive(Copy, Clone, Debug)]
pub struct VirtioPciCapInfo {
    pub cfg_type: u8,
    pub bar: u8,
    pub bar_addr: u64,
    pub offset: u32,
    pub length: u32,
    pub notify_off_multiplier: u32,
    pub mmio_addr: Option<usize>,
}

#[derive(Copy, Clone, Debug)]
pub struct VirtioBlkPciInfo {
    pub device: PciDeviceInfo,
    pub bars: [PciBarInfo; 6],
    pub status: u16,
    pub cap_ptr: u8,
    pub cap_chain_offsets: [u8; 8],
    pub cap_chain_ids: [u8; 8],
    pub cap_chain_len: u8,
    pub common_cfg: Option<VirtioPciCapInfo>,
    pub notify_cfg: Option<VirtioPciCapInfo>,
    pub isr_cfg: Option<VirtioPciCapInfo>,
    pub device_cfg: Option<VirtioPciCapInfo>,
}

#[inline]
fn mmio_phys_to_virt(pa: u64) -> usize {
    LA64_MMIO_DMW_BASE | pa as usize
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
fn read_cfg_u8(ecam_base: usize, bus: u8, device: u8, function: u8, offset: usize) -> u8 {
    let word = read_cfg_aligned_u32(ecam_base, bus, device, function, offset);
    ((word >> ((offset & 0x3) * 8)) & 0xff) as u8
}

#[inline]
fn read_cfg_u16(ecam_base: usize, bus: u8, device: u8, function: u8, offset: usize) -> u16 {
    let word = read_cfg_aligned_u32(ecam_base, bus, device, function, offset);
    ((word >> ((offset & 0x2) * 8)) & 0xffff) as u16
}

#[inline]
fn read_cfg_u32(ecam_base: usize, bus: u8, device: u8, function: u8, offset: usize) -> u32 {
    read_cfg_aligned_u32(ecam_base, bus, device, function, offset)
}

fn read_bars(ecam_base: usize, bus: u8, device: u8, function: u8) -> [PciBarInfo; 6] {
    let mut bars = [PciBarInfo::default(); 6];
    let mut bar = 0usize;

    while bar < bars.len() {
        let raw = read_cfg_u32(ecam_base, bus, device, function, PCI_BAR0_OFFSET + bar * 4);
        if raw == 0 {
            bar += 1;
            continue;
        }

        if raw & 0x1 != 0 {
            bars[bar] = PciBarInfo {
                addr: (raw & !0x3) as u64,
                is_io: true,
                is_64: false,
            };
            bar += 1;
            continue;
        }

        let mem_type = (raw >> 1) & 0x3;
        if mem_type == 0x2 && bar + 1 < bars.len() {
            let high = read_cfg_u32(ecam_base, bus, device, function, PCI_BAR0_OFFSET + (bar + 1) * 4);
            bars[bar] = PciBarInfo {
                addr: ((high as u64) << 32) | ((raw & !0xf) as u64),
                is_io: false,
                is_64: true,
            };
            bar += 2;
        } else {
            bars[bar] = PciBarInfo {
                addr: (raw & !0xf) as u64,
                is_io: false,
                is_64: false,
            };
            bar += 1;
        }
    }

    bars
}

fn parse_virtio_cap(
    ecam_base: usize,
    bus: u8,
    device: u8,
    function: u8,
    cap_ptr: usize,
    bars: &[PciBarInfo; 6],
) -> Option<VirtioPciCapInfo> {
    let cap_len = read_cfg_u8(ecam_base, bus, device, function, cap_ptr + 2);
    if cap_len < 16 {
        return None;
    }

    let cfg_type = read_cfg_u8(ecam_base, bus, device, function, cap_ptr + 3);
    let bar = read_cfg_u8(ecam_base, bus, device, function, cap_ptr + 4);
    if bar as usize >= bars.len() {
        return None;
    }

    let bar_info = bars[bar as usize];
    if bar_info.is_io {
        return None;
    }

    let offset = read_cfg_u32(ecam_base, bus, device, function, cap_ptr + 8);
    let length = read_cfg_u32(ecam_base, bus, device, function, cap_ptr + 12);
    let notify_off_multiplier = if cfg_type == VIRTIO_PCI_CAP_NOTIFY_CFG && cap_len >= 20 {
        read_cfg_u32(ecam_base, bus, device, function, cap_ptr + 16)
    } else {
        0
    };

    Some(VirtioPciCapInfo {
        cfg_type,
        bar,
        bar_addr: bar_info.addr,
        offset,
        length,
        notify_off_multiplier,
        mmio_addr: if bar_info.addr != 0 {
            Some(mmio_phys_to_virt(bar_info.addr + offset as u64))
        } else {
            None
        },
    })
}

pub fn find_virtio_blk_pci_transport() -> Option<VirtioBlkPciInfo> {
    let pi = platform();
    if !pi.has_pci_host || pi.pci_ecam_base == 0 {
        return None;
    }

    for bus in pi.pci_bus_start..=pi.pci_bus_end {
        for device in 0u8..32 {
            for function in 0u8..8 {
                let id = read_cfg_u32(pi.pci_ecam_base, bus, device, function, 0x0);
                let vendor_id = (id & 0xffff) as u16;
                let device_id = (id >> 16) as u16;
                if vendor_id == 0xffff || vendor_id == 0x0000 {
                    if function == 0 {
                        break;
                    }
                    continue;
                }

                if vendor_id != PCI_VENDOR_QEMU_VIRTIO {
                    continue;
                }
                if device_id != PCI_DEVICE_VIRTIO_BLOCK_LEGACY
                    && device_id != PCI_DEVICE_VIRTIO_BLOCK_MODERN
                {
                    continue;
                }

                let class = read_cfg_u32(pi.pci_ecam_base, bus, device, function, 0x08);
                let bars = read_bars(pi.pci_ecam_base, bus, device, function);
                let mut common_cfg = None;
                let mut notify_cfg = None;
                let mut isr_cfg = None;
                let mut device_cfg = None;
                let mut cap_chain_offsets = [0u8; 8];
                let mut cap_chain_ids = [0u8; 8];
                let mut cap_chain_len = 0u8;

                let status = read_cfg_u16(pi.pci_ecam_base, bus, device, function, PCI_STATUS_OFFSET);
                let cap_ptr =
                    read_cfg_u8(pi.pci_ecam_base, bus, device, function, PCI_CAP_PTR_OFFSET);
                if status & PCI_STATUS_CAP_LIST != 0 {
                    let mut cap_cursor = cap_ptr as usize;
                    let mut visited = 0usize;
                    while cap_cursor >= 0x40 && cap_cursor < 0x100 && visited < 32 {
                        let cap_id =
                            read_cfg_u8(pi.pci_ecam_base, bus, device, function, cap_cursor);
                        let next =
                            read_cfg_u8(pi.pci_ecam_base, bus, device, function, cap_cursor + 1)
                                as usize;
                        if (cap_chain_len as usize) < cap_chain_ids.len() {
                            let idx = cap_chain_len as usize;
                            cap_chain_offsets[idx] = cap_cursor as u8;
                            cap_chain_ids[idx] = cap_id;
                            cap_chain_len += 1;
                        }
                        if cap_id == PCI_CAP_ID_VENDOR {
                            if let Some(cap) = parse_virtio_cap(
                                pi.pci_ecam_base,
                                bus,
                                device,
                                function,
                                cap_cursor,
                                &bars,
                            ) {
                                match cap.cfg_type {
                                    VIRTIO_PCI_CAP_COMMON_CFG if common_cfg.is_none() => {
                                        common_cfg = Some(cap);
                                    }
                                    VIRTIO_PCI_CAP_NOTIFY_CFG if notify_cfg.is_none() => {
                                        notify_cfg = Some(cap);
                                    }
                                    VIRTIO_PCI_CAP_ISR_CFG if isr_cfg.is_none() => {
                                        isr_cfg = Some(cap);
                                    }
                                    VIRTIO_PCI_CAP_DEVICE_CFG if device_cfg.is_none() => {
                                        device_cfg = Some(cap);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        if next == 0 || next == cap_cursor {
                            break;
                        }
                        cap_cursor = next;
                        visited += 1;
                    }
                }

                return Some(VirtioBlkPciInfo {
                    device: PciDeviceInfo {
                        bus,
                        device,
                        function,
                        vendor_id,
                        device_id,
                        class_code: (class >> 24) as u8,
                        subclass: (class >> 16) as u8,
                        prog_if: (class >> 8) as u8,
                        revision_id: class as u8,
                        bar0: read_cfg_u32(pi.pci_ecam_base, bus, device, function, PCI_BAR0_OFFSET),
                    },
                    bars,
                    status,
                    cap_ptr,
                    cap_chain_offsets,
                    cap_chain_ids,
                    cap_chain_len,
                    common_cfg,
                    notify_cfg,
                    isr_cfg,
                    device_cfg,
                });
            }
        }
    }

    None
}

pub fn find_virtio_blk_pci() -> Option<PciDeviceInfo> {
    find_virtio_blk_pci_transport().map(|info| info.device)
}
