ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);
    data PT_LOAD FLAGS(6);
}

SECTIONS
{
    /* 64 KiB 对齐：避开低端引导区（0x7c00 stage1/stage2），
     * 同时满足 INT 13h 一次传输不得跨越 64 KiB 边界。 */
    . = 0x20000;
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

    ASSERTSYM = __bss_end;
    ASSERT(__bss_end <= 0x3f000, "Kernel image must stay below page tables at 0x40000")
    /DISCARD/ : { *(.comment) *(.note.GNU-stack) }
}
