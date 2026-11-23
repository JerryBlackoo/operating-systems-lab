/*
 * link.ld — 地址布局是平台的属性，所以它属于 arch。
 *
 * 三个必须正确的点：
 *   1. .text.entry 必须最前 —— _start 必须落在 0x80200000，
 *      否则 QEMU 跳到那里执行的是别的函数的中间。
 *   2. .bss 必须 NOLOAD —— 否则镜像会被 .bss 的零填满。
 *   3. __kernel_end 必须页对齐 —— pmm 从这里往后分配。
 */
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
    . = 0x80200000;
    __kernel_start = .;

    .text : ALIGN(16) {
        KEEP(*(.text.entry))
        *(.text .text.*)
    }

    .rodata : ALIGN(16) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
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

        /* 启动栈放在 .bss 里，随 .bss 一起清零，不额外占镜像空间。 */
        . = ALIGN(16);
        __boot_stack_bottom = .;
        . += 0x4000;
        __boot_stack_top = .;
    }

    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.riscv.attributes)
    }
}
