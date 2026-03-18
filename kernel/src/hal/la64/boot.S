# boot.S -- la64 bare-metal entry point for QEMU virt bring-up
# a0 = boot CPU id, a1 = firmware / DT blob pointer (current bring-up contract)

.section .text.entry
.globl _start
_start:
    # Single-CPU early bring-up for now.
    # Park any CPU other than CPU 0 until SMP boot is implemented.
    bnez    a0, .park

    # Clear .bss
    la.local t0, sbss
    la.local t1, ebss
1:
    bgeu    t0, t1, 2f
    st.d    zero, t0, 0
    addi.d  t0, t0, 8
    b       1b

2:
    # Set early stack. `boot_stack_top` is provided by the linker script.
    la.local sp, boot_stack_top

    # Clear percpu base convention for early bring-up.
    move    tp, zero

    # Call rust_main(boot_cpu_id, boot_blob)
    bl      rust_main

.park:
    idle    0
    b       .park
