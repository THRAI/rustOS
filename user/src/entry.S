    .section .text.entry
    .globl _start
_start:
    call main
    mv a0, a0
    li a7, 93
    ecall
1:
    j 1b
