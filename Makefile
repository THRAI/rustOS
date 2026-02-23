# BSD-Async Rust OS Kernel -- Top-level Makefile

HOST_TARGET := $(shell rustc -vV | grep host | cut -d' ' -f2)
TARGET_RV64 := riscv64gc-unknown-none-elf
KERNEL_ELF_RV64 := target/$(TARGET_RV64)/release/kernel
KERNEL_BIN_RV64 := kernel-rv64.bin

QEMU_RV64 := qemu-system-riscv64
SMP ?= 4
QEMU_TRACE ?=
QEMU_RV64_FLAGS := -machine virt -nographic -bios default -kernel $(KERNEL_BIN_RV64) -smp $(SMP) $(QEMU_TRACE)

OBJCOPY := rust-objcopy

.PHONY: kernel-rv64 run-rv64 debug-rv64 gdbserver-rv64 qemu-test-rv64 test test-all clean

kernel-rv64:
	cargo build --release -p kernel --target $(TARGET_RV64)
	$(OBJCOPY) --binary-architecture=riscv64 $(KERNEL_ELF_RV64) --strip-all -O binary $(KERNEL_BIN_RV64)

run-rv64: kernel-rv64
	$(QEMU_RV64) $(QEMU_RV64_FLAGS)

# GDB debug: halt on start, GDB server on port 1234
debug-rv64: kernel-rv64
	$(QEMU_RV64) $(QEMU_RV64_FLAGS) -s -S

# GDB server without halt (attach to running kernel)
gdbserver-rv64: kernel-rv64
	$(QEMU_RV64) $(QEMU_RV64_FLAGS) -s

# QEMU integration test: boot, capture output, check expected strings
# Uses background QEMU + sleep + kill for macOS compatibility (no coreutils `timeout`)
qemu-test-rv64: kernel-rv64
	@echo "=== QEMU integration test (SMP=$(SMP)) ==="
	@TMPOUT=$$(mktemp); \
	$(QEMU_RV64) $(QEMU_RV64_FLAGS) > $$TMPOUT 2>&1 & \
	QPID=$$!; \
	sleep 15; \
	kill $$QPID 2>/dev/null; wait $$QPID 2>/dev/null; \
	PASS=0; FAIL=0; \
	for pat in "hello from async future" "woke after 100ms" "hello from CPU" "register clobber PASS" "buddy initialized" "pmap extract-only PASS" "pmap satp-switch PASS"; do \
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
