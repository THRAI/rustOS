# ================================================================
# BSD-Async Rust OS Kernel — Development & Test Container
#
# Runtime image only:
# - QEMU / ext4 / Python / zig live in the image
# - Rust toolchain is mounted from the host via docker-compose.yml
#
# Usage:
#   docker compose build oscomp
#   docker compose run --rm oscomp make oscomp-basic
# ================================================================

FROM ubuntu:24.04

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential cmake \
        qemu-system-misc \
        e2fsprogs \
        python3 python3-pexpect \
        make git curl ca-certificates \
        xz-utils && \
    rm -rf /var/lib/apt/lists/*

# Install zig (cross-compiler backend for lwext4_rust C library)
RUN curl -L https://ziglang.org/download/0.13.0/zig-linux-x86_64-0.13.0.tar.xz | \
    tar -xJ -C /opt && \
    ln -s /opt/zig-linux-x86_64-0.13.0/zig /usr/local/bin/zig

# Install uv (Python package runner, used by test_runner.py)
RUN curl -LsSf https://astral.sh/uv/install.sh | sh
ENV PATH="/root/.local/bin:$PATH"

# Create zig-based musl cross-compiler wrappers
# (same as `make setup-toolchain` but baked into the image)
RUN printf '#!/bin/sh\nexec zig cc -target riscv64-linux-musl "$@"\n' \
        > /usr/local/bin/riscv64-linux-musl-cc && \
    printf '#!/bin/sh\nexec zig ar "$@"\n' \
        > /usr/local/bin/riscv64-linux-musl-ar && \
    printf '#!/bin/sh\nexec zig cc -target loongarch64-linux-musl "$@"\n' \
        > /usr/local/bin/loongarch64-linux-musl-cc && \
    printf '#!/bin/sh\nexec zig ar "$@"\n' \
        > /usr/local/bin/loongarch64-linux-musl-ar && \
    chmod +x /usr/local/bin/riscv64-linux-musl-cc \
             /usr/local/bin/riscv64-linux-musl-ar \
             /usr/local/bin/loongarch64-linux-musl-cc \
             /usr/local/bin/loongarch64-linux-musl-ar

# qemu-system-loongarch64 in this image still probes for the virtio PCI ROM
# filename even when the device ROM BAR is disabled. Point it at an existing
# non-empty ROM blob so la64 PCI smoke tests can proceed to the kernel.
RUN install -d /usr/share/qemu && ln -sf /usr/share/qemu/qboot.rom /usr/share/qemu/efi-virtio.rom

ENV RUSTUP_HOME=/root/.rustup \
    CARGO_HOME=/root/.cargo \
    PATH="/root/.cargo/bin:/root/.local/bin:$PATH"

WORKDIR /workspace
