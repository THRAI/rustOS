# BSD-Async Rust OS Kernel -- Top-level Makefile

HOST_TARGET := $(shell rustc -vV | grep host | cut -d' ' -f2)
TARGET_RV64 := riscv64gc-unknown-none-elf
KERNEL_ELF_RV64 := target/$(TARGET_RV64)/release/kernel
KERNEL_BIN_RV64 := kernel-rv64.bin

QEMU_RV64 := qemu-system-riscv64
QEMU_RV64_FLAGS := -machine virt -nographic -bios default -kernel $(KERNEL_BIN_RV64)

OBJCOPY := rust-objcopy

.PHONY: kernel-rv64 run-rv64 test clean

kernel-rv64:
	cargo build --release -p kernel --target $(TARGET_RV64)
	$(OBJCOPY) --binary-architecture=riscv64 $(KERNEL_ELF_RV64) --strip-all -O binary $(KERNEL_BIN_RV64)

run-rv64: kernel-rv64
	$(QEMU_RV64) $(QEMU_RV64_FLAGS)

test:
	cargo test --lib -p hal-common --target $(HOST_TARGET)

clean:
	cargo clean
	rm -f $(KERNEL_BIN_RV64)
