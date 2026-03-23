#!/bin/sh
# Run selected LoongArch oscomp test groups from the minimal la64 test image.
# Keep this script intentionally simple: avoid command substitution and
# subshell-heavy constructs so bring-up shells can execute it reliably.
set +e

GROUPS="${1:-basic}"
LIBC="${2:-musl}"
ROOT="/loongarch/$LIBC"
SUITE_ROOT="$ROOT"

if [ -f "$ROOT/basic_testcode.sh" ] || [ -f "$ROOT/busybox_testcode.sh" ]; then
    SUITE_ROOT="$ROOT"
elif [ -f "$ROOT/$LIBC/basic_testcode.sh" ] || [ -f "$ROOT/$LIBC/busybox_testcode.sh" ]; then
    SUITE_ROOT="$ROOT/$LIBC"
fi

cd "$SUITE_ROOT" || {
    echo "=== FAIL: cannot cd to $SUITE_ROOT ==="
    exit 1
}

OLD_IFS="$IFS"
IFS=","
set -- $GROUPS
IFS="$OLD_IFS"

for group in "$@"; do
    script="./${group}_testcode.sh"
    if [ -f "$script" ]; then
        echo "=== Running $group ($LIBC) ==="
        ./busybox sh "$script"
    else
        echo "=== SKIP $group ($LIBC): $script not found ==="
    fi
done

exit 0
