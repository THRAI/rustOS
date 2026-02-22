# boot.S -- rv64 bare-metal entry point
# a0 = hartid (from OpenSBI), a1 = dtb_ptr

.section .text.entry
.globl _start
_start:
    # a0 = hartid, a1 = dtb_ptr
    # Park harts with hartid >= MAX_CPUS (8)
    li      t0, 8
    bgeu    a0, t0, .park

    # Hart 0 clears .bss
    bnez    a0, .skip_bss_clear

    la      t0, sbss
    la      t1, ebss
.bss_loop:
    bgeu    t0, t1, .skip_bss_clear
    sd      zero, 0(t0)
    addi    t0, t0, 8
    j       .bss_loop

.skip_bss_clear:
    # Compute per-hart stack pointer:
    # sp = boot_stack_top - (hartid * STACK_SIZE)
    # STACK_SIZE = 16384 (16KB)
    la      sp, boot_stack_top
    # hartid * 16384 == hartid << 14 (avoids mul instruction)
    slli    t1, a0, 14
    sub     sp, sp, t1

    # Write 0 to sscratch -- critical: prevents corrupted sp on first trap
    # sscratch == 0 means "trap from kernel mode" in our trap entry logic
    csrw    sscratch, zero

    # Call rust_main(hartid, dtb_ptr)
    # a0 = hartid (already in a0)
    # a1 = dtb_ptr (already in a1)
    call    rust_main

    # rust_main should never return; park if it does
.park:
    wfi
    j       .park
