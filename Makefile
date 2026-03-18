# BSD-Async Rust OS Kernel -- Top-level Makefile

HOST_TARGET := $(shell rustc -vV | grep host | cut -d' ' -f2)
UNAME_S := $(shell uname -s)
ARCH ?= rv64

# LA64 bring-up note:
# - `cargo build -p kernel --target loongarch64-unknown-none` currently reaches
#   the external C dependency build for `lwext4_rust`.
# - That path expects a host-visible `loongarch64-linux-musl-cc` toolchain
#   (and matching binutils such as `ar`) for the ext4 C library build.
# - Until that toolchain is installed or the ext4 path is gated off for
#   kernel-only bring-up, la64 kernel builds may fail before reaching HAL code.

TARGET_rv64 := riscv64gc-unknown-none-elf
TARGET_la64 := loongarch64-unknown-none
KERNEL_ELF_rv64 := target/$(TARGET_rv64)/release/kernel
KERNEL_ELF_la64 := target/$(TARGET_la64)/release/kernel
USER_ELF_rv64 := user/target/$(TARGET_rv64)/release/initproc
USER_ELF_la64 := user/target/$(TARGET_la64)/release/initproc
KERNEL_BIN_rv64 := kernel-rv64.bin
KERNEL_BIN_la64 := kernel-la64.bin
USER_INSTALL_rv64 := scripts/initproc
USER_INSTALL_la64 := scripts/initproc-la64
USER_LINKER_rv64 := src/linker.ld
USER_LINKER_la64 := src/linker-la64.ld
USER_RUSTFLAGS_rv64 := -Clink-arg=-Tsrc/linker.ld
USER_RUSTFLAGS_la64 := -Clink-arg=-Tsrc/linker-la64.ld
KERNEL_LINKER_rv64 := kernel/linker/rv64-qemu.ld
KERNEL_LINKER_la64 := kernel/linker/la64-qemu.ld
KERNEL_RUSTFLAGS_rv64 := -Clink-arg=-Tkernel/linker/rv64-qemu.ld
KERNEL_RUSTFLAGS_la64 := -Clink-arg=-Tkernel/linker/la64-qemu.ld

QEMU_rv64 := qemu-system-riscv64
QEMU_la64 := qemu-system-loongarch64
SMP ?= 4
QEMU_TRACE ?=
DISK_IMG_rv64 := scripts/test.img
DISK_IMG_la64 :=
OBJCOPY_ARCH_rv64 := riscv64
OBJCOPY_ARCH_la64 := loongarch64
QEMU_rv64_FLAGS := -machine virt -nographic -bios default -kernel $(KERNEL_BIN_rv64) -smp $(SMP) -m 128M \
	-drive file=$(DISK_IMG_rv64),format=raw,if=none,id=hd0 -device virtio-blk-device,drive=hd0 $(QEMU_TRACE)
QEMU_la64_FLAGS := -machine virt -cpu la464 -nographic -kernel $(KERNEL_BIN_la64) -smp $(SMP) -m 128M $(QEMU_TRACE)

TARGET = $(TARGET_$(ARCH))
KERNEL_ELF = $(KERNEL_ELF_$(ARCH))
USER_ELF = $(USER_ELF_$(ARCH))
KERNEL_BIN = $(KERNEL_BIN_$(ARCH))
USER_INSTALL = $(USER_INSTALL_$(ARCH))
USER_LINKER = $(USER_LINKER_$(ARCH))
USER_RUSTFLAGS = $(USER_RUSTFLAGS_$(ARCH))
KERNEL_LINKER = $(KERNEL_LINKER_$(ARCH))
KERNEL_RUSTFLAGS = $(KERNEL_RUSTFLAGS_$(ARCH))
QEMU = $(QEMU_$(ARCH))
QEMU_FLAGS = $(QEMU_$(ARCH)_FLAGS)
DISK_IMG = $(DISK_IMG_$(ARCH))
OBJCOPY_ARCH = $(OBJCOPY_ARCH_$(ARCH))

RUN_DEPS_rv64 := $(DISK_IMG_rv64)
RUN_DEPS_la64 :=
RUN_DEPS = $(RUN_DEPS_$(ARCH))

# ---- 赛题评测标准参数 ----
# TEST_FS: 含测试程序的 ext4 磁盘镜像（sdcard-rv.img 或赛平台注入的 fs 镜像）
TEST_FS ?= scripts/sdcard-rv.img
# OSCOMP_FLAGS 严格对齐赛题 README 的 QEMU 启动命令
OSCOMP_FLAGS := -machine virt -nographic -bios default -kernel $(KERNEL_BIN_rv64) \
	-smp $(SMP) -m 128M -no-reboot \
	-drive file=$(TEST_FS),if=none,format=raw,id=x0 \
	-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
	-device virtio-net-device,netdev=net -netdev user,id=net \
	-rtc base=utc

OBJCOPY := rust-objcopy

# Kernel log control: LOG=all | LOG=boot,fs,driver | (empty = all modules)
# Available modules: boot syscall trap vm sched fs driver smp signal pipe exec proc
comma := ,
space := $(subst ,, )
LOG ?= all
LEVEL ?= error
ifdef LOG
  ifeq ($(LOG),all)
    _LOG_FEATURES := log-all
  else
    _LOG_FEATURES := $(patsubst %,log-%,$(subst $(comma), ,$(LOG)))
  endif
endif

_LOG_LEVEL_FEATURE := log-level-$(LEVEL)

ifdef _LOG_FEATURES
  _CARGO_LOG = --features $(subst $(space),$(comma),$(_LOG_FEATURES)),$(_LOG_LEVEL_FEATURE)
  _TEST_FEATURES = qemu-test,$(subst $(space),$(comma),$(_LOG_FEATURES)),$(_LOG_LEVEL_FEATURE)
else
  _CARGO_LOG = --features $(_LOG_LEVEL_FEATURE)
  _TEST_FEATURES = qemu-test,$(_LOG_LEVEL_FEATURE)
endif

.PHONY: all kernel user run debug gdbserver smoke kernel-rv kernel-rv64 kernel-la64 kernel-rv64-test kernel-rv64-autotest user-rv64 user-la64 user-rv64-autotest run-rv64 run-la64 run-oscomp sdcard-rv oscomp oscomp-basic oscomp-basic-all debug-rv64 debug-la64 gdbserver-rv64 gdbserver-la64 smoke-la64 qemu-test-rv64 agent-test test test-all disk-img setup-toolchain clean

# 赛题评测入口：make all 产出 ELF 格式的 kernel-rv（autotest 模式，自动跑测试脚本后关机）
all: kernel-rv

kernel-rv:
	cargo build --release -p kernel --target $(TARGET_rv64) --features autotest,$(_LOG_LEVEL_FEATURE)
	cp $(KERNEL_ELF_rv64) kernel-rv

kernel:
	cargo build --release -p kernel --target $(TARGET) $(_CARGO_LOG) --config 'target.$(TARGET).rustflags=["-C","link-arg=-T$(CURDIR)/$(KERNEL_LINKER)"]'
	$(OBJCOPY) --binary-architecture=$(OBJCOPY_ARCH) $(KERNEL_ELF) --strip-all -O binary $(KERNEL_BIN)

kernel-rv64: ARCH=rv64
kernel-rv64: kernel

kernel-la64: ARCH=la64
kernel-la64: kernel

user:
	@if [ "$(ARCH)" = "rv64" ]; then \
		cd /tmp && RUSTUP_TOOLCHAIN=nightly-2025-06-01 CARGO_TARGET_DIR='$(CURDIR)/user/target' cargo build --manifest-path '$(CURDIR)/user/Cargo.toml' --release --target $(TARGET_rv64) --config 'build.target="$(TARGET_rv64)"' --config 'target.$(TARGET_rv64).rustflags=["-C","link-arg=-T$(CURDIR)/user/$(USER_LINKER_rv64)"]'; \
	else \
		cd /tmp && RUSTUP_TOOLCHAIN=nightly-2025-06-01 CARGO_TARGET_DIR='$(CURDIR)/user/target' cargo build --manifest-path '$(CURDIR)/user/Cargo.toml' --release --target $(TARGET_la64) --config 'build.target="$(TARGET_la64)"' --config 'target.$(TARGET_la64).rustflags=["-C","link-arg=-T$(CURDIR)/user/$(USER_LINKER_la64)"]'; \
	fi
	cp $(USER_ELF) $(USER_INSTALL)

user-rv64: ARCH=rv64
user-rv64: user

user-la64: ARCH=la64
user-la64: user

user-rv64-autotest:
	cd /tmp && RUSTUP_TOOLCHAIN=nightly-2025-06-01 CARGO_TARGET_DIR='$(CURDIR)/user/target' cargo build --manifest-path '$(CURDIR)/user/Cargo.toml' --release --target $(TARGET_rv64) --features autotest --config 'build.target="$(TARGET_rv64)"' --config 'target.$(TARGET_rv64).rustflags=["-C","link-arg=-T$(CURDIR)/user/$(USER_LINKER_rv64)"]'
	cp $(USER_ELF_rv64) $(USER_INSTALL_rv64)

kernel-rv64-test:
	cargo build --release -p kernel --target $(TARGET_rv64) --features $(_TEST_FEATURES)
	$(OBJCOPY) --binary-architecture=$(OBJCOPY_ARCH_rv64) $(KERNEL_ELF_rv64) --strip-all -O binary $(KERNEL_BIN_rv64)

# 编译带 autotest feature 的内核（自动运行测试脚本，完成后关机）
kernel-rv64-autotest:
	cargo build --release -p kernel --target $(TARGET_rv64) --features autotest,$(_LOG_LEVEL_FEATURE)
	$(OBJCOPY) --binary-architecture=$(OBJCOPY_ARCH_rv64) $(KERNEL_ELF_rv64) --strip-all -O binary $(KERNEL_BIN_rv64)


$(DISK_IMG_rv64): user-rv64 scripts/make_test_img.sh $(wildcard scripts/init)
	rm -f $(DISK_IMG_rv64)
	cd scripts && ./make_test_img.sh

disk-img: user-rv64
	rm -f $(DISK_IMG_rv64)
	cd scripts && ./make_test_img.sh

run: kernel $(RUN_DEPS)
	@echo "=== Running QEMU for ARCH=$(ARCH) (LOG=$(_LOG_FEATURES)) ==="
	@if [ "$(ARCH)" = "rv64" ]; then \
		uv run --with pexpect python3 scripts/test_runner.py --interactive $(QEMU) -- $(QEMU_FLAGS); \
	else \
		$(QEMU) $(QEMU_FLAGS); \
	fi

run-rv64: ARCH=rv64
run-rv64: kernel-rv64 $(DISK_IMG_rv64)
	@echo "=== Running QEMU for ARCH=rv64 (LOG=$(_LOG_FEATURES)) ==="
	uv run --with pexpect python3 scripts/test_runner.py --interactive $(QEMU_rv64) -- $(QEMU_rv64_FLAGS)

run-la64: ARCH=la64
run-la64: kernel-la64
	@echo "=== Running QEMU for ARCH=la64 (LOG=$(_LOG_FEATURES)) ==="
	$(QEMU_la64) $(QEMU_la64_FLAGS)

# 赛题标准评测：编译 autotest 内核 + 赛题磁盘镜像，直接用赛题 QEMU 参数运行
run-oscomp: kernel-rv
	@echo "=== OS COMP 评测模式 TEST_FS=$(TEST_FS) ==="
	@test -f $(TEST_FS) || (echo "ERROR: $(TEST_FS) 不存在，请先准备测试镜像" && exit 1)
	$(QEMU_rv64) -machine virt -nographic -bios default -kernel kernel-rv \
		-smp $(SMP) -m 128M -no-reboot \
		-drive file=$(TEST_FS),if=none,format=raw,id=x0 \
		-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
		-device virtio-net-device,netdev=net -netdev user,id=net \
		-rtc base=utc

OSCOMP_SRC := $(CURDIR)/scripts/oscomp
OSCOMP_TC := $(CURDIR)/testcase
OSCOMP_RUN := $(OSCOMP_SRC)/run-rv-oj.sh


# 便捷目标：一条命令完成构建镜像 + 运行
# make oscomp        → 全量测试
# make oscomp-basic  → 仅跑 basic 测试
# make oscomp-basic-all → 跑 basic-musl + basic-glibc
oscomp: OSCOMP_RUN=$(OSCOMP_SRC)/run-rv-oj.sh
oscomp: sdcard-rv run-oscomp

oscomp-basic: OSCOMP_RUN=$(OSCOMP_SRC)/run-rv-basic.sh
oscomp-basic: sdcard-rv run-oscomp

oscomp-basic-all: OSCOMP_RUN=$(OSCOMP_SRC)/run-rv-basic-all.sh
oscomp-basic-all: sdcard-rv run-oscomp


sdcard-rv: user-rv64-autotest
ifneq ($(UNAME_S),Linux)
	@echo "ERROR: sdcard-rv target requires Linux for ext4 filesystem operations"
	@echo "Please run this target on a Linux machine or use Docker with Linux container"
	@exit 1
endif
	@test -d $(OSCOMP_TC) || (echo "missing $(OSCOMP_TC)"; exit 1)
	@test -f $(OSCOMP_RUN) || (echo "missing $(OSCOMP_RUN)"; exit 1)
	@test -f $(OSCOMP_TC)/riscv/musl/busybox || (echo "missing $(OSCOMP_TC)/riscv/musl/busybox"; exit 1)
	rm -f scripts/sdcard-rv.img
	dd if=/dev/zero of=scripts/sdcard-rv.img bs=1M count=512
	mkfs.ext4 -F -O ^metadata_csum_seed scripts/sdcard-rv.img
	mkdir -p scripts/mnt
	sudo mount scripts/sdcard-rv.img scripts/mnt
	sudo cp -r $(OSCOMP_TC)/* scripts/mnt/
	sudo cp $(OSCOMP_RUN) scripts/mnt/riscv/run-oj.sh
	sudo find scripts/mnt -type f -name "*.sh" -exec chmod +x {} \;
	sudo chmod +x scripts/mnt/riscv/run-oj.sh
	sudo mkdir -p scripts/mnt/bin scripts/mnt/lib scripts/mnt/lib64 scripts/mnt/etc
	sudo cp $(OSCOMP_TC)/riscv/musl/busybox scripts/mnt/bin/busybox
	sudo chmod +x scripts/mnt/bin/busybox
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/sh
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/ls
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/cat
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/mkdir
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/rm
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/rmdir
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/mv
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/cp
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/touch
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/pwd
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/echo
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/ln
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/chmod
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/chown
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/uname
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/ps
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/kill
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/grep
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/find
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/head
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/tail
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/wc
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/sort
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/sed
	sudo ln -f scripts/mnt/bin/busybox scripts/mnt/bin/awk
	sudo cp scripts/initproc scripts/mnt/bin/initproc
	sudo chmod +x scripts/mnt/bin/initproc
	sudo umount scripts/mnt
	rmdir scripts/mnt
	@echo "=== scripts/sdcard-rv.img ready ==="


# GDB debug: halt on start, GDB server on port 1234
debug-rv64: kernel-rv64

debug: kernel
	$(QEMU) $(QEMU_FLAGS) -s -S

debug-rv64: ARCH=rv64
debug-rv64: kernel-rv64
	$(QEMU_rv64) $(QEMU_rv64_FLAGS) -s -S

debug-la64: ARCH=la64
debug-la64: kernel-la64
	$(QEMU_la64) $(QEMU_la64_FLAGS) -s -S

# GDB server without halt (attach to running kernel)
gdbserver-rv64: kernel-rv64

gdbserver: kernel
	$(QEMU) $(QEMU_FLAGS) -s

gdbserver-rv64: ARCH=rv64
gdbserver-rv64: kernel-rv64
	$(QEMU_rv64) $(QEMU_rv64_FLAGS) -s

gdbserver-la64: ARCH=la64
gdbserver-la64: kernel-la64
	$(QEMU_la64) $(QEMU_la64_FLAGS) -s

smoke: kernel user
	@echo "=== Smoke build complete for ARCH=$(ARCH) ==="

smoke-la64: ARCH=la64
smoke-la64: kernel-la64 user-la64
	@echo "=== Smoke build complete for ARCH=la64 ==="

# QEMU integration test: boot, capture output, check expected strings
# Kernel calls SBI shutdown after tests; fallback kill after 30s timeout.
QEMU_TEST_PATTERNS := \
	"hello from async future" \
	"woke after 100ms" \
	"hello from CPU" \
	"register clobber PASS" \
	"buddy initialized" \
	"pmap extract-only PASS" \
	"fixup bad-dst PASS" \
	"fixup bad-src PASS" \
	"uiomove short-read PASS" \
	"fork-exit-wait4 PASS" \
	"lwext4 mounted at /" \
	"exec OK" \
	"pmap satp-switch PASS" \
	"vm anonymous fault PASS" \
	"vm cow fault PASS" \
	"vm iterative drop 500 PASS" \
	"vm frame_alloc_sync PASS" \
	"vm fork bomb PASS" \
	"delegate read PASS" \
	"vfs read PASS" \
	"fork-exec-wait4 PASS" \
	"pipe data transfer PASS" \
	"signal pending delivery PASS" \
	"mmap munmap PASS" \
	"device nodes PASS" \
	"futex wake PASS"

qemu-test-rv64: kernel-rv64-test $(DISK_IMG_rv64)
	@echo "=== QEMU integration test (SMP=$(SMP)) ==="
	@TMPOUT=$$(mktemp); \
	$(QEMU_rv64) $(QEMU_rv64_FLAGS) > $$TMPOUT 2>&1 & \
	QPID=$$!; \
	( sleep 30; kill $$QPID 2>/dev/null ) & WATCHDOG=$$!; \
	wait $$QPID 2>/dev/null; \
	kill $$WATCHDOG 2>/dev/null; wait $$WATCHDOG 2>/dev/null; \
	PASS=0; FAIL=0; \
	for pat in $(QEMU_TEST_PATTERNS); do \
		if grep -q "$$pat" $$TMPOUT; then \
			echo "  PASS: $$pat"; \
			PASS=$$((PASS + 1)); \
		else \
			echo "  FAIL: $$pat"; \
			FAIL=$$((FAIL + 1)); \
		fi; \
	done; \
	rm -f $$TMPOUT; \
	echo "=== $$PASS passed, $$FAIL failed ==="; \
	[ $$FAIL -eq 0 ]

# Quick agent-driven smoke test: build + boot + check core patterns (no qemu-test feature)
AGENT_TEST_TIMEOUT ?= 15
AGENT_TEST_PATTERNS := \
	"lwext4 mounted at /" \
	"delegate running" \
	"exec OK"

agent-test: kernel-rv64 $(DISK_IMG_rv64)
	@echo "=== Agent smoke test (timeout=$(AGENT_TEST_TIMEOUT)s) ==="
	@TMPOUT=$$(mktemp); \
	$(QEMU_rv64) $(QEMU_rv64_FLAGS) > $$TMPOUT 2>&1 & \
	QPID=$$!; \
	( sleep $(AGENT_TEST_TIMEOUT); kill $$QPID 2>/dev/null ) & WATCHDOG=$$!; \
	wait $$QPID 2>/dev/null; \
	kill $$WATCHDOG 2>/dev/null; wait $$WATCHDOG 2>/dev/null; \
	PASS=0; FAIL=0; \
	for pat in $(AGENT_TEST_PATTERNS); do \
		if grep -q "$$pat" $$TMPOUT; then \
			echo "  PASS: $$pat"; \
			PASS=$$((PASS + 1)); \
		else \
			echo "  FAIL: $$pat"; \
			FAIL=$$((FAIL + 1)); \
		fi; \
	done; \
	echo "--- last 20 lines ---"; \
	tail -20 $$TMPOUT; \
	rm -f $$TMPOUT; \
	echo "=== $$PASS passed, $$FAIL failed ==="; \
	[ $$FAIL -eq 0 ]

test:
	@echo "=== No host unit tests (kernel is binary-only) ==="
	@echo "Run 'make qemu-test-rv64' for integration tests"

test-all: test qemu-test-rv64 python-test-rv64

# QEMU python interactive test
python-test-rv64: kernel-rv64 $(DISK_IMG_rv64)
	@echo "=== QEMU Python Integration Test (SMP=$(SMP), LOG=$(_LOG_FEATURES)) ==="
	uv run --with pexpect python3 scripts/test_runner.py $(QEMU_rv64) -- $(QEMU_rv64_FLAGS)

clean:
	cargo clean
	rm -f $(KERNEL_BIN_rv64) $(KERNEL_BIN_la64) $(USER_INSTALL_la64)

# Create zig-based riscv64-linux-musl-cc/ar wrappers (matches CI).
# Replaces any existing wrappers that may use newlib headers (which break lwext4).
# Requires: zig (brew install zig)
# Note: la64 bring-up needs analogous `loongarch64-linux-musl-cc/ar` wrappers or
# a real loongarch64 musl cross toolchain in PATH when building ext4 dependencies.
WRAPPER_DIR ?= $(HOME)/.local/bin
setup-toolchain:
	@command -v zig >/dev/null 2>&1 || (echo "ERROR: zig not found. Install with: brew install zig"; exit 1)
	@mkdir -p $(WRAPPER_DIR)
	@rm -f $(WRAPPER_DIR)/riscv64-linux-musl-cc
	@printf '#!/bin/sh\nexec zig cc -target riscv64-linux-musl "$$@"\n' > $(WRAPPER_DIR)/riscv64-linux-musl-cc
	@chmod +x $(WRAPPER_DIR)/riscv64-linux-musl-cc
	@rm -f $(WRAPPER_DIR)/riscv64-linux-musl-ar
	@printf '#!/bin/sh\nexec zig ar "$$@"\n' > $(WRAPPER_DIR)/riscv64-linux-musl-ar
	@chmod +x $(WRAPPER_DIR)/riscv64-linux-musl-ar
	@echo "Installed zig-based wrappers in $(WRAPPER_DIR):"
	@echo "  riscv64-linux-musl-cc -> zig cc -target riscv64-linux-musl"
	@echo "  riscv64-linux-musl-ar -> zig ar"
