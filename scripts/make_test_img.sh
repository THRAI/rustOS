#!/bin/bash
# Create a 32MB ext4 test image populated from a staging directory.
# Requires e2fsprogs (brew install e2fsprogs on macOS).
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
IMG="$SCRIPT_DIR/test.img"
MKE2FS="$(command -v mke2fs || echo /opt/homebrew/opt/e2fsprogs/sbin/mke2fs)"

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

    # Create common BusyBox applet hard links so plain commands resolve via PATH.
    # Use hard links (not symlinks) to avoid symlink type checks in current open path.
    for applet in \
        sh ls cat mkdir rm rmdir mv cp touch pwd echo \
        ln chmod chown uname ps kill grep find head tail wc sort sed awk
    do
        ln -f "$STAGING/bin/busybox" "$STAGING/bin/$applet"
    done
fi

# Add /bin/initproc if the binary exists
if [ -f "$SCRIPT_DIR/initproc" ]; then
    mkdir -p "$STAGING/bin"
    cp "$SCRIPT_DIR/initproc" "$STAGING/bin/initproc"
    chmod 755 "$STAGING/bin/initproc"
fi

echo "Creating 32MB ext4 test image..."
$MKE2FS -t ext2 -b 1024 -d "$STAGING" -L testfs \
    -O filetype,^ext_attr,^resize_inode,^dir_index,^sparse_super \
    -I 128 -N 128 -m 0 -F "$IMG" 32768

echo "Created $IMG (32 MB ext4)"
ls -lh "$IMG"
