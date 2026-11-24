ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);
    data PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0x1000;
    .text : {
        KEEP(*(.text.entry))
        *(.text .text.*)
    } :text
    .rodata : { *(.rodata .rodata.*) } :text
    .data : { *(.data .data.*) } :data
    .bss (NOLOAD) : {
        __bss_start = .;
        *(.bss .bss.* COMMON)
        __bss_end = .;
    } :data

    ASSERT(__bss_end <= 0x6c00, "Reserve 4 KiB below 0x7c00 for the scheduler stack")
    /DISCARD/ : { *(.comment) *(.note.GNU-stack) }
}
