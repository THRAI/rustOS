# trap.S -- rv64 kernel trap entry/exit
#
# TrapFrame layout (37 * 8 = 296 bytes):
#   x0-x31:    offsets 0..248   (32 * 8)
#   sstatus:   offset 256       (32 * 8)
#   sepc:      offset 264       (33 * 8)
#   scause:    offset 272       (34 * 8)
#   stval:     offset 280       (35 * 8)
#   kernel_tp: offset 288       (36 * 8)

.equ FRAME_SIZE, 296

.section .text
.globl __kernel_trap
.globl __user_trap
.align 4

# __kernel_trap: entered when trap fires in S-mode (sscratch == 0).
# sp is already a valid kernel stack pointer.
__kernel_trap:
    # Allocate TrapFrame on kernel stack
    addi    sp, sp, -FRAME_SIZE

    # Save x1 (ra) -- skip x0 (always zero)
    sd      x1,   1*8(sp)
    # Save x3 (gp) -- skip x2 (sp), saved separately below
    sd      x3,   3*8(sp)
    sd      x4,   4*8(sp)
    sd      x5,   5*8(sp)
    sd      x6,   6*8(sp)
    sd      x7,   7*8(sp)
    sd      x8,   8*8(sp)
    sd      x9,   9*8(sp)
    sd      x10, 10*8(sp)
    sd      x11, 11*8(sp)
    sd      x12, 12*8(sp)
    sd      x13, 13*8(sp)
    sd      x14, 14*8(sp)
    sd      x15, 15*8(sp)
    sd      x16, 16*8(sp)
    sd      x17, 17*8(sp)
    sd      x18, 18*8(sp)
    sd      x19, 19*8(sp)
    sd      x20, 20*8(sp)
    sd      x21, 21*8(sp)
    sd      x22, 22*8(sp)
    sd      x23, 23*8(sp)
    sd      x24, 24*8(sp)
    sd      x25, 25*8(sp)
    sd      x26, 26*8(sp)
    sd      x27, 27*8(sp)
    sd      x28, 28*8(sp)
    sd      x29, 29*8(sp)
    sd      x30, 30*8(sp)
    sd      x31, 31*8(sp)

    # Save original sp (before frame allocation) as x2 in TrapFrame
    addi    t0, sp, FRAME_SIZE
    sd      t0,  2*8(sp)

    # Save CSRs
    csrr    t0, sstatus
    sd      t0, 32*8(sp)
    csrr    t0, sepc
    sd      t0, 33*8(sp)
    csrr    t0, scause
    sd      t0, 34*8(sp)
    csrr    t0, stval
    sd      t0, 35*8(sp)

    # Save kernel tp
    sd      tp, 36*8(sp)

    # Call Rust handler: kernel_trap_handler(&mut TrapFrame)
    mv      a0, sp
    call    kernel_trap_handler

    # Restore CSRs
    ld      t0, 32*8(sp)
    csrw    sstatus, t0
    ld      t0, 33*8(sp)
    csrw    sepc, t0
    # scause and stval are read-only from software perspective (no need to restore)

    # Restore kernel tp
    ld      tp, 36*8(sp)

    # Restore GPRs (skip x0, x2/sp restored last)
    ld      x1,   1*8(sp)
    ld      x3,   3*8(sp)
    ld      x4,   4*8(sp)
    ld      x5,   5*8(sp)
    ld      x6,   6*8(sp)
    ld      x7,   7*8(sp)
    ld      x8,   8*8(sp)
    ld      x9,   9*8(sp)
    ld      x10, 10*8(sp)
    ld      x11, 11*8(sp)
    ld      x12, 12*8(sp)
    ld      x13, 13*8(sp)
    ld      x14, 14*8(sp)
    ld      x15, 15*8(sp)
    ld      x16, 16*8(sp)
    ld      x17, 17*8(sp)
    ld      x18, 18*8(sp)
    ld      x19, 19*8(sp)
    ld      x20, 20*8(sp)
    ld      x21, 21*8(sp)
    ld      x22, 22*8(sp)
    ld      x23, 23*8(sp)
    ld      x24, 24*8(sp)
    ld      x25, 25*8(sp)
    ld      x26, 26*8(sp)
    ld      x27, 27*8(sp)
    ld      x28, 28*8(sp)
    ld      x29, 29*8(sp)
    ld      x30, 30*8(sp)
    ld      x31, 31*8(sp)

    # Deallocate TrapFrame
    addi    sp, sp, FRAME_SIZE

    sret

# __user_trap: stub for Phase 1 -- no userspace exists.
# If this vector is ever hit, it means catastrophic kernel state failure.
# Halt immediately with ebreak + infinite loop.
.align 4
__user_trap:
    ebreak
    j       __user_trap
