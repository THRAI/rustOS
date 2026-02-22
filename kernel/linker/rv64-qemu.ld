/* rv64-qemu.ld -- QEMU virt machine linker script for rv64 */
/* BASE_ADDRESS = 0x80200000 (after OpenSBI) */

OUTPUT_ARCH(riscv)
ENTRY(_start)

BASE_ADDRESS = 0x80200000;
MAX_CPUS = 8;
STACK_SIZE = 16384;  /* 16KB per hart */

SECTIONS
{
    . = BASE_ADDRESS;

    skernel = .;

    .text.entry : {
        *(.text.entry)
    }

    .text : ALIGN(4K) {
        stext = .;
        *(.text .text.*)
        etext = .;
    }

    .rodata : ALIGN(4K) {
        srodata = .;
        *(.rodata .rodata.*)
        erodata = .;
    }

    .data : ALIGN(4K) {
        sdata = .;
        *(.data .data.*)
        edata = .;
    }

    .bss : ALIGN(4K) {
        sbss = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        ebss = .;
    }

    . = ALIGN(4K);

    /* Per-hart boot stacks: 16KB each, MAX_CPUS=8 harts = 128KB total */
    boot_stack_bottom = .;
    . += STACK_SIZE * MAX_CPUS;
    boot_stack_top = .;

    ekernel = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
    }
}
