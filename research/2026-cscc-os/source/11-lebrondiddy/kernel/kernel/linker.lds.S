OUTPUT_ARCH(%ARCH%)
ENTRY(__entry)

BASE_ADDRESS = %KERNEL_BASE%;
ENTRY_ADDRESS = %ENTRY_ADDR%;
LOAD_OFFSET = BASE_ADDRESS - ENTRY_ADDRESS;

SECTIONS
{
    . = BASE_ADDRESS;
    _skernel = .;
    __entry = ENTRY_ADDRESS;

    .text ALIGN(4K): AT(ADDR(.text) - LOAD_OFFSET) {
        *(.multiboot .multiboot.*)
        stext = .;
        KEEP(*(.text.entry))
        *(.text .text.*)
        etext = .;
    }

    .rodata ALIGN(4K): AT(ADDR(.rodata) - LOAD_OFFSET) {
        srodata = .;
        *(.rodata .rodata.*)
        erodata = .;
    }

    .got : AT(ADDR(.got) - LOAD_OFFSET) {
        *(.got .got.*)
    }

    .data ALIGN(4K): AT(ADDR(.data) - LOAD_OFFSET) {
        _sdata = .;
        . = ALIGN(4K);
        *(.data.boot_page_table)

        *(.data .data.*)
        *(.sdata .sdata.*)
        _edata = .;
    }

    .bss ALIGN(4K): AT(ADDR(.bss) - LOAD_OFFSET) {
        _load_end = .;
        *(.bss.bstack .bss.bstack.*)
        _sbss = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        _ebss = .;
    }

    PROVIDE(_end = .);
    /DISCARD/ : {
        *(.comment) *(.gnu*) *(.note*) *(.eh_frame*)
    }
}
