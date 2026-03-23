    .section .text.la64_user_smoke, "ax"
    .balign 16
    .globl __la64_user_smoke_start
__la64_user_smoke_start:
    li.w    $a0, 1
    addi.d  $a1, $sp, -1
    li.w    $a2, 1
    li.w    $a7, 64
    syscall 0

    li.w    $a0, 0
    li.w    $a7, 93
    syscall 0

1:
    b       1b

    .globl __la64_user_smoke_end
__la64_user_smoke_end:
