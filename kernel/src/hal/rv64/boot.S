# boot.S -- rv64 bare-metal entry point
# a0 = hartid (from OpenSBI), a1 = dtb_ptr
#
# OpenSBI can pick ANY hart as the boot hart (not necessarily hart 0).
# First hart to arrive claims boot role, clears BSS, then enters rust_main.
# Other harts spin until BSS is ready, then also enter rust_main.
# Boot hart election happens in rust_main via Rust atomics (safe after BSS clear).

.section .text.entry
.globl _start
_start:
    # a0 = hartid, a1 = dtb_ptr
    # Park harts with hartid >= MAX_CPUS (8)
    li      t0, 8
    bgeu    a0, t0, .park

    # Try to claim boot hart role.
    # _boot_flag lives in .data (not BSS), initialized to 0 by the loader.
    la      t0, _boot_flag
    li      t1, 1
    # Use lr/sc for atomic compare-and-swap
    .option push
    .option arch, +zalrsc
1:  lr.w    t2, (t0)
    bnez    t2, .wait_bss       # already claimed
    sc.w    t3, t1, (t0)
    bnez    t3, 1b              # sc failed, retry
    .option pop

    # We are the boot hart -- clear .bss
    la      t0, sbss
    la      t1, ebss
.bss_loop:
    bgeu    t0, t1, .bss_done
    sd      zero, 0(t0)
    addi    t0, t0, 8
    j       .bss_loop

.bss_done:
    # Signal BSS clear complete
    fence   w, rw
    la      t0, _bss_done
    li      t1, 1
    sw      t1, 0(t0)
    fence   rw, rw
    j       .setup_stack

.wait_bss:
    # Spin until boot hart finishes BSS clear
    la      t0, _bss_done
.wait_loop:
    fence   r, r
    lw      t1, 0(t0)
    beqz    t1, .wait_loop
    fence   r, rw

.setup_stack:
    # Compute per-hart stack pointer:
    # sp = boot_stack_top - (hartid * STACK_SIZE)
    # STACK_SIZE = 65536 (64KB)
    la      sp, boot_stack_top
    slli    t1, a0, 16
    sub     sp, sp, t1

    # Write 0 to sscratch -- critical: prevents corrupted sp on first trap
    csrw    sscratch, zero

    # Call rust_main(hartid, dtb_ptr)
    call    rust_main

    # rust_main should never return; park if it does
.park:
    wfi
    j       .park

# Flags in .data (not BSS) -- initialized to 0 by the binary image.
.section .data
.align 4
.globl _boot_flag
_boot_flag:
    .word 0
.globl _bss_done
_bss_done:
    .word 0
