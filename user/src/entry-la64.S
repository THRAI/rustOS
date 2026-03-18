    .section .text.entry
    .globl _start
_start:
    bl main
    move $a0, $a0
    li.w $a7, 93
    syscall 0
1:
    b 1b
