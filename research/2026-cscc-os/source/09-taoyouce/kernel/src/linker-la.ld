/* ==========================================================================
 * RustMicroOS 链接脚本 — LoongArch64
 * ==========================================================================
 *
 * 两段式链接：
 *   .text.entry 段链接在物理地址 0x0020_0000（QEMU entry point）
 *   其余段链接在 DMW0 窗口地址 0x9000_0000_xxxx_xxxx
 *
 * entry.S 中的 _start 在 DA=1 模式下从物理地址 0x0020_0000 执行，
 * 配置 DMW 后切换到 PG=1，跳转到 DMW0 窗口的高地址继续执行。
 * ========================================================================== */

OUTPUT_ARCH(loongarch)
ENTRY(_start)

PHYS_BASE = 0x00200000;
VIRT_BASE = 0x9000000000200000;

SECTIONS
{
    /* ====================================================================
     * 启动入口段 — 链接在物理地址（DA=1 模式下执行）
     * ==================================================================== */
    . = PHYS_BASE;
    skernel = .;

    .text.entry : {
        *(.text.entry)
    }
    . = ALIGN(4K);

    /* ====================================================================
     * 切换到虚拟地址空间（DMW0 窗口内）
     * 使用 AT() 确保 paddr 连续
     * ==================================================================== */
    . = VIRT_BASE + (. - PHYS_BASE);

    stext = .;
    .text : AT(PHYS_BASE + (stext - VIRT_BASE)) {
        *(.text .text.*)
    }
    . = ALIGN(4K);
    etext = .;

    srodata = .;
    .rodata : AT(PHYS_BASE + (srodata - VIRT_BASE)) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }
    . = ALIGN(4K);
    erodata = .;

    sdata = .;
    .data : AT(PHYS_BASE + (sdata - VIRT_BASE)) {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }
    . = ALIGN(4K);
    edata = .;

    /* 不清零大块 BSS（NOLOAD）：先于 .bss 匹配以免被 *(.bss.*) 吞掉；
     * 位于 sbss..ebss 之外，entry.S 清零循环跳过它（96MB 堆 ≈ 省39M 条指令） */
    .bss.noinit (NOLOAD) : {
        . = ALIGN(4K);
        *(.bss.noinit)
    }
    . = ALIGN(4K);

    sbss_with_stack = .;
    .bss : AT(PHYS_BASE + (sbss_with_stack - VIRT_BASE)) {
        *(.bss.stack)
        . = ALIGN(8);
        sbss = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
    }
    . = ALIGN(4K);
    ebss = .;

    ekernel = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
    }
}
