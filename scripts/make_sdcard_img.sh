#!/usr/bin/env bash
# Build the 512MB ext4 competition disk image without mount/sudo.
# Uses mke2fs -d (populate from directory) — works on Linux and macOS.
#
# Usage: ./make_sdcard_img.sh [run-script] [groups] [libc]
#   run-script: oscomp run script name (default: run-rv-basic.sh)
#               looked up in scripts/oscomp/
#   groups:     comma-separated test groups for run-rv-select.sh (optional)
#   libc:       musl|glibc|all for run-rv-select.sh (optional)
#
# When groups is set, run-rv-select.sh is used instead of run-script.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
TESTCASE_DIR="${PROJECT_DIR}/testcase"
OUTFILE="${SCRIPT_DIR}/sdcard-rv.img"
OSCOMP_SCRIPT="${1:-run-rv-basic.sh}"
OSCOMP_GROUPS="${2:-}"
OSCOMP_LIBC="${3:-musl}"

# --- Locate mke2fs (macOS Homebrew fallback) ---
MKE2FS="mke2fs"
if ! command -v "$MKE2FS" &>/dev/null; then
    if [[ -x /opt/homebrew/opt/e2fsprogs/sbin/mke2fs ]]; then
        MKE2FS=/opt/homebrew/opt/e2fsprogs/sbin/mke2fs
    elif [[ -x /usr/local/opt/e2fsprogs/sbin/mke2fs ]]; then
        MKE2FS=/usr/local/opt/e2fsprogs/sbin/mke2fs
    else
        echo "ERROR: mke2fs not found. Install e2fsprogs." >&2
        echo "  macOS:  brew install e2fsprogs" >&2
        echo "  Linux:  sudo apt install e2fsprogs" >&2
        exit 1
    fi
fi

# --- Validate inputs ---
if [[ ! -d "${TESTCASE_DIR}/riscv" ]]; then
    echo "ERROR: testcase/riscv/ not found at ${TESTCASE_DIR}" >&2
    exit 1
fi

RUN_SCRIPT="${SCRIPT_DIR}/oscomp/${OSCOMP_SCRIPT}"
if [[ ! -f "$RUN_SCRIPT" ]]; then
    echo "ERROR: run script not found: ${RUN_SCRIPT}" >&2
    exit 1
fi

# --- Create staging directory ---
STAGING=$(mktemp -d)
trap 'rm -rf "$STAGING"' EXIT

echo "Staging testcase tree..."
cp -r "${TESTCASE_DIR}/riscv" "${STAGING}/riscv"

# Copy initproc
mkdir -p "${STAGING}/bin" "${STAGING}/lib" "${STAGING}/lib64" "${STAGING}/etc"
if [[ -f "${SCRIPT_DIR}/initproc" ]]; then
    cp "${SCRIPT_DIR}/initproc" "${STAGING}/bin/initproc"
    chmod 755 "${STAGING}/bin/initproc"
fi

# Create /bin/sh and /bin/busybox symlinks so scripts with shebangs
# (#!/bin/sh, #!/bin/busybox sh, #!/busybox sh) can resolve.
# The actual busybox binary lives at /riscv/{musl,glibc}/busybox.
if [[ -f "${STAGING}/riscv/musl/busybox" ]]; then
    cp "${STAGING}/riscv/musl/busybox" "${STAGING}/bin/busybox"
    chmod 755 "${STAGING}/bin/busybox"
    ln -f "${STAGING}/bin/busybox" "${STAGING}/bin/sh"
    ln -f "${STAGING}/bin/busybox" "${STAGING}/busybox"
fi

# Create musl dynamic linker symlinks.
# Many test binaries (iozone, dhry2reg, unixbench, etc.) are dynamically
# linked with PT_INTERP = /lib/ld-musl-riscv64-sf.so.1.  The run scripts
# (run-rv-oj.sh etc.) create some symlinks at runtime, but we also need
# them baked into the image for non-basic test groups.
mkdir -p "${STAGING}/lib"
if [[ -f "${STAGING}/riscv/musl/lib/libc.so" ]]; then
    ln -sf /riscv/musl/lib/libc.so "${STAGING}/lib/ld-musl-riscv64-sf.so.1"
    ln -sf /riscv/musl/lib/libc.so "${STAGING}/lib/ld-musl-riscv64.so.1"
fi
if [[ -f "${STAGING}/riscv/glibc/lib/ld-linux-riscv64-lp64d.so.1" ]]; then
    ln -sf /riscv/glibc/lib/ld-linux-riscv64-lp64d.so.1 "${STAGING}/lib/ld-linux-riscv64-lp64d.so.1"
    ln -sf /riscv/glibc/lib/ld-linux-riscv64-lp64d.so.1 "${STAGING}/lib/ld-linux-riscv64-lp64.so.1"
    ln -sf /riscv/glibc/lib/libc.so "${STAGING}/lib/libc.so.6"
    ln -sf /riscv/glibc/lib/libm.so "${STAGING}/lib/libm.so.6"
fi

# Copy the selected oscomp run script.
# If OSCOMP_GROUPS is set, generate a wrapper that calls run-rv-select.sh.
if [[ -n "$OSCOMP_GROUPS" ]]; then
    cp "${SCRIPT_DIR}/oscomp/run-rv-select.sh" "${STAGING}/riscv/run-rv-select.sh"
    cat > "${STAGING}/riscv/run-oj.sh" <<WRAPPER
#!/bin/sh
/riscv/musl/busybox sh /riscv/run-rv-select.sh "$OSCOMP_GROUPS" "$OSCOMP_LIBC"
WRAPPER
    chmod +x "${STAGING}/riscv/run-oj.sh"
    echo "Using run-rv-select.sh groups=${OSCOMP_GROUPS} libc=${OSCOMP_LIBC}"
else
    cp "$RUN_SCRIPT" "${STAGING}/riscv/run-oj.sh"
fi

# Make all shell scripts executable
find "$STAGING" -type f -name '*.sh' -exec chmod +x {} \;

# --- Build ext4 image (512 MB, no mount needed) ---
rm -f "$OUTFILE"
echo "Creating 512 MB ext4 image with mke2fs -d (no mount/sudo needed)..."
"$MKE2FS" -t ext4 \
    -d "$STAGING" \
    -O ^metadata_csum_seed \
    -b 4096 \
    -r 1 \
    -N 8192 \
    -m 0 \
    -F \
    "$OUTFILE" \
    131072   # 131072 blocks × 4096 bytes = 512 MB

echo "=== scripts/sdcard-rv.img ready (512 MB ext4, script=${OSCOMP_SCRIPT}) ==="
ls -lh "$OUTFILE"
