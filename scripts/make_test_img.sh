#!/bin/bash
# Create an ext4 test image populated from a staging directory.
# Requires e2fsprogs (brew install e2fsprogs on macOS).
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
IMG_NAME="${RUSTOS_TEST_IMG:-test.img}"
IMG="$SCRIPT_DIR/${IMG_NAME}"
INIT_NAME="${RUSTOS_TEST_INIT:-init}"
BUSYBOX_NAME="${RUSTOS_TEST_BUSYBOX:-busybox}"
INITPROC_NAME="${RUSTOS_TEST_INITPROC:-initproc}"
AUTOTEST_ARCH="${RUSTOS_TEST_AUTOTEST_ARCH:-}"
AUTOTEST_LIBC="${RUSTOS_TEST_AUTOTEST_LIBC:-musl}"
AUTOTEST_GROUPS="${RUSTOS_TEST_AUTOTEST_GROUPS:-basic}"
DEFAULT_IMG_SIZE_KIB=32768
MKE2FS="$(command -v mke2fs || echo /opt/homebrew/opt/e2fsprogs/sbin/mke2fs)"

resolve_binary_path() {
    local requested="$1"
    shift

    if [ -f "$SCRIPT_DIR/$requested" ]; then
        printf '%s\n' "$SCRIPT_DIR/$requested"
        return 0
    fi

    local candidate
    for candidate in "$@"; do
        [ -n "$candidate" ] || continue
        if [ -f "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
        if [ -f "$SCRIPT_DIR/$candidate" ]; then
            printf '%s\n' "$SCRIPT_DIR/$candidate"
            return 0
        fi
    done

    return 1
}

resolve_init_path() {
    local requested="$1"

    if [ "$requested" = "init-la64" ]; then
        resolve_binary_path "$requested" \
            "$SCRIPT_DIR/initproc-la64"
    else
        resolve_binary_path "$requested" \
            "init"
    fi
}

resolve_busybox_path() {
    local requested="$1"

    if [ "$requested" = "busybox-la64" ]; then
        resolve_binary_path "$requested" \
            "$SCRIPT_DIR/../testcase/loongarch/musl/busybox" \
            "$SCRIPT_DIR/../testcase/loongarch/glibc/busybox"
    else
        resolve_binary_path "$requested" \
            "busybox"
    fi
}

resolve_autotest_runner() {
    local arch="$1"

    case "$arch" in
        loongarch)
            resolve_binary_path "oscomp/run-la-select.sh"
            ;;
        *)
            return 1
            ;;
    esac
}

INIT_SRC="$(resolve_init_path "$INIT_NAME" || true)"
BUSYBOX_SRC="$(resolve_busybox_path "$BUSYBOX_NAME" || true)"
INITPROC_SRC="$(resolve_binary_path "$INITPROC_NAME" || true)"
AUTOTEST_RUNNER="$(resolve_autotest_runner "$AUTOTEST_ARCH" || true)"
AUTOTEST_TREE=""

if [ -n "$AUTOTEST_ARCH" ]; then
    AUTOTEST_TREE="$SCRIPT_DIR/../testcase/$AUTOTEST_ARCH/$AUTOTEST_LIBC"
    if [ ! -d "$AUTOTEST_TREE" ]; then
        echo "ERROR: autotest testcase tree not found: $AUTOTEST_TREE" >&2
        exit 1
    fi
    if [ -z "$AUTOTEST_RUNNER" ]; then
        echo "ERROR: no autotest runner for arch=$AUTOTEST_ARCH" >&2
        exit 1
    fi
fi

if [ -f "$IMG" ]; then
    echo "test.img already exists, skipping"
    exit 0
fi

# Build staging directory
STAGING=$(mktemp -d)
trap "rm -rf $STAGING" EXIT

echo "hello from ext4" > "$STAGING/hello.txt"

# Add /bin/init if the binary exists
if [ -n "$INIT_SRC" ] && [ -f "$INIT_SRC" ]; then
    mkdir -p "$STAGING/bin"
    cp "$INIT_SRC" "$STAGING/bin/init"
    chmod 755 "$STAGING/bin/init"
fi

# Add /bin/busybox if the binary exists
if [ -n "$BUSYBOX_SRC" ] && [ -f "$BUSYBOX_SRC" ]; then
    mkdir -p "$STAGING/bin"
    cp "$BUSYBOX_SRC" "$STAGING/bin/busybox"
    chmod 755 "$STAGING/bin/busybox"
    ln -f "$STAGING/bin/busybox" "$STAGING/busybox"

    # Create common BusyBox applet hard links so plain commands resolve via PATH.
    # Use hard links (not symlinks) to avoid symlink type checks in current open path.
    for applet in \
        sh ls cat mkdir rm rmdir mv cp touch pwd echo \
        ln chmod chown uname ps kill grep find head tail wc sort sed awk
    do
        ln -f "$STAGING/bin/busybox" "$STAGING/bin/$applet"
    done
fi

# Add /bin/initproc if the selected binary exists
if [ -n "$INITPROC_SRC" ] && [ -f "$INITPROC_SRC" ]; then
    mkdir -p "$STAGING/bin"
    cp "$INITPROC_SRC" "$STAGING/bin/initproc"
    chmod 755 "$STAGING/bin/initproc"
fi

if [ -n "$AUTOTEST_ARCH" ]; then
    if [ ! -f "$STAGING/bin/busybox" ]; then
        echo "ERROR: autotest image requires /bin/busybox" >&2
        exit 1
    fi

    mkdir -p "$STAGING/$AUTOTEST_ARCH"
    cp -R "$AUTOTEST_TREE" "$STAGING/$AUTOTEST_ARCH/$AUTOTEST_LIBC"
    cp "$AUTOTEST_RUNNER" "$STAGING/$AUTOTEST_ARCH/run-select.sh"
    chmod 755 "$STAGING/$AUTOTEST_ARCH/run-select.sh"
    mkdir -p "$STAGING/lib" "$STAGING/lib64" "$STAGING/proc/1" "$STAGING/dev/misc"

    if [ -n "$BUSYBOX_SRC" ] && [ -f "$BUSYBOX_SRC" ]; then
        cp "$BUSYBOX_SRC" "$STAGING/$AUTOTEST_ARCH/$AUTOTEST_LIBC/busybox"
        chmod 755 "$STAGING/$AUTOTEST_ARCH/$AUTOTEST_LIBC/busybox"
    fi

    while IFS= read -r -d '' script_path; do
        chmod 755 "$script_path"
        if [ "$(head -c 2 "$script_path" 2>/dev/null || true)" != "#!" ]; then
            tmp_script="${script_path}.with-shebang"
            {
                printf '%s\n' '#!/bin/busybox sh'
                cat "$script_path"
            } > "$tmp_script"
            mv "$tmp_script" "$script_path"
        fi
    done < <(find "$STAGING/$AUTOTEST_ARCH/$AUTOTEST_LIBC" -type f -name '*.sh' -print0)

    cat > "$STAGING/proc/mounts" <<EOF
/dev/vda / ext2 rw 0 0
EOF

    cat > "$STAGING/proc/meminfo" <<EOF
MemTotal:       239872 kB
MemFree:        200000 kB
MemAvailable:   200000 kB
Buffers:             0 kB
Cached:              0 kB
SwapCached:          0 kB
SwapTotal:           0 kB
SwapFree:            0 kB
EOF

    printf 'initproc\0' > "$STAGING/proc/1/cmdline"
    cat > "$STAGING/proc/1/stat" <<EOF
1 (initproc) S 0 1 1 0 -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
EOF
    cat > "$STAGING/proc/1/status" <<EOF
Name:	initproc
State:	S (sleeping)
Pid:	1
PPid:	0
Threads:	1
EOF

    if [ -f "$STAGING/$AUTOTEST_ARCH/$AUTOTEST_LIBC/lib/libc.so" ]; then
        ln -sf "/$AUTOTEST_ARCH/$AUTOTEST_LIBC/lib/libc.so" \
            "$STAGING/lib64/ld-linux-loongarch-lp64d.so.1"
        ln -sf "/$AUTOTEST_ARCH/$AUTOTEST_LIBC/lib/libc.so" \
            "$STAGING/lib64/ld-linux-loongarch-lp64.so.1"
    fi

    if [ -f "$STAGING/$AUTOTEST_ARCH/$AUTOTEST_LIBC/lib/ld-linux-loongarch-lp64d.so.1" ]; then
        ln -sf "/$AUTOTEST_ARCH/$AUTOTEST_LIBC/lib/ld-linux-loongarch-lp64d.so.1" \
            "$STAGING/lib64/ld-linux-loongarch-lp64d.so.1"
        ln -sf "/$AUTOTEST_ARCH/$AUTOTEST_LIBC/lib/ld-linux-loongarch-lp64d.so.1" \
            "$STAGING/lib64/ld-linux-loongarch-lp64.so.1"
    fi

    if [ -f "$STAGING/$AUTOTEST_ARCH/$AUTOTEST_LIBC/lib/libc.so.6" ]; then
        ln -sf "/$AUTOTEST_ARCH/$AUTOTEST_LIBC/lib/libc.so.6" "$STAGING/lib/libc.so.6"
    fi

    if [ -f "$STAGING/$AUTOTEST_ARCH/$AUTOTEST_LIBC/lib/libm.so.6" ]; then
        ln -sf "/$AUTOTEST_ARCH/$AUTOTEST_LIBC/lib/libm.so.6" "$STAGING/lib/libm.so.6"
    fi

    cat > "$STAGING/bin/run-oj.sh" <<EOF
#!/bin/sh
/bin/busybox sh /$AUTOTEST_ARCH/run-select.sh "$AUTOTEST_GROUPS" "$AUTOTEST_LIBC"
EOF
    chmod 755 "$STAGING/bin/run-oj.sh"
fi

echo "image payloads:"
[ -n "$INIT_SRC" ] && echo "  init     -> $INIT_SRC" || echo "  init     -> <missing>"
[ -n "$BUSYBOX_SRC" ] && echo "  busybox  -> $BUSYBOX_SRC" || echo "  busybox  -> <missing>"
[ -n "$INITPROC_SRC" ] && echo "  initproc -> $INITPROC_SRC" || echo "  initproc -> <missing>"
[ -n "$AUTOTEST_ARCH" ] && echo "  autotest -> $AUTOTEST_TREE (groups=$AUTOTEST_GROUPS)"

INODE_COUNT="$(find "$STAGING" | wc -l)"
if [ "$INODE_COUNT" -lt 128 ]; then
    INODE_COUNT=128
else
    INODE_COUNT=$((INODE_COUNT + 64))
fi

STAGING_KIB="$(du -sk "$STAGING" | awk '{print $1}')"
IMG_SIZE_KIB="${RUSTOS_TEST_IMG_SIZE_KIB:-}"
if [ -z "$IMG_SIZE_KIB" ]; then
    IMG_SIZE_KIB=$((STAGING_KIB + STAGING_KIB / 2 + 8192))
    if [ "$IMG_SIZE_KIB" -lt "$DEFAULT_IMG_SIZE_KIB" ]; then
        IMG_SIZE_KIB="$DEFAULT_IMG_SIZE_KIB"
    fi
fi
IMG_SIZE_KIB=$((((IMG_SIZE_KIB + 4095) / 4096) * 4096))
IMG_SIZE_MIB=$((IMG_SIZE_KIB / 1024))

echo "Creating ${IMG_SIZE_MIB}MB ext4 test image..."
$MKE2FS -t ext2 -b 1024 -d "$STAGING" -L testfs \
    -O filetype,^ext_attr,^resize_inode,^dir_index,^sparse_super \
    -I 128 -N "$INODE_COUNT" -m 0 -F "$IMG" "$IMG_SIZE_KIB"

echo "Created $IMG (${IMG_SIZE_MIB} MB ext4)"
ls -lh "$IMG"
