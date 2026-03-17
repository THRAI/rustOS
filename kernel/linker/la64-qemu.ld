/* la64-qemu.ld -- QEMU virt machine linker script for LA64 */
/* BASE_ADDRESS = 0x9000000080200000 (cached XKPRANGE mapping of PA 0x80200000) */

OUTPUT_ARCH(loongarch)
ENTRY(_start)

BASE_ADDRESS = 0x9000000080200000;
MAX_CPUS = 8;
STACK_SIZE = 65536;  /* 64KB per CPU */

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

    boot_stack_bottom = .;
    . += STACK_SIZE * MAX_CPUS;
    boot_stack_top = .;

    ekernel = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
        *(.debug*)
        *(.note .note.*)
    }
}
