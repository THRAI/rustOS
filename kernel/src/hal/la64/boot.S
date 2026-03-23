# boot.S -- la64 bare-metal entry point for QEMU virt bring-up
# a0 = boot CPU id, a1 = firmware / DT blob pointer (current bring-up contract)

.section .text.entry
.globl _start
_start:
    # QEMU virt provides the current CPU id in CSR 0x20.
    csrrd   $a0, 0x20

    # Single-CPU early bring-up for now.
    # Park any CPU other than CPU 0 until SMP boot is implemented.
    bnez    $a0, .park

    # Clear .bss
    la.local $t0, sbss
    la.local $t1, ebss
1:
    bgeu    $t0, $t1, 2f
    st.d    $zero, $t0, 0
    addi.d  $t0, $t0, 8
    b       1b

2:
    # Set early stack. `boot_stack_top` is provided by the linker script.
    la.local $sp, boot_stack_top

    # Program direct-map windows explicitly so later PG=1 transitions
    # can keep executing kernel text/data in the high-half window.
    ori     $t0, $zero, 0x1
    lu52i.d $t0, $t0, -2048
    csrwr   $t0, 0x180
    ori     $t0, $zero, 0x11
    lu52i.d $t0, $t0, -1792
    csrwr   $t0, 0x181

    # Enable FP/SIMD units for early bring-up.
    # Current la64 kernel builds still emit LSX/LASX instructions in core/alloc
    # paths before we have full per-task extended context management.
    ori     $t0, $zero, 0x7
    csrwr   $t0, 0x2

    # Clear percpu base convention for early bring-up.
    move    $tp, $zero

    # Call rust_main(boot_cpu_id, boot_blob)
    bl      rust_main

.park:
    idle    0
    b       .park
