/* RustMicroOS 链接脚本 */
/* 目标: RISC-V 64-bit */

OUTPUT_ARCH(riscv)
ENTRY(_start)

/* QEMU virt 平台内核加载地址 */
BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;
    skernel = .;

    /* 代码段 */
    stext = .;
    .text : {
        *(.text.entry)      /* 入口代码 */
        *(.text .text.*)
    }
    . = ALIGN(4K);
    etext = .;

    /* 只读数据段 */
    srodata = .;
    .rodata : {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }
    . = ALIGN(4K);
    erodata = .;

    /* 数据段 */
    sdata = .;
    .data : {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }
    . = ALIGN(4K);
    edata = .;

    /* 不清零大块 BSS（NOLOAD）：先于 .bss 匹配以免被 *(.bss.*) 吞掉；
     * 位于 sbss..ebss 之外，boot 清零循环跳过它（96MB 堆 ≈ 省39M 条指令） */
    .bss.noinit (NOLOAD) : {
        . = ALIGN(4K);
        *(.bss.noinit)
    }
    . = ALIGN(4K);

    /* BSS 段 */
    sbss_with_stack = .;
    .bss : {
        *(.bss.stack)       /* 内核栈 */
        . = ALIGN(8);
        sbss = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
    }
    . = ALIGN(4K);
    ebss = .;

    ekernel = .;

    /* 丢弃不需要的段 */
    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
    }
}
