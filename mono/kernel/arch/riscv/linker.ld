OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
    /* QEMU virt 从这里加载内核，紧挨在 OpenSBI（0x80000000）之上。 */
    . = 0x80200000;
    __kernel_start = .;

    .text : ALIGN(16) {
        KEEP(*(.text.entry))
        *(.text .text.*)
    }

    .rodata : ALIGN(16) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)

        /* 用户态 flat 镜像；由 userland_blob.S 通过 .incbin 嵌入。 */
        . = ALIGN(16);
        *(.user)
    }

    .data : ALIGN(16) {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }

    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;

        /* 16 KiB 启动栈，随 BSS 一起清零。 */
        . = ALIGN(16);
        __stack_bottom = .;
        . += 0x4000;
        __stack_top = .;
    }

    /* 内核镜像结束位置按页对齐，物理页分配器从这里之后开始。 */
    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.eh_frame_hdr)
    }
}
