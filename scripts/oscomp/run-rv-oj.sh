#!/bin/sh
set +e
echo "start to test riscv"

echo "start to set up libs"
mkdir -p /lib
ln -s /riscv/glibc/lib/ld-linux-riscv64-lp64d.so.1 /lib/ld-linux-riscv64-lp64d.so.1
ln -s /riscv/glibc/lib/ld-linux-riscv64-lp64d.so.1 /lib/ld-linux-riscv64-lp64.so.1
ln -s /riscv/glibc/lib/libc.so /lib/libc.so.6
ln -s /riscv/glibc/lib/libm.so /lib/libm.so.6
ln -s /riscv/musl/lib/libc.so /lib/ld-musl-riscv64-sf.so.1
ln -s /riscv/musl/lib/libc.so /lib/ld-musl-riscv64.so.1
echo "finish set up libs"

echo "start to run musl"
cd /riscv/musl || exit 1
./busybox sh ./basic_testcode.sh
./busybox sh ./busybox_testcode.sh
./busybox sh ./lua_testcode.sh
./busybox sh ./libctest_testcode.sh
./busybox sh ./iozone_testcode.sh
./busybox sh ./unixbench_testcode.sh
./busybox sh ./iperf_testcode.sh
./busybox sh ./libcbench_testcode.sh
./busybox sh ./lmbench_testcode.sh
./busybox sh ./netperf_testcode.sh
./busybox sh ./cyclictest_testcode.sh
./busybox sh ./ltp_testcode.sh

echo "start to run glibc"
cd /riscv/glibc || exit 1
./busybox sh ./basic_testcode.sh
./busybox sh ./busybox_testcode.sh
./busybox sh ./lua_testcode.sh
./busybox sh ./libctest_testcode.sh
./busybox sh ./iozone_testcode.sh
./busybox sh ./unixbench_testcode.sh
./busybox sh ./iperf_testcode.sh
./busybox sh ./libcbench_testcode.sh
./busybox sh ./lmbench_testcode.sh
./busybox sh ./netperf_testcode.sh
./busybox sh ./cyclictest_testcode.sh
./busybox sh ./ltp_testcode.sh
cd /riscv || exit 1

exit
