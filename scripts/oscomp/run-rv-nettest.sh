#!/bin/sh
set +e

BB=/riscv/musl/busybox
echo "start to set up libs"
$BB mkdir -p /lib
$BB ln -sf /riscv/musl/lib/libc.so /lib/ld-musl-riscv64-sf.so.1
$BB ln -sf /riscv/musl/lib/libc.so /lib/ld-musl-riscv64.so.1
echo "finish set up libs"

echo "=== running netperf test ==="
cd /riscv/musl
./busybox sh ./netperf_testcode.sh
echo "=== netperf test done ==="
exit
