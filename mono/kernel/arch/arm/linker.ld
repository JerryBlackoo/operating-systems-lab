OUTPUT_ARCH(arm)
ENTRY(_start)

SECTIONS
{
    /* Zynq 的 DDR 从 0x00100000 开始（低 1 MiB 是 OCM）。
     * U-Boot / QEMU 从这里加载内核。 */
    . = 0x00100000;
    __kernel_start = .;

    .text : ALIGN(8) {
        KEEP(*(.vectors))
        KEEP(*(.text.entry))
        *(.text .text.*)
    }

    .rodata : ALIGN(8) {
        *(.rodata .rodata.*)

        /* 用户态 flat 镜像；由 userland_blob.S 通过 .incbin 嵌入。 */
        . = ALIGN(8);
        *(.user)
    }

    .data : ALIGN(8) {
        *(.data .data.*)
    }

    .bss (NOLOAD) : ALIGN(8) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(8);

        /* 各异常模式独立栈。 */
        __svc_stack_bottom = .;
        . += 0x4000;
        __svc_stack_top = .;

        __irq_stack_bottom = .;
        . += 0x2000;
        __irq_stack_top = .;

        __abt_stack_bottom = .;
        . += 0x800;
        __abt_stack_top = .;

        __und_stack_bottom = .;
        . += 0x800;
        __und_stack_top = .;

        __fiq_stack_bottom = .;
        . += 0x800;
        __fiq_stack_top = .;

        . = ALIGN(8);
        __bss_end = .;
    }

    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.ARM.exidx*)
        *(.ARM.extab*)
    }
}
