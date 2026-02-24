# Minimal /bin/init for RISC-V64 — writes "hello from init\n" then exits.
# Linux syscall ABI: a7=syscall#, a0-a5=args, ecall
.global _start
_start:
    # write(1, msg, 16)
    li a7, 64           # SYS_write (Linux rv64)
    li a0, 1            # fd = stdout
    la a1, msg          # buf
    li a2, 16           # len
    ecall

    # exit(0)
    li a7, 93           # SYS_exit
    li a0, 0
    ecall

    # unreachable
1:  j 1b

.section .rodata
msg:
    .ascii "hello from init\n"
