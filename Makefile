# BSD-Async Rust OS Kernel -- Top-level Makefile

HOST_TARGET := $(shell rustc -vV | grep host | cut -d' ' -f2)
TARGET_RV64 := riscv64gc-unknown-none-elf
KERNEL_ELF_RV64 := target/$(TARGET_RV64)/release/kernel
KERNEL_BIN_RV64 := kernel-rv64.bin

QEMU_RV64 := qemu-system-riscv64
SMP ?= 4
QEMU_TRACE ?=
DISK_IMG := scripts/test.img
QEMU_RV64_FLAGS := -machine virt -nographic -bios default -kernel $(KERNEL_BIN_RV64) -smp $(SMP) -m 128M \
	-drive file=$(DISK_IMG),format=raw,if=none,id=hd0 -device virtio-blk-device,drive=hd0 $(QEMU_TRACE)

# ---- 赛题评测标准参数 ----
# TEST_FS: 含测试程序的 ext4 磁盘镜像（sdcard-rv.img 或赛平台注入的 fs 镜像）
TEST_FS ?= scripts/sdcard-rv.img
# OSCOMP_FLAGS 严格对齐赛题 README 的 QEMU 启动命令
OSCOMP_FLAGS := -machine virt -nographic -bios default -kernel $(KERNEL_BIN_RV64) \
	-smp $(SMP) -m 128M -no-reboot \
	-drive file=$(TEST_FS),if=none,format=raw,id=x0 \
	-device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
	-device virtio-net-device,netdev=net -netdev user,id=net \
	-rtc base=utc

OBJCOPY := rust-objcopy

# Kernel log control: LOG=all | LOG=boot,fs,driver | (empty = quiet)
# Available modules: boot syscall trap vm sched fs driver smp signal pipe exec proc
comma := ,
space := $(subst ,, )
LOG ?=
LEVEL ?= trace
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

.PHONY: all kernel-rv kernel-rv64 kernel-rv64-test kernel-rv64-autotest run-rv64 run-oscomp sdcard-rv oscomp oscomp-basic debug-rv64 gdbserver-rv64 qemu-test-rv64 agent-test test test-all disk-img clean

# 赛题评测入口：make all 产出 ELF 格式的 kernel-rv（autotest 模式，自动跑测试脚本后关机）
all: kernel-rv

kernel-rv:
	cargo build --release -p kernel --target $(TARGET_RV64) --features autotest,$(_LOG_LEVEL_FEATURE)
	cp $(KERNEL_ELF_RV64) kernel-rv

kernel-rv64:
	cargo build --release -p kernel --target $(TARGET_RV64) $(_CARGO_LOG)
	$(OBJCOPY) --binary-architecture=riscv64 $(KERNEL_ELF_RV64) --strip-all -O binary $(KERNEL_BIN_RV64)

kernel-rv64-test:
	cargo build --release -p kernel --target $(TARGET_RV64) --features $(_TEST_FEATURES)
	$(OBJCOPY) --binary-architecture=riscv64 $(KERNEL_ELF_RV64) --strip-all -O binary $(KERNEL_BIN_RV64)

# 编译带 autotest feature 的内核（自动运行测试脚本，完成后关机）
kernel-rv64-autotest:
	cargo build --release -p kernel --target $(TARGET_RV64) --features autotest,$(_LOG_LEVEL_FEATURE)
	$(OBJCOPY) --binary-architecture=riscv64 $(KERNEL_ELF_RV64) --strip-all -O binary $(KERNEL_BIN_RV64)

$(DISK_IMG): scripts/make_test_img.sh $(wildcard scripts/init)
	rm -f $(DISK_IMG)
	cd scripts && ./make_test_img.sh

disk-img:
	rm -f $(DISK_IMG)
	cd scripts && ./make_test_img.sh

run-rv64: kernel-rv64 $(DISK_IMG)
	@echo "=== Running QEMU Interactively (LOG=$(_LOG_FEATURES)) ==="
	uv run --with pexpect python3 scripts/test_runner.py --interactive $(QEMU_RV64) -- $(QEMU_RV64_FLAGS)

# 赛题标准评测：编译 autotest 内核 + 赛题磁盘镜像，直接用赛题 QEMU 参数运行
run-oscomp: kernel-rv
	@echo "=== OS COMP 评测模式 TEST_FS=$(TEST_FS) ==="
	@test -f $(TEST_FS) || (echo "ERROR: $(TEST_FS) 不存在，请先准备测试镜像" && exit 1)
	$(QEMU_RV64) -machine virt -nographic -bios default -kernel kernel-rv \
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
oscomp: OSCOMP_RUN=$(OSCOMP_SRC)/run-rv-oj.sh
oscomp: sdcard-rv run-oscomp

oscomp-basic: OSCOMP_RUN=$(OSCOMP_SRC)/run-rv-basic.sh
oscomp-basic: sdcard-rv run-oscomp


sdcard-rv:
	@test -d $(OSCOMP_TC) || (echo "missing $(OSCOMP_TC)"; exit 1)
	@test -f $(OSCOMP_RUN) || (echo "missing $(OSCOMP_RUN)"; exit 1)
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
	sudo umount scripts/mnt
	rmdir scripts/mnt
	@echo "=== scripts/sdcard-rv.img ready ==="


# GDB debug: halt on start, GDB server on port 1234
debug-rv64: kernel-rv64
	$(QEMU_RV64) $(QEMU_RV64_FLAGS) -s -S

# GDB server without halt (attach to running kernel)
gdbserver-rv64: kernel-rv64
	$(QEMU_RV64) $(QEMU_RV64_FLAGS) -s

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

qemu-test-rv64: kernel-rv64-test $(DISK_IMG)
	@echo "=== QEMU integration test (SMP=$(SMP)) ==="
	@TMPOUT=$$(mktemp); \
	$(QEMU_RV64) $(QEMU_RV64_FLAGS) > $$TMPOUT 2>&1 & \
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

agent-test: kernel-rv64 $(DISK_IMG)
	@echo "=== Agent smoke test (timeout=$(AGENT_TEST_TIMEOUT)s) ==="
	@TMPOUT=$$(mktemp); \
	timeout $(AGENT_TEST_TIMEOUT) $(QEMU_RV64) $(QEMU_RV64_FLAGS) > $$TMPOUT 2>&1 || true; \
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
	cargo test --lib -p hal-common --target $(HOST_TARGET)
	cargo test --lib -p kernel-mm --target $(HOST_TARGET)

test-all: test qemu-test-rv64 python-test-rv64

# QEMU python interactive test
python-test-rv64: kernel-rv64 $(DISK_IMG)
	@echo "=== QEMU Python Integration Test (SMP=$(SMP), LOG=$(_LOG_FEATURES)) ==="
	uv run --with pexpect python3 scripts/test_runner.py $(QEMU_RV64) -- $(QEMU_RV64_FLAGS)

clean:
	cargo clean
	rm -f $(KERNEL_BIN_RV64)
