# trap.S -- la64 minimal kernel trap entry/exit for bring-up
#
# Shared TrapFrame layout for now (37 * 8 = 296 bytes):
#   x0-x31     offsets 0..248
#   sstatus    offset 256  (reused for CRMD snapshot on la64)
#   sepc       offset 264  (reused for ERA on la64)
#   scause     offset 272  (reused for ESTAT on la64)
#   stval      offset 280  (reused for BADV on la64)
#   kernel_tp  offset 288

.equ FRAME_SIZE, 296
.equ CSR_PRMD,   0x1
.equ CSR_ESTAT,  0x5
.equ CSR_ERA,    0x6
.equ CSR_BADV,   0x7
.equ CSR_EENTRY, 0xc

.equ TC_TRAP_FRAME_PTR, 0
.equ TC_KERNEL_SP,      8
.equ TC_KERNEL_TP,      16
.equ TC_SCRATCH0,       24
.equ TC_SCRATCH1,       32
.equ SAVE_AREA_SIZE,    112

.section .text
.globl __kernel_trap
.globl __user_trap
.globl __trap_return
.globl __la64_write_kernel_eentry
.globl __la64_write_user_eentry
.globl __la64_read_eentry
.globl __la64_kernel_trap_count
.globl __la64_user_trap_count

.section .bss
.align 3
__la64_kernel_trap_count:
    .dword 0
__la64_user_trap_count:
    .dword 0

.section .text

.align 12

__la64_write_kernel_eentry:
    la.local $t0, __kernel_trap
    csrwr   $t0, CSR_EENTRY
    ret

__la64_write_user_eentry:
    la.local $t0, __user_trap
    csrwr   $t0, CSR_EENTRY
    ret

__la64_read_eentry:
    csrrd   $a0, CSR_EENTRY
    ret

__kernel_trap:
    la.local $t0, __la64_kernel_trap_count
    ld.d    $t1, $t0, 0
    addi.d  $t1, $t1, 1
    st.d    $t1, $t0, 0

    addi.d  $sp, $sp, -FRAME_SIZE

    st.d    $ra, $sp, 1*8
    st.d    $tp, $sp, 2*8
    st.d    $a0, $sp, 4*8
    st.d    $a1, $sp, 5*8
    st.d    $a2, $sp, 6*8
    st.d    $a3, $sp, 7*8
    st.d    $a4, $sp, 8*8
    st.d    $a5, $sp, 9*8
    st.d    $a6, $sp, 10*8
    st.d    $a7, $sp, 11*8
    st.d    $t0, $sp, 12*8
    st.d    $t1, $sp, 13*8
    st.d    $t2, $sp, 14*8
    st.d    $t3, $sp, 15*8
    st.d    $t4, $sp, 16*8
    st.d    $t5, $sp, 17*8
    st.d    $t6, $sp, 18*8
    st.d    $t7, $sp, 19*8
    st.d    $t8, $sp, 20*8
    st.d    $r21, $sp, 21*8
    st.d    $fp, $sp, 22*8
    st.d    $s0, $sp, 23*8
    st.d    $s1, $sp, 24*8
    st.d    $s2, $sp, 25*8
    st.d    $s3, $sp, 26*8
    st.d    $s4, $sp, 27*8
    st.d    $s5, $sp, 28*8
    st.d    $s6, $sp, 29*8
    st.d    $s7, $sp, 30*8
    st.d    $s8, $sp, 31*8

    csrrd   $t0, CSR_PRMD
    st.d    $t0, $sp, 32*8
    csrrd   $t0, CSR_ERA
    st.d    $t0, $sp, 33*8
    csrrd   $t0, CSR_ESTAT
    st.d    $t0, $sp, 34*8
    csrrd   $t0, CSR_BADV
    st.d    $t0, $sp, 35*8

    move    $t0, $tp
    st.d    $t0, $sp, 36*8

    la.local $t0, __la64_kernel_trap_count
    ld.d    $t1, $t0, 0
    addi.d  $t1, $t1, 1
    st.d    $t1, $t0, 0

    move    $a0, $sp
    bl      kernel_trap_handler

    ld.d    $t0, $sp, 32*8
    csrwr   $t0, CSR_PRMD
    ld.d    $t0, $sp, 33*8
    csrwr   $t0, CSR_ERA

    ld.d    $tp, $sp, 2*8
    ld.d    $ra, $sp, 1*8
    ld.d    $a0, $sp, 4*8
    ld.d    $a1, $sp, 5*8
    ld.d    $a2, $sp, 6*8
    ld.d    $a3, $sp, 7*8
    ld.d    $a4, $sp, 8*8
    ld.d    $a5, $sp, 9*8
    ld.d    $a6, $sp, 10*8
    ld.d    $a7, $sp, 11*8
    ld.d    $t0, $sp, 12*8
    ld.d    $t1, $sp, 13*8
    ld.d    $t2, $sp, 14*8
    ld.d    $t3, $sp, 15*8
    ld.d    $t4, $sp, 16*8
    ld.d    $t5, $sp, 17*8
    ld.d    $t6, $sp, 18*8
    ld.d    $t7, $sp, 19*8
    ld.d    $t8, $sp, 20*8
    ld.d    $r21, $sp, 21*8
    ld.d    $fp, $sp, 22*8
    ld.d    $s0, $sp, 23*8
    ld.d    $s1, $sp, 24*8
    ld.d    $s2, $sp, 25*8
    ld.d    $s3, $sp, 26*8
    ld.d    $s4, $sp, 27*8
    ld.d    $s5, $sp, 28*8
    ld.d    $s6, $sp, 29*8
    ld.d    $s7, $sp, 30*8
    ld.d    $s8, $sp, 31*8

    addi.d  $sp, $sp, FRAME_SIZE
    ertn

.align 12
__user_trap:
    csrrd   $sp, 0x30
    st.d    $t0, $sp, TC_SCRATCH0
    st.d    $t1, $sp, TC_SCRATCH1

    ld.d    $t0, $sp, TC_TRAP_FRAME_PTR

    st.d    $ra,  $t0, 1*8
    st.d    $tp,  $t0, 2*8
    st.d    $a0,  $t0, 4*8
    st.d    $a1,  $t0, 5*8
    st.d    $a2,  $t0, 6*8
    st.d    $a3,  $t0, 7*8
    st.d    $a4,  $t0, 8*8
    st.d    $a5,  $t0, 9*8
    st.d    $a6,  $t0, 10*8
    st.d    $a7,  $t0, 11*8
    ld.d    $t1,  $sp, TC_SCRATCH0
    st.d    $t1,  $t0, 12*8
    ld.d    $t1,  $sp, TC_SCRATCH1
    st.d    $t1,  $t0, 13*8
    st.d    $t2,  $t0, 14*8
    st.d    $t3,  $t0, 15*8
    st.d    $t4,  $t0, 16*8
    st.d    $t5,  $t0, 17*8
    st.d    $t6,  $t0, 18*8
    st.d    $t7,  $t0, 19*8
    st.d    $t8,  $t0, 20*8
    st.d    $r21, $t0, 21*8
    st.d    $fp,  $t0, 22*8
    st.d    $s0,  $t0, 23*8
    st.d    $s1,  $t0, 24*8
    st.d    $s2,  $t0, 25*8
    st.d    $s3,  $t0, 26*8
    st.d    $s4,  $t0, 27*8
    st.d    $s5,  $t0, 28*8
    st.d    $s6,  $t0, 29*8
    st.d    $s7,  $t0, 30*8
    st.d    $s8,  $t0, 31*8

    csrrd   $t1, CSR_PRMD
    st.d    $t1,  $t0, 32*8
    csrrd   $t1, CSR_ERA
    st.d    $t1,  $t0, 33*8
    csrrd   $t1, CSR_ESTAT
    st.d    $t1,  $t0, 34*8
    csrrd   $t1, CSR_BADV
    st.d    $t1,  $t0, 35*8

    ld.d    $tp,  $sp, TC_KERNEL_TP
    st.d    $tp,  $t0, 36*8

    la.local $t1, __la64_user_trap_count
    ld.d    $t2, $t1, 0
    addi.d  $t2, $t2, 1
    st.d    $t2, $t1, 0

    la.local $t1, __kernel_trap
    csrwr    $t1, CSR_EENTRY

    ld.d    $sp,  $sp, TC_KERNEL_SP

    ld.d    $ra,  $sp, 0*8
    ld.d    $fp,  $sp, 1*8
    ld.d    $s0,  $sp, 2*8
    ld.d    $s1,  $sp, 3*8
    ld.d    $s2,  $sp, 4*8
    ld.d    $s3,  $sp, 5*8
    ld.d    $s4,  $sp, 6*8
    ld.d    $s5,  $sp, 7*8
    ld.d    $s6,  $sp, 8*8
    ld.d    $s7,  $sp, 9*8
    ld.d    $s8,  $sp, 10*8
    ld.d    $sp,  $sp, 11*8
    ret

__trap_return:
    move    $t0, $a0
    move    $t7, $a1
    move    $t6, $sp

    addi.d  $sp, $a1, -SAVE_AREA_SIZE
    st.d    $ra,  $sp, 0*8
    st.d    $fp,  $sp, 1*8
    st.d    $s0,  $sp, 2*8
    st.d    $s1,  $sp, 3*8
    st.d    $s2,  $sp, 4*8
    st.d    $s3,  $sp, 5*8
    st.d    $s4,  $sp, 6*8
    st.d    $s5,  $sp, 7*8
    st.d    $s6,  $sp, 8*8
    st.d    $s7,  $sp, 9*8
    st.d    $s8,  $sp, 10*8
    st.d    $t6,  $sp, 11*8

    st.d    $sp, $t7, TC_KERNEL_SP
    csrwr    $t7, 0x30

    la.local $t0, __user_trap
    csrwr    $t0, CSR_EENTRY

    ld.d    $t1, $t0, 32*8
    csrwr    $t1, CSR_PRMD
    ld.d    $t1, $t0, 33*8
    csrwr    $t1, CSR_ERA

    ld.d    $tp,  $t0, 2*8
    ld.d    $ra,  $t0, 1*8
    ld.d    $a0,  $t0, 4*8
    ld.d    $a1,  $t0, 5*8
    ld.d    $a2,  $t0, 6*8
    ld.d    $a3,  $t0, 7*8
    ld.d    $a4,  $t0, 8*8
    ld.d    $a5,  $t0, 9*8
    ld.d    $a6,  $t0, 10*8
    ld.d    $a7,  $t0, 11*8
    ld.d    $t2,  $t0, 14*8
    ld.d    $t3,  $t0, 15*8
    ld.d    $t4,  $t0, 16*8
    ld.d    $t5,  $t0, 17*8
    ld.d    $t6,  $t0, 18*8
    ld.d    $t7,  $t0, 19*8
    ld.d    $t8,  $t0, 20*8
    ld.d    $r21, $t0, 21*8
    ld.d    $fp,  $t0, 22*8
    ld.d    $s0,  $t0, 23*8
    ld.d    $s1,  $t0, 24*8
    ld.d    $s2,  $t0, 25*8
    ld.d    $s3,  $t0, 26*8
    ld.d    $s4,  $t0, 27*8
    ld.d    $s5,  $t0, 28*8
    ld.d    $s6,  $t0, 29*8
    ld.d    $s7,  $t0, 30*8
    ld.d    $s8,  $t0, 31*8
    ld.d    $sp,  $t0, 3*8
    ld.d    $t1,  $t0, 13*8
    ld.d    $t0,  $t0, 12*8

    ertn
