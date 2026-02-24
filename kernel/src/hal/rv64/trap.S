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

# ===========================================================================
# __trap_return: called from Rust as extern "C" fn(tf_ptr: a0, tc_ptr: a1)
#
# Saves callee-saved registers on kernel stack, loads user state from
# TrapFrame, sets sscratch = TrapContext ptr, sret to userspace.
#
# When user traps back, __user_trap saves user regs, restores callee-saved,
# and "ret" returns to the call site of __trap_return in the Rust future.
# ===========================================================================
.globl __trap_return
.align 4
__trap_return:
    # a0 = TrapFrame pointer
    # a1 = TrapContext pointer (top of kstack - TC_SIZE)

    # Save callee-saved registers below TrapContext on kstack.
    # Save area: 15 slots * 8 bytes = 120 bytes below tc_ptr.
    # Slot 14 holds the caller's sp so __user_trap can restore it.
    #
    # Save caller's sp into t0 before we clobber sp.
    mv      t0, sp
    addi    sp, a1, -15*8
    sd      ra,   0*8(sp)
    sd      s0,   1*8(sp)
    sd      s1,   2*8(sp)
    sd      s2,   3*8(sp)
    sd      s3,   4*8(sp)
    sd      s4,   5*8(sp)
    sd      s5,   6*8(sp)
    sd      s6,   7*8(sp)
    sd      s7,   8*8(sp)
    sd      s8,   9*8(sp)
    sd      s9,  10*8(sp)
    sd      s10, 11*8(sp)
    sd      s11, 12*8(sp)
    sd      gp,  13*8(sp)
    sd      t0,  14*8(sp)          # caller's sp

    # Store kernel sp (callee-saved save area) into TrapContext.kernel_sp
    # so __user_trap can restore it.
    sd      sp, 8(a1)

    # Set stvec to __user_trap
    la      t0, __user_trap
    csrw    stvec, t0

    # Set sscratch = TrapContext pointer
    csrw    sscratch, a1

    # Load user registers from TrapFrame (a0 = tf_ptr)
    mv      t6, a0

    # Restore sstatus and sepc
    ld      t0, 32*8(t6)
    csrw    sstatus, t0
    ld      t0, 33*8(t6)
    csrw    sepc, t0

    # Restore GPRs from TrapFrame
    ld      x1,   1*8(t6)          # ra
    ld      x3,   3*8(t6)          # gp
    ld      x4,   4*8(t6)          # tp (user tp)
    ld      x5,   5*8(t6)          # t0
    ld      x6,   6*8(t6)          # t1
    ld      x7,   7*8(t6)          # t2
    ld      x8,   8*8(t6)          # s0/fp
    ld      x9,   9*8(t6)          # s1
    ld      x10, 10*8(t6)          # a0
    ld      x11, 11*8(t6)          # a1
    ld      x12, 12*8(t6)          # a2
    ld      x13, 13*8(t6)          # a3
    ld      x14, 14*8(t6)          # a4
    ld      x15, 15*8(t6)          # a5
    ld      x16, 16*8(t6)          # a6
    ld      x17, 17*8(t6)          # a7
    ld      x18, 18*8(t6)          # s2
    ld      x19, 19*8(t6)          # s3
    ld      x20, 20*8(t6)          # s4
    ld      x21, 21*8(t6)          # s5
    ld      x22, 22*8(t6)          # s6
    ld      x23, 23*8(t6)          # s7
    ld      x24, 24*8(t6)          # s8
    ld      x25, 25*8(t6)          # s9
    ld      x26, 26*8(t6)          # s10
    ld      x27, 27*8(t6)          # s11
    ld      x28, 28*8(t6)          # t3
    ld      x29, 29*8(t6)          # t4
    ld      x30, 30*8(t6)          # t5

    # Load sp and t6 last (we're using them)
    ld      x2,   2*8(t6)          # sp (user sp)
    ld      x31, 31*8(t6)          # t6

    sret

# ===========================================================================
# __user_trap: entered when trap fires in U-mode.
#
# sscratch = TrapContext pointer:
#   offset  0: trap_frame_ptr
#   offset  8: kernel_sp (callee-saved save area)
#   offset 16: kernel_tp
#   offset 24: scratch0 (for saving t0)
#   offset 32: scratch1 (for saving t1)
#
# Saves all user GPRs + CSRs into TrapFrame, restores callee-saved regs
# from kernel stack, restores stvec to __kernel_trap, clears sscratch,
# and "ret" returns to __trap_return's call site in the Rust future.
# ===========================================================================
.align 4
__user_trap:
    # Step 1: swap sp <-> sscratch. sp = TrapContext ptr, sscratch = user sp.
    csrrw   sp, sscratch, sp

    # Step 2: save t0, t1 to scratch slots before clobbering them.
    sd      t0, 24(sp)              # scratch0 = user t0
    sd      t1, 32(sp)              # scratch1 = user t1

    # Step 3: load TrapFrame pointer into t0.
    ld      t0, 0(sp)               # t0 = trap_frame_ptr

    # Step 4: save user sp (in sscratch) to TrapFrame.
    csrr    t1, sscratch
    sd      t1, 2*8(t0)             # tf.x[2] = user sp

    # Step 5: save all user GPRs to TrapFrame.
    sd      x1,   1*8(t0)           # ra
    # x2 (sp) already saved above
    sd      x3,   3*8(t0)           # gp
    sd      x4,   4*8(t0)           # tp (user tp)
    # x5 (t0) and x6 (t1) were clobbered — restore from scratch slots
    ld      t1, 24(sp)              # reload original user t0
    sd      t1,  5*8(t0)            # tf.x[5] = user t0
    ld      t1, 32(sp)              # reload original user t1
    sd      t1,  6*8(t0)            # tf.x[6] = user t1
    sd      x7,   7*8(t0)           # t2
    sd      x8,   8*8(t0)           # s0/fp
    sd      x9,   9*8(t0)           # s1
    sd      x10, 10*8(t0)           # a0
    sd      x11, 11*8(t0)           # a1
    sd      x12, 12*8(t0)           # a2
    sd      x13, 13*8(t0)           # a3
    sd      x14, 14*8(t0)           # a4
    sd      x15, 15*8(t0)           # a5
    sd      x16, 16*8(t0)           # a6
    sd      x17, 17*8(t0)           # a7
    sd      x18, 18*8(t0)           # s2
    sd      x19, 19*8(t0)           # s3
    sd      x20, 20*8(t0)           # s4
    sd      x21, 21*8(t0)           # s5
    sd      x22, 22*8(t0)           # s6
    sd      x23, 23*8(t0)           # s7
    sd      x24, 24*8(t0)           # s8
    sd      x25, 25*8(t0)           # s9
    sd      x26, 26*8(t0)           # s10
    sd      x27, 27*8(t0)           # s11
    sd      x28, 28*8(t0)           # t3
    sd      x29, 29*8(t0)           # t4
    sd      x30, 30*8(t0)           # t5
    sd      x31, 31*8(t0)           # t6

    # Step 6: save CSRs to TrapFrame.
    csrr    t1, sstatus
    sd      t1, 32*8(t0)            # tf.sstatus
    csrr    t1, sepc
    sd      t1, 33*8(t0)            # tf.sepc
    csrr    t1, scause
    sd      t1, 34*8(t0)            # tf.scause
    csrr    t1, stval
    sd      t1, 35*8(t0)            # tf.stval

    # Step 7: restore kernel tp from TrapContext.
    ld      tp, 16(sp)              # tp = kernel_tp

    # Step 8: restore stvec to __kernel_trap (kernel-mode traps).
    la      t1, __kernel_trap
    csrw    stvec, t1

    # Step 9: clear sscratch (convention: sscratch == 0 means in kernel mode).
    csrw    sscratch, zero

    # Step 10: restore callee-saved registers from kernel stack.
    ld      sp, 8(sp)               # sp = kernel_sp (callee-saved save area)
    ld      ra,   0*8(sp)
    ld      s0,   1*8(sp)
    ld      s1,   2*8(sp)
    ld      s2,   3*8(sp)
    ld      s3,   4*8(sp)
    ld      s4,   5*8(sp)
    ld      s5,   6*8(sp)
    ld      s6,   7*8(sp)
    ld      s7,   8*8(sp)
    ld      s8,   9*8(sp)
    ld      s9,  10*8(sp)
    ld      s10, 11*8(sp)
    ld      s11, 12*8(sp)
    ld      gp,  13*8(sp)

    # Step 11: restore caller's sp from the save area.
    ld      sp, 14*8(sp)

    # Step 12: return to the call site of __trap_return in the Rust future.
    ret
