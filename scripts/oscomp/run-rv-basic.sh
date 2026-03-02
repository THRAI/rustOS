#!/bin/sh
set +e

BB=/riscv/musl/busybox
echo "start to set up libs"
$BB mkdir -p /lib
$BB ln -sf /riscv/glibc/lib/ld-linux-riscv64-lp64d.so.1 /lib/ld-linux-riscv64-lp64d.so.1
$BB ln -sf /riscv/glibc/lib/ld-linux-riscv64-lp64d.so.1 /lib/ld-linux-riscv64-lp64.so.1
$BB ln -sf /riscv/glibc/lib/libc.so /lib/libc.so.6
$BB ln -sf /riscv/glibc/lib/libm.so /lib/libm.so.6
$BB ln -sf /riscv/musl/lib/libc.so /lib/ld-musl-riscv64-sf.so.1
$BB ln -sf /riscv/musl/lib/libc.so /lib/ld-musl-riscv64.so.1
echo "finish set up libs"

echo "#### OS COMP TEST GROUP START basic-musl ####"
cd /riscv/musl/basic || exit 1

for t in brk chdir clone close dup2 dup execve exit fork fstat getcwd getdents getpid getppid gettimeofday mkdir_ mmap mount munmap openat open pipe read sleep times umount uname unlink wait waitpid write yield; do
    echo "Testing $t :"
    ./$t
done

echo "#### OS COMP TEST GROUP END basic-musl ####"
exit
