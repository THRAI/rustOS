//! SBI ecall wrappers for rv64.
//!
//! Provides set_timer, send_ipi, hart_start, hart_status via SBI extensions.

/// SBI return value: error in a0, value in a1.
#[derive(Debug)]
pub struct SbiRet {
    pub error: isize,
    pub value: usize,
}

/// Raw SBI ecall with up to 3 arguments.
#[inline]
fn sbi_call(eid: usize, fid: usize, args: [usize; 3]) -> SbiRet {
    let error: isize;
    let value: usize;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a0") args[0],
            in("a1") args[1],
            in("a2") args[2],
            in("a6") fid,
            in("a7") eid,
            lateout("a0") error,
            lateout("a1") value,
        );
    }
    SbiRet { error, value }
}

// SBI extension IDs
const EID_TIME: usize = 0x54494D45; // Timer extension
const EID_SPI: usize = 0x735049;    // s-mode IPI extension
const EID_HSM: usize = 0x48534D;    // Hart State Management

/// Set the next timer interrupt deadline (absolute time value).
/// EID=TIME, FID=0.
pub fn set_timer(stime: u64) {
    sbi_call(EID_TIME, 0, [stime as usize, 0, 0]);
}

/// Send S-mode software interrupt to harts specified by hart_mask.
/// hart_mask is a bitmask relative to hart_mask_base.
/// EID=sPI, FID=0.
pub fn send_ipi(hart_mask: usize, hart_mask_base: usize) {
    sbi_call(EID_SPI, 0, [hart_mask, hart_mask_base, 0]);
}

/// Start a stopped hart. The hart begins execution at start_addr with a0=opaque.
/// EID=HSM, FID=0.
pub fn hart_start(hartid: usize, start_addr: usize, opaque: usize) -> SbiRet {
    sbi_call(EID_HSM, 0, [hartid, start_addr, opaque])
}

/// Query hart state. Returns 0=Started, 1=Stopped, 2=StartPending, etc.
/// EID=HSM, FID=2.
pub fn hart_status(hartid: usize) -> usize {
    sbi_call(EID_HSM, 2, [hartid, 0, 0]).value
}

/// Stop the calling hart. Returns to SBI stopped state.
/// Can be restarted later via hart_start.
/// EID=HSM, FID=1.
pub fn hart_stop() -> SbiRet {
    sbi_call(EID_HSM, 1, [0, 0, 0])
}
