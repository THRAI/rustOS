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

OBJCOPY := rust-objcopy

.PHONY: kernel-rv64 kernel-rv64-test run-rv64 debug-rv64 gdbserver-rv64 qemu-test-rv64 test test-all clean

kernel-rv64:
	cargo build --release -p kernel --target $(TARGET_RV64)
	$(OBJCOPY) --binary-architecture=riscv64 $(KERNEL_ELF_RV64) --strip-all -O binary $(KERNEL_BIN_RV64)

kernel-rv64-test:
	cargo build --release -p kernel --target $(TARGET_RV64) --features qemu-test
	$(OBJCOPY) --binary-architecture=riscv64 $(KERNEL_ELF_RV64) --strip-all -O binary $(KERNEL_BIN_RV64)

$(DISK_IMG):
	cd scripts && ./make_test_img.sh

run-rv64: kernel-rv64 $(DISK_IMG)
	$(QEMU_RV64) $(QEMU_RV64_FLAGS)

# GDB debug: halt on start, GDB server on port 1234
debug-rv64: kernel-rv64
	$(QEMU_RV64) $(QEMU_RV64_FLAGS) -s -S

# GDB server without halt (attach to running kernel)
gdbserver-rv64: kernel-rv64
	$(QEMU_RV64) $(QEMU_RV64_FLAGS) -s

# QEMU integration test: boot, capture output, check expected strings
# Kernel calls SBI shutdown after tests; fallback kill after 30s timeout.
qemu-test-rv64: kernel-rv64-test $(DISK_IMG)
	@echo "=== QEMU integration test (SMP=$(SMP)) ==="
	@TMPOUT=$$(mktemp); \
	$(QEMU_RV64) $(QEMU_RV64_FLAGS) > $$TMPOUT 2>&1 & \
	QPID=$$!; \
	( sleep 30; kill $$QPID 2>/dev/null ) & WATCHDOG=$$!; \
	wait $$QPID 2>/dev/null; \
	kill $$WATCHDOG 2>/dev/null; wait $$WATCHDOG 2>/dev/null; \
	PASS=0; FAIL=0; \
	for pat in "hello from async future" "woke after 100ms" "hello from CPU" "register clobber PASS" "buddy initialized" "pmap extract-only PASS" "pmap satp-switch PASS" "vm anonymous fault PASS" "vm cow fault PASS" "vm iterative drop 500 PASS" "vm frame_alloc_sync PASS" "fixup bad-dst PASS" "fixup bad-src PASS" "uiomove short-read PASS" "fork-exit-wait4 PASS" "delegate read PASS" "vfs read PASS" "fork-exec-wait4 PASS"; do \
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

test:
	cargo test --lib -p hal-common --target $(HOST_TARGET)
	cargo test --lib -p kernel-mm --target $(HOST_TARGET)

test-all: test qemu-test-rv64

clean:
	cargo clean
	rm -f $(KERNEL_BIN_RV64)
