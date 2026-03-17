#!/bin/sh
# Run selected oscomp test groups.
# Usage: run-rv-select.sh <group>[,<group>...] [libc]
#   group: basic busybox lua libctest iozone unixbench iperf libcbench lmbench netperf cyclictest ltp
#   libc:  musl (default), glibc, all
#
# Examples:
#   run-rv-select.sh basic
#   run-rv-select.sh busybox,lua musl
#   run-rv-select.sh basic,busybox,lua all
set +e

GROUPS="${1:-basic}"
LIBC="${2:-musl}"

BB=/riscv/musl/busybox

# Set up shared library symlinks
echo "start to set up libs"
$BB mkdir -p /lib
$BB ln -sf /riscv/glibc/lib/ld-linux-riscv64-lp64d.so.1 /lib/ld-linux-riscv64-lp64d.so.1
$BB ln -sf /riscv/glibc/lib/ld-linux-riscv64-lp64d.so.1 /lib/ld-linux-riscv64-lp64.so.1
$BB ln -sf /riscv/glibc/lib/libc.so /lib/libc.so.6
$BB ln -sf /riscv/glibc/lib/libm.so /lib/libm.so.6
$BB ln -sf /riscv/musl/lib/libc.so /lib/ld-musl-riscv64-sf.so.1
$BB ln -sf /riscv/musl/lib/libc.so /lib/ld-musl-riscv64.so.1
echo "finish set up libs"

run_groups() {
    libc="$1"
    cd "/riscv/$libc" || return 1

    # Split comma-separated groups
    OLD_IFS="$IFS"
    IFS=","
    for group in $GROUPS; do
        IFS="$OLD_IFS"
        script="./${group}_testcode.sh"
        if [ -f "$script" ]; then
            echo "=== Running $group ($libc) ==="
            ./busybox sh "$script"
        else
            echo "=== SKIP $group ($libc): $script not found ==="
        fi
    done
    IFS="$OLD_IFS"
}

if [ "$LIBC" = "all" ]; then
    run_groups musl
    run_groups glibc
else
    run_groups "$LIBC"
fi

exit
