#!/bin/sh
set +e

echo "start to set up libs"
mkdir -p /lib
ln -s /riscv/glibc/lib/ld-linux-riscv64-lp64d.so.1 /lib/ld-linux-riscv64-lp64d.so.1
ln -s /riscv/glibc/lib/ld-linux-riscv64-lp64d.so.1 /lib/ld-linux-riscv64-lp64.so.1
ln -s /riscv/glibc/lib/libc.so /lib/libc.so.6
ln -s /riscv/glibc/lib/libm.so /lib/libm.so.6
ln -s /riscv/musl/lib/libc.so /lib/ld-musl-riscv64-sf.so.1
ln -s /riscv/musl/lib/libc.so /lib/ld-musl-riscv64.so.1
echo "finish set up libs"

echo "#### OS COMP TEST GROUP START basic-musl ####"

echo "[debug] pwd before cd:"
/riscv/musl/busybox pwd
cd /riscv/musl/basic
echo "[debug] pwd after cd:"
/riscv/musl/busybox pwd
echo "[debug] ls basic dir:"
/riscv/musl/busybox ls /riscv/musl/basic

for t in brk chdir clone close dup2 dup execve exit fork fstat getcwd getdents getpid getppid gettimeofday mkdir_ mmap mount munmap openat open pipe read sleep times umount uname unlink wait waitpid write yield; do
    echo "Testing $t :"
    /riscv/musl/basic/$t
done

echo "#### OS COMP TEST GROUP END basic-musl ####"
exit
