ENTRY(_start)

SECTIONS
{
    . = 0x1000;
    .text : {
        KEEP(*(.text.entry))
        *(.text .text.*)
    }
    .rodata : { *(.rodata .rodata.*) }
    .data : { *(.data .data.*) }
    ASSERT(. <= 0x1200, "PBR reads one 512-byte kernel sector")

    .bss (NOLOAD) : {
        __bss_start = .;
        *(.bss .bss.* COMMON)
        __bss_end = .;
    }
    ASSERT(. <= 0x6c00, "Leave 4 KiB below 0x7c00 for the boot stack")

    /DISCARD/ : { *(.comment) *(.note.GNU-stack) }
}
