//! Minimal FDT (Flattened Device Tree) parsing for CPU discovery.
//!
//! Parses the /cpus node to discover hart IDs. Hand-parses the FDT
//! header and structure block -- no external crate needed.
use super::super::super::executor::per_cpu::MAX_CPUS;

/// Maximum physical harts we track (for hartid -> cpu_id mapping).
pub const MAX_HARTS: usize = 64;

/// Global mapping from physical hartid to logical cpu_id.
/// None means the hart is not mapped (parked or beyond MAX_CPUS).
static mut HART_TO_CPU: [Option<usize>; MAX_HARTS] = [None; MAX_HARTS];

/// Global mapping from logical cpu_id to physical hartid.
static mut CPU_TO_HART: [usize; MAX_CPUS] = [0; MAX_CPUS];

/// Parse /cpus node from FDT at dtb_ptr.
/// Returns (num_cpus, array of hartids for each logical cpu).
///
/// Safety: dtb_ptr must point to a valid FDT blob in memory.
pub fn parse_cpus(dtb_ptr: usize) -> (usize, [usize; MAX_CPUS]) {
    let mut hartids = [0usize; MAX_CPUS];

    if dtb_ptr == 0 {
        klog!(smp, error, "dtb_ptr is null, assuming 1 CPU (hart 0)");
        hartids[0] = 0;
        unsafe {
            HART_TO_CPU[0] = Some(0);
            CPU_TO_HART[0] = 0;
        }
        return (1, hartids);
    }

    // Validate FDT magic
    let magic = read_be32(dtb_ptr);
    if magic != 0xd00dfeed {
        klog!(smp, error, "bad magic {:#x}, assuming 1 CPU", magic);
        hartids[0] = 0;
        unsafe {
            HART_TO_CPU[0] = Some(0);
            CPU_TO_HART[0] = 0;
        }
        return (1, hartids);
    }

    let totalsize = read_be32(dtb_ptr + 4) as usize;
    let off_dt_struct = read_be32(dtb_ptr + 8) as usize;
    let off_dt_strings = read_be32(dtb_ptr + 12) as usize;

    let struct_base = dtb_ptr + off_dt_struct;
    let strings_base = dtb_ptr + off_dt_strings;
    let struct_end = dtb_ptr + totalsize;

    // Walk the structure block looking for cpu@ nodes under /cpus
    let mut num_cpus = 0usize;
    let mut pos = struct_base;
    let mut depth: i32 = 0;
    let mut in_cpus = false;
    let mut in_cpu_node = false;

    while pos < struct_end {
        let token = read_be32(pos);
        pos += 4;

        match token {
            // FDT_BEGIN_NODE
            0x00000001 => {
                let name = read_cstr(pos);
                let name_len = name.len();
                // Align to 4 bytes after null terminator
                pos += align4(name_len + 1);
                depth += 1;

                if depth == 2 && starts_with(name, b"cpus") {
                    in_cpus = true;
                } else if in_cpus && depth == 3 && starts_with(name, b"cpu@") {
                    in_cpu_node = true;
                }
            },
            // FDT_END_NODE
            0x00000002 => {
                if in_cpu_node && depth == 3 {
                    in_cpu_node = false;
                }
                if in_cpus && depth == 2 {
                    in_cpus = false;
                }
                depth -= 1;
            },
            // FDT_PROP
            0x00000003 => {
                let val_len = read_be32(pos) as usize;
                let name_off = read_be32(pos + 4) as usize;
                pos += 8;
                let val_ptr = pos;
                pos += align4(val_len);

                if in_cpu_node {
                    let prop_name = read_cstr(strings_base + name_off);
                    if eq_bytes(prop_name, b"reg") && val_len >= 4 {
                        // reg property: hartid (could be 4 or 8 bytes)
                        let hartid = if val_len >= 8 {
                            read_be64(val_ptr) as usize
                        } else {
                            read_be32(val_ptr) as usize
                        };
                        if num_cpus < MAX_CPUS && hartid < MAX_HARTS {
                            hartids[num_cpus] = hartid;
                            unsafe {
                                HART_TO_CPU[hartid] = Some(num_cpus);
                                CPU_TO_HART[num_cpus] = hartid;
                            }
                            num_cpus += 1;
                        }
                    }
                }
            },
            // FDT_NOP
            0x00000004 => {},
            // FDT_END
            0x00000009 => break,
            _ => break,
        }
    }

    if num_cpus == 0 {
        klog!(smp, error, "no CPUs found, assuming 1 CPU (hart 0)");
        hartids[0] = 0;
        num_cpus = 1;
        unsafe {
            HART_TO_CPU[0] = Some(0);
            CPU_TO_HART[0] = 0;
        }
    }

    klog!(smp, info, "discovered {} CPUs", num_cpus);
    (num_cpus, hartids)
}

/// Look up logical cpu_id from physical hartid.
pub fn hart_to_cpu(hartid: usize) -> Option<usize> {
    if hartid >= MAX_HARTS {
        return None;
    }
    unsafe { HART_TO_CPU[hartid] }
}

/// Look up physical hartid from logical cpu_id.
pub fn cpu_to_hart(cpu_id: usize) -> usize {
    assert!(cpu_id < MAX_CPUS, "cpu_id out of range");
    unsafe { CPU_TO_HART[cpu_id] }
}

// --- FDT byte-level helpers ---

fn read_be32(addr: usize) -> u32 {
    unsafe { u32::from_be(core::ptr::read_volatile(addr as *const u32)) }
}

fn read_be64(addr: usize) -> u64 {
    unsafe { u64::from_be(core::ptr::read_volatile(addr as *const u64)) }
}

/// Read a null-terminated C string from addr. Returns byte slice (no null).
fn read_cstr(addr: usize) -> &'static [u8] {
    let mut len = 0usize;
    unsafe {
        while *((addr + len) as *const u8) != 0 {
            len += 1;
            if len > 256 {
                break;
            } // safety limit
        }
        core::slice::from_raw_parts(addr as *const u8, len)
    }
}

/// Align up to 4-byte boundary.
fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// Check if `s` starts with `prefix`.
fn starts_with(s: &[u8], prefix: &[u8]) -> bool {
    s.len() >= prefix.len() && &s[..prefix.len()] == prefix
}

/// Check byte-level equality.
fn eq_bytes(a: &[u8], b: &[u8]) -> bool {
    a == b
}

// ---------------------------------------------------------------------------
// Physical memory region discovery from FDT /memory node
// ---------------------------------------------------------------------------

/// Discovered physical memory region from the FDT `/memory` node.
#[derive(Debug, Copy, Clone)]
pub struct MemRegion {
    pub base: usize,
    pub size: usize,
}

/// Parse the `/memory` node from FDT to discover physical RAM regions.
///
/// Returns a fixed-size array of up to 4 regions and the count found.
/// Falls back to a default 128MB region at `0x8000_0000` if the FDT is
/// missing or unparsable (matching QEMU virt defaults).
///
/// # Safety
/// `dtb_ptr` must point to a valid FDT blob in memory.
pub fn parse_memory(dtb_ptr: usize) -> (usize, [MemRegion; 4]) {
    let empty = MemRegion { base: 0, size: 0 };
    let default_region = MemRegion {
        base: 0x8000_0000,
        size: 128 * 1024 * 1024,
    };
    let mut regions = [empty; 4];

    if dtb_ptr == 0 {
        regions[0] = default_region;
        return (1, regions);
    }

    let magic = read_be32(dtb_ptr);
    if magic != 0xd00dfeed {
        regions[0] = default_region;
        return (1, regions);
    }

    let totalsize = read_be32(dtb_ptr + 4) as usize;
    let off_dt_struct = read_be32(dtb_ptr + 8) as usize;
    let off_dt_strings = read_be32(dtb_ptr + 12) as usize;

    let struct_base = dtb_ptr + off_dt_struct;
    let strings_base = dtb_ptr + off_dt_strings;
    let struct_end = dtb_ptr + totalsize;

    let mut num_regions = 0usize;
    let mut pos = struct_base;
    let mut depth: i32 = 0;
    let mut in_memory_node = false;

    while pos < struct_end {
        let token = read_be32(pos);
        pos += 4;

        match token {
            // FDT_BEGIN_NODE
            0x00000001 => {
                let name = read_cstr(pos);
                let name_len = name.len();
                pos += align4(name_len + 1);
                depth += 1;

                // /memory or /memory@XXXXXXXX at depth 1
                if depth == 1 && starts_with(name, b"memory") {
                    in_memory_node = true;
                }
            },
            // FDT_END_NODE
            0x00000002 => {
                if in_memory_node && depth == 1 {
                    in_memory_node = false;
                }
                depth -= 1;
            },
            // FDT_PROP
            0x00000003 => {
                let val_len = read_be32(pos) as usize;
                let name_off = read_be32(pos + 4) as usize;
                pos += 8;
                let val_ptr = pos;
                pos += align4(val_len);

                if in_memory_node {
                    let prop_name = read_cstr(strings_base + name_off);
                    if eq_bytes(prop_name, b"reg") {
                        // Parse reg property: pairs of (base, size).
                        // Each cell is typically 8 bytes (#address-cells=2, #size-cells=2)
                        // for 64-bit platforms, but may be 4 bytes on 32-bit.
                        // QEMU virt uses 2+2 cells (16 bytes per entry).
                        let entry_size = 16; // 8-byte base + 8-byte size
                        let mut off = 0;
                        while off + entry_size <= val_len && num_regions < 4 {
                            let base = read_be64(val_ptr + off) as usize;
                            let size = read_be64(val_ptr + off + 8) as usize;
                            if size > 0 {
                                regions[num_regions] = MemRegion { base, size };
                                num_regions += 1;
                            }
                            off += entry_size;
                        }
                    }
                }
            },
            // FDT_NOP
            0x00000004 => {},
            // FDT_END
            0x00000009 => break,
            _ => break,
        }
    }

    if num_regions == 0 {
        klog!(
            boot,
            warn,
            "FDT: no /memory node found, assuming 128MB at 0x8000_0000"
        );
        regions[0] = default_region;
        return (1, regions);
    }

    klog!(
        boot,
        info,
        "FDT: discovered {} memory region(s)",
        num_regions
    );
    for i in 0..num_regions {
        klog!(
            boot,
            info,
            "  region {}: base={:#x} size={:#x} ({}MB)",
            i,
            regions[i].base,
            regions[i].size,
            regions[i].size / (1024 * 1024)
        );
    }
    (num_regions, regions)
}
