#!/bin/bash
# Create a 32MB ext4 test image populated from a staging directory.
# Requires e2fsprogs (brew install e2fsprogs on macOS).
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
IMG="$SCRIPT_DIR/test.img"
MKE2FS="/opt/homebrew/opt/e2fsprogs/sbin/mke2fs"

if [ -f "$IMG" ]; then
    echo "test.img already exists, skipping"
    exit 0
fi

# Build staging directory
STAGING=$(mktemp -d)
trap "rm -rf $STAGING" EXIT

echo "hello from ext4" > "$STAGING/hello.txt"

# Add /bin/init if the binary exists
if [ -f "$SCRIPT_DIR/init" ]; then
    mkdir -p "$STAGING/bin"
    cp "$SCRIPT_DIR/init" "$STAGING/bin/init"
    chmod 755 "$STAGING/bin/init"
fi

# Add /bin/busybox if the binary exists
if [ -f "$SCRIPT_DIR/busybox" ]; then
    mkdir -p "$STAGING/bin"
    cp "$SCRIPT_DIR/busybox" "$STAGING/bin/busybox"
    chmod 755 "$STAGING/bin/busybox"
fi

echo "Creating 32MB ext4 test image..."
$MKE2FS -t ext2 -b 1024 -d "$STAGING" -L testfs \
    -O filetype,^ext_attr,^resize_inode,^dir_index,^sparse_super \
    -I 128 -N 128 -m 0 -F "$IMG" 32768

echo "Created $IMG (32 MB ext4)"
ls -lh "$IMG"
