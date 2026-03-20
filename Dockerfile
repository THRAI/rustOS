# ================================================================
# BSD-Async Rust OS Kernel — Development & Test Container
#
# Two-stage build:
#   Stage 1 (toolchain): Rust nightly-2025-06-01 + cargo-binutils
#   Stage 2 (runner):    Ubuntu 24.04 + QEMU + e2fsprogs + uv/Python
#
# Usage:
#   docker compose build oscomp
#   docker compose run --rm oscomp make oscomp-basic
# ================================================================

# ---- Stage 1: Rust toolchain (cached, ~1.5 GB) -----------------
FROM ubuntu:24.04 AS toolchain

RUN apt-get update && apt-get install -y --no-install-recommends \
        curl ca-certificates build-essential && \
    rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH="/usr/local/cargo/bin:$PATH"

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain nightly-2025-06-01 \
        --component rust-src,llvm-tools,rustfmt,clippy \
        --target riscv64gc-unknown-none-elf \
        --target loongarch64-unknown-none && \
    cargo install cargo-binutils && \
    rm -rf /usr/local/cargo/registry /tmp/*

# ---- Stage 2: Runtime ------------------------------------------
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

# Copy Rust toolchain from stage 1
COPY --from=toolchain /usr/local/rustup /usr/local/rustup
COPY --from=toolchain /usr/local/cargo  /usr/local/cargo

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH="/usr/local/cargo/bin:$PATH"

WORKDIR /workspace
