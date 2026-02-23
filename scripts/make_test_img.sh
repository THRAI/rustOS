#!/bin/bash
# Create a 32MB ext4 test image with /hello.txt
# Uses Python script (works on macOS without mkfs.ext4 or Docker)
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
IMG="$SCRIPT_DIR/test.img"

if [ -f "$IMG" ]; then
    echo "test.img already exists, skipping"
    exit 0
fi

echo "Creating 32MB ext4 test image..."
python3 "$SCRIPT_DIR/make_ext4_img.py" "$IMG"
