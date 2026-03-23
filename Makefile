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
KERNEL_TARGET_DIR ?= $(CURDIR)/.target-kernel
KERNEL_TMPDIR ?= $(CURDIR)/.tmp/rustc
KERNEL_ELF_rv64 := $(KERNEL_TARGET_DIR)/$(TARGET_rv64)/release/kernel
KERNEL_ELF_la64 := $(KERNEL_TARGET_DIR)/$(TARGET_la64)/release/kernel
USER_ELF_rv64 := user/target/$(TARGET_rv64)/release/initproc
USER_ELF_la64 := user/target/$(TARGET_la64)/release/initproc
KERNEL_BIN_rv64 := kernel-rv64.bin
KERNEL_BIN_la64 := kernel-la64.bin
USER_INSTALL_rv64 := scripts/initproc
USER_INSTALL_la64 := scripts/initproc-la64
USER_INSTALL_la64_AUTOTEST := scripts/initproc-la64-autotest
USER_LINKER_rv64 := src/linker.ld
USER_LINKER_la64 := src/linker-la64.ld
USER_RUSTFLAGS_rv64 := -Clink-arg=-Tsrc/linker.ld
USER_RUSTFLAGS_la64 := -Clink-arg=-Tsrc/linker-la64.ld -Clink-arg=-z -Clink-arg=max-page-size=0x1000
KERNEL_LINKER_rv64 := kernel/linker/rv64-qemu.ld
KERNEL_LINKER_la64 := kernel/linker/la64-qemu.ld
KERNEL_RUSTFLAGS_rv64 := ["-C","link-arg=-T$(CURDIR)/kernel/linker/rv64-qemu.ld"]
KERNEL_RUSTFLAGS_la64 := ["-C","link-arg=-T$(CURDIR)/kernel/linker/la64-qemu.ld","-C","target-feature=-lsx,-lasx"]

QEMU_rv64 := qemu-system-riscv64
QEMU_la64 := qemu-system-loongarch64
SMP ?= 4
QEMU_TRACE ?=
DISK_IMG_rv64 := scripts/test.img
DISK_IMG_la64 := scripts/test-la64.img
DISK_IMG_la64_AUTOTEST := scripts/test-la64-autotest.img
LA64_AUTOTEST_GROUPS ?= basic
LA64_AUTOTEST_LIBC ?= musl
OBJCOPY_ARCH_rv64 := riscv64
OBJCOPY_ARCH_la64 := loongarch64
QEMU_rv64_FLAGS := -machine virt -nographic -bios default -kernel $(KERNEL_BIN_rv64) -smp $(SMP) -m 128M \
	-drive file=$(DISK_IMG_rv64),format=raw,if=none,id=hd0 -device virtio-blk-device,drive=hd0 $(QEMU_TRACE)
# LoongArch QEMU virt exposes block devices through PCI by default.
# Use the modern non-transitional device so la64 bring-up tracks the
# capability-based transport we plan to implement next.
QEMU_la64_FLAGS := -machine virt -cpu la464 -nographic -kernel $(KERNEL_ELF_la64) -smp $(SMP) -m 1G \
	-drive file=$(DISK_IMG_la64),format=raw,if=none,id=hd0 -device virtio-blk-pci-non-transitional,drive=hd0,rombar=0 $(QEMU_TRACE)

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
KERNEL_FEATURE_ARGS_rv64 :=
KERNEL_FEATURE_ARGS_la64 := --no-default-features --features la64-bringup
KERNEL_FEATURE_ARGS = $(KERNEL_FEATURE_ARGS_$(ARCH))

RUN_DEPS_rv64 := $(DISK_IMG_rv64)
RUN_DEPS_la64 := $(DISK_IMG_la64)
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

.PHONY: all kernel kernel-rv kernel-rv64 kernel-la64 kernel-la64-bringup kernel-la64-full kernel-la64-autotest kernel-rv64-test kernel-rv64-autotest user user-rv64 user-la64 user-rv64-autotest user-la64-autotest run-rv64 run-la64 run-la64-full run-la64-autotest run-la64-kernel-only smoke-la64-boot smoke-la64-full check-la64-full-toolchain run-oscomp sdcard-rv oscomp oscomp-basic oscomp-basic-all oscomp-run debug-rv64 debug-la64 gdbserver-rv64 gdbserver-la64 smoke smoke-la64 qemu-test-rv64 agent-test test test-all disk-img setup-toolchain clean docker-build docker-kernel-rv64 docker-run-rv64 docker-kernel-la64 docker-kernel-la64-full docker-kernel-la64-autotest docker-user-la64 docker-user-la64-autotest docker-run-la64 docker-run-la64-full docker-run-la64-autotest docker-debug-la64 docker-gdbserver-la64 docker-smoke-la64 docker-smoke-la64-full docker-agent-test docker-python-test docker-oscomp docker-oscomp-basic docker-oscomp-basic-all docker-oscomp-run docker-judge docker-shell docker-test-all docker-ltp-build

# 赛题评测入口：make all 产出 ELF 格式的 kernel-rv（autotest 模式，自动跑测试脚本后关机）
all: kernel-rv

kernel-rv:
	mkdir -p $(KERNEL_TMPDIR)
	TMPDIR="$(KERNEL_TMPDIR)" CARGO_TARGET_DIR="$(KERNEL_TARGET_DIR)" cargo build --release -p kernel --target $(TARGET_rv64) --features autotest,$(_LOG_LEVEL_FEATURE)
	cp $(KERNEL_ELF_rv64) kernel-rv

kernel:
	mkdir -p $(KERNEL_TMPDIR)
	PATH="$(WRAPPER_DIR):$$PATH" TMPDIR="$(KERNEL_TMPDIR)" CARGO_TARGET_DIR="$(KERNEL_TARGET_DIR)" cargo build --release -p kernel --target $(TARGET) $(KERNEL_FEATURE_ARGS) $(_CARGO_LOG) --config 'target.$(TARGET).rustflags=$(KERNEL_RUSTFLAGS)'
	$(OBJCOPY) --binary-architecture=$(OBJCOPY_ARCH) $(KERNEL_ELF) --strip-all -O binary $(KERNEL_BIN)

kernel-rv64: ARCH=rv64
kernel-rv64: kernel

kernel-la64: ARCH=la64
kernel-la64: kernel

kernel-la64-bringup: ARCH=la64
kernel-la64-bringup: kernel

check-la64-full-toolchain:
	@command -v loongarch64-linux-musl-cc >/dev/null 2>&1 || (echo "ERROR: loongarch64-linux-musl-cc not found in PATH. Use the Docker path ('make docker-kernel-la64-full' or 'make docker-smoke-la64-full'), or run 'make setup-toolchain' after installing zig."; exit 1)
	@command -v loongarch64-linux-musl-ar >/dev/null 2>&1 || (echo "ERROR: loongarch64-linux-musl-ar not found in PATH. Use the Docker path ('make docker-kernel-la64-full' or 'make docker-smoke-la64-full'), or run 'make setup-toolchain' after installing zig."; exit 1)

kernel-la64-full: check-la64-full-toolchain
	mkdir -p $(KERNEL_TMPDIR)
	GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*' PATH="$(CURDIR)/scripts/toolchain-wrappers:$(WRAPPER_DIR):$$PATH" TMPDIR="$(KERNEL_TMPDIR)" CARGO_TARGET_DIR="$(KERNEL_TARGET_DIR)" cargo build --release -p kernel --target $(TARGET_la64) --features la64-bringup $(_CARGO_LOG) --config 'target.$(TARGET_la64).rustflags=$(KERNEL_RUSTFLAGS_la64)'
	$(OBJCOPY) --binary-architecture=$(OBJCOPY_ARCH_la64) $(KERNEL_ELF_la64) --strip-all -O binary $(KERNEL_BIN_la64)

kernel-la64-autotest: check-la64-full-toolchain
	mkdir -p $(KERNEL_TMPDIR)
	GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0='*' PATH="$(CURDIR)/scripts/toolchain-wrappers:$(WRAPPER_DIR):$$PATH" TMPDIR="$(KERNEL_TMPDIR)" CARGO_TARGET_DIR="$(KERNEL_TARGET_DIR)" cargo build --release -p kernel --target $(TARGET_la64) --features la64-bringup,autotest $(_CARGO_LOG) --config 'target.$(TARGET_la64).rustflags=$(KERNEL_RUSTFLAGS_la64)'
	$(OBJCOPY) --binary-architecture=$(OBJCOPY_ARCH_la64) $(KERNEL_ELF_la64) --strip-all -O binary $(KERNEL_BIN_la64)

user:
	@if [ "$(ARCH)" = "rv64" ]; then \
		cd /tmp && PATH='$(WRAPPER_DIR):'$$PATH RUSTUP_TOOLCHAIN=nightly-2025-06-01 CARGO_TARGET_DIR='$(CURDIR)/user/target' cargo build --manifest-path '$(CURDIR)/user/Cargo.toml' --release --target $(TARGET_rv64) --config 'build.target="$(TARGET_rv64)"' --config 'target.$(TARGET_rv64).rustflags=["-C","link-arg=-T$(CURDIR)/user/$(USER_LINKER_rv64)"]'; \
	else \
		rm -f '$(CURDIR)/$(USER_ELF_la64)' '$(CURDIR)/$(USER_INSTALL_la64)'; \
		cd /tmp && PATH='$(WRAPPER_DIR):'$$PATH RUSTUP_TOOLCHAIN=nightly-2025-06-01 CARGO_TARGET_DIR='$(CURDIR)/user/target' cargo build --manifest-path '$(CURDIR)/user/Cargo.toml' --release --target $(TARGET_la64) --config 'build.target="$(TARGET_la64)"' --config 'target.$(TARGET_la64).rustflags=["-C","link-arg=-T$(CURDIR)/user/$(USER_LINKER_la64)","-C","link-arg=-z","-C","link-arg=max-page-size=0x1000"]'; \
	fi
	cp $(USER_ELF) $(USER_INSTALL)

user-rv64: ARCH=rv64
user-rv64: user

user-la64: ARCH=la64
user-la64: user

user-rv64-autotest:
	cd /tmp && RUSTUP_TOOLCHAIN=nightly-2025-06-01 CARGO_TARGET_DIR='$(CURDIR)/user/target' cargo build --manifest-path '$(CURDIR)/user/Cargo.toml' --release --target $(TARGET_rv64) --features autotest --config 'build.target="$(TARGET_rv64)"' --config 'target.$(TARGET_rv64).rustflags=["-C","link-arg=-T$(CURDIR)/user/$(USER_LINKER_rv64)"]'
	cp $(USER_ELF_rv64) $(USER_INSTALL_rv64)

user-la64-autotest:
	rm -f '$(CURDIR)/$(USER_ELF_la64)' '$(CURDIR)/$(USER_INSTALL_la64_AUTOTEST)'
	cd /tmp && PATH='$(WRAPPER_DIR):'$$PATH RUSTUP_TOOLCHAIN=nightly-2025-06-01 CARGO_TARGET_DIR='$(CURDIR)/user/target' cargo build --manifest-path '$(CURDIR)/user/Cargo.toml' --release --target $(TARGET_la64) --features autotest --config 'build.target="$(TARGET_la64)"' --config 'target.$(TARGET_la64).rustflags=["-C","link-arg=-T$(CURDIR)/user/$(USER_LINKER_la64)","-C","link-arg=-z","-C","link-arg=max-page-size=0x1000"]'
	cp $(USER_ELF_la64) $(USER_INSTALL_la64_AUTOTEST)

kernel-rv64-test:
	mkdir -p $(KERNEL_TMPDIR)
	TMPDIR="$(KERNEL_TMPDIR)" CARGO_TARGET_DIR="$(KERNEL_TARGET_DIR)" cargo build --release -p kernel --target $(TARGET_rv64) --features $(_TEST_FEATURES)
	$(OBJCOPY) --binary-architecture=$(OBJCOPY_ARCH_rv64) $(KERNEL_ELF_rv64) --strip-all -O binary $(KERNEL_BIN_rv64)

# 编译带 autotest feature 的内核（自动运行测试脚本，完成后关机）
kernel-rv64-autotest:
	mkdir -p $(KERNEL_TMPDIR)
	TMPDIR="$(KERNEL_TMPDIR)" CARGO_TARGET_DIR="$(KERNEL_TARGET_DIR)" cargo build --release -p kernel --target $(TARGET_rv64) --features autotest,$(_LOG_LEVEL_FEATURE)
	$(OBJCOPY) --binary-architecture=$(OBJCOPY_ARCH_rv64) $(KERNEL_ELF_rv64) --strip-all -O binary $(KERNEL_BIN_rv64)


$(DISK_IMG_rv64): user-rv64 scripts/make_test_img.sh $(wildcard scripts/init scripts/init-la64 scripts/busybox scripts/busybox-la64)
	rm -f $(DISK_IMG_rv64)
	cd scripts && ./make_test_img.sh

$(DISK_IMG_la64): user-la64 scripts/make_test_img.sh $(wildcard scripts/init scripts/init-la64 scripts/busybox scripts/busybox-la64)
	rm -f $(DISK_IMG_la64)
	cd scripts && RUSTOS_TEST_IMG="$(notdir $(DISK_IMG_la64))" RUSTOS_TEST_INIT="init-la64" RUSTOS_TEST_BUSYBOX="busybox-la64" RUSTOS_TEST_INITPROC="$(notdir $(USER_INSTALL_la64))" ./make_test_img.sh

$(DISK_IMG_la64_AUTOTEST): user-la64-autotest scripts/make_test_img.sh scripts/oscomp/run-la-select.sh
	rm -f $(DISK_IMG_la64_AUTOTEST)
	cd scripts && RUSTOS_TEST_IMG="$(notdir $(DISK_IMG_la64_AUTOTEST))" RUSTOS_TEST_INIT="init-la64" RUSTOS_TEST_BUSYBOX="busybox-la64" RUSTOS_TEST_INITPROC="$(notdir $(USER_INSTALL_la64_AUTOTEST))" RUSTOS_TEST_AUTOTEST_ARCH="loongarch" RUSTOS_TEST_AUTOTEST_LIBC="$(LA64_AUTOTEST_LIBC)" RUSTOS_TEST_AUTOTEST_GROUPS="$(LA64_AUTOTEST_GROUPS)" ./make_test_img.sh

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
run-la64: kernel-la64 $(DISK_IMG_la64)
	@echo "=== Running QEMU for ARCH=la64 (LOG=$(_LOG_FEATURES)) ==="
	$(QEMU_la64) $(QEMU_la64_FLAGS)

run-la64-full: ARCH=la64
run-la64-full: kernel-la64-full $(DISK_IMG_la64)
	@echo "=== Running QEMU for ARCH=la64 full-fs (LOG=$(_LOG_FEATURES)) ==="
	$(QEMU_la64) $(QEMU_la64_FLAGS)

run-la64-autotest: kernel-la64-autotest $(DISK_IMG_la64_AUTOTEST)
	@echo "=== Running QEMU for ARCH=la64 autotest (groups=$(LA64_AUTOTEST_GROUPS) libc=$(LA64_AUTOTEST_LIBC), LOG=$(_LOG_FEATURES)) ==="
	$(QEMU_la64) -machine virt -cpu la464 -nographic -kernel $(KERNEL_ELF_la64) -smp $(SMP) -m 1G \
		-drive file=$(DISK_IMG_la64_AUTOTEST),format=raw,if=none,id=hd0 -device virtio-blk-pci-non-transitional,drive=hd0,rombar=0 $(QEMU_TRACE)

run-la64-kernel-only: ARCH=la64
run-la64-kernel-only: run-la64

smoke-la64-boot: ARCH=la64
smoke-la64-boot: kernel-la64
	bash scripts/la64-smoke.sh

smoke-la64-full: ARCH=la64
smoke-la64-full: check-la64-full-toolchain
	bash scripts/la64-full-smoke.sh

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
OSCOMP_RUN_SCRIPT ?= run-rv-basic.sh
OSCOMP_GROUPS ?=
OSCOMP_LIBC ?= musl


# 便捷目标：一条命令完成构建镜像 + 运行
# make oscomp        → 全量测试
# make oscomp-basic  → 仅跑 basic 测试
# make oscomp-basic-all → 跑 basic-musl + basic-glibc
# make oscomp-run GROUPS=busybox,lua  → 选定测试组
# make oscomp-run GROUPS=basic,busybox LIBC=all → 选定测试组 + 两种 libc
oscomp: OSCOMP_RUN_SCRIPT=run-rv-oj.sh
oscomp: sdcard-rv run-oscomp

oscomp-basic: OSCOMP_RUN_SCRIPT=run-rv-basic.sh
oscomp-basic: sdcard-rv run-oscomp

oscomp-basic-all: OSCOMP_RUN_SCRIPT=run-rv-basic-all.sh
oscomp-basic-all: sdcard-rv run-oscomp

# Parameterized: run selected test groups
# Usage: make oscomp-run GROUPS=busybox,lua LIBC=musl
oscomp-run: OSCOMP_GROUPS=$(GROUPS)
oscomp-run: OSCOMP_LIBC=$(LIBC)
oscomp-run: sdcard-rv run-oscomp


# Cross-platform sdcard image builder (no mount, no sudo, no Linux required).
# Uses mke2fs -d to populate the ext4 image from a staging directory.
sdcard-rv: user-rv64-autotest
	@test -d $(OSCOMP_TC)/riscv || (echo "ERROR: testcase/riscv/ not found"; exit 1)
	bash scripts/make_sdcard_img.sh $(OSCOMP_RUN_SCRIPT) "$(OSCOMP_GROUPS)" "$(OSCOMP_LIBC)"


# ---- Docker targets (cross-platform test runner) ----

DOCKER_COMPOSE ?= docker compose
DOCKER_SERVICE ?= oscomp
DOCKER_RUN = $(DOCKER_COMPOSE) run --rm $(DOCKER_SERVICE)

docker-build:
	$(DOCKER_COMPOSE) build $(DOCKER_SERVICE)

docker-kernel-rv64:
	$(DOCKER_RUN) make kernel-rv64

docker-run-rv64:
	$(DOCKER_RUN) make run-rv64

docker-kernel-la64:
	$(DOCKER_RUN) make kernel-la64

docker-kernel-la64-full:
	$(DOCKER_RUN) make kernel-la64-full

docker-kernel-la64-autotest:
	$(DOCKER_RUN) make kernel-la64-autotest

docker-user-la64:
	$(DOCKER_RUN) make user-la64

docker-user-la64-autotest:
	$(DOCKER_RUN) make user-la64-autotest

docker-run-la64:
	$(DOCKER_RUN) make run-la64

docker-run-la64-full:
	$(DOCKER_RUN) make run-la64-full

docker-run-la64-autotest:
	$(DOCKER_RUN) make run-la64-autotest LA64_AUTOTEST_GROUPS="$(LA64_AUTOTEST_GROUPS)" LA64_AUTOTEST_LIBC="$(LA64_AUTOTEST_LIBC)"

docker-debug-la64:
	$(DOCKER_RUN) make debug-la64

docker-gdbserver-la64:
	$(DOCKER_RUN) make gdbserver-la64

docker-smoke-la64:
	$(DOCKER_RUN) make smoke-la64

docker-smoke-la64-full:
	$(DOCKER_RUN) make smoke-la64-full

docker-agent-test:
	$(DOCKER_RUN) make agent-test

docker-python-test:
	$(DOCKER_RUN) make python-test-rv64

docker-oscomp:
	$(DOCKER_RUN) make oscomp

docker-oscomp-basic:
	$(DOCKER_RUN) make oscomp-basic

docker-oscomp-basic-all:
	$(DOCKER_RUN) make oscomp-basic-all

# Run selected test groups in Docker
# Usage: make docker-oscomp-run GROUPS=busybox,lua LIBC=musl
#        make docker-oscomp-run GROUPS=basic,busybox,lua LIBC=all
docker-oscomp-run:
	$(DOCKER_RUN) make oscomp-run GROUPS="$(GROUPS)" LIBC="$(LIBC)"

docker-judge:
	$(DOCKER_RUN) bash -c 'make oscomp-basic-all 2>&1 | tee /tmp/oscomp.log && ./judge/batch_judge.sh --log /tmp/oscomp.log'

docker-shell:
	$(DOCKER_RUN) bash

docker-test-all: docker-agent-test docker-python-test docker-oscomp-basic-all

# Dedicated LTP build (opt-in, ~10 min, requires competition Docker image)
docker-ltp-build:
	$(DOCKER_COMPOSE) build ltp
	$(DOCKER_COMPOSE) run --rm ltp
	@echo "LTP binaries now in testcase/riscv/{musl,glibc}/ltp/"


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

# Create zig-based musl cross-compiler wrappers (matches CI).
# Replaces any existing wrappers that may use newlib headers (which break lwext4).
# Requires: zig in PATH.
WRAPPER_DIR ?= $(HOME)/.local/bin
setup-toolchain:
	@command -v zig >/dev/null 2>&1 || (echo "ERROR: zig not found. Install zig first, then rerun 'make setup-toolchain'."; exit 1)
	@mkdir -p $(WRAPPER_DIR)
	@rm -f $(WRAPPER_DIR)/riscv64-linux-musl-cc
	@printf '#!/bin/sh\nexec zig cc -target riscv64-linux-musl "$$@"\n' > $(WRAPPER_DIR)/riscv64-linux-musl-cc
	@chmod +x $(WRAPPER_DIR)/riscv64-linux-musl-cc
	@rm -f $(WRAPPER_DIR)/riscv64-linux-musl-ar
	@printf '#!/bin/sh\nexec zig ar "$$@"\n' > $(WRAPPER_DIR)/riscv64-linux-musl-ar
	@chmod +x $(WRAPPER_DIR)/riscv64-linux-musl-ar
	@rm -f $(WRAPPER_DIR)/loongarch64-linux-musl-cc
	@printf '#!/bin/sh\nexec zig cc -target loongarch64-linux-musl "$$@"\n' > $(WRAPPER_DIR)/loongarch64-linux-musl-cc
	@chmod +x $(WRAPPER_DIR)/loongarch64-linux-musl-cc
	@rm -f $(WRAPPER_DIR)/loongarch64-linux-musl-ar
	@printf '#!/bin/sh\nexec zig ar "$$@"\n' > $(WRAPPER_DIR)/loongarch64-linux-musl-ar
	@chmod +x $(WRAPPER_DIR)/loongarch64-linux-musl-ar
	@echo "Installed zig-based wrappers in $(WRAPPER_DIR):"
	@echo "  riscv64-linux-musl-cc -> zig cc -target riscv64-linux-musl"
	@echo "  riscv64-linux-musl-ar -> zig ar"
	@echo "  loongarch64-linux-musl-cc -> zig cc -target loongarch64-linux-musl"
	@echo "  loongarch64-linux-musl-ar -> zig ar"
