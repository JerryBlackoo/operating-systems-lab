/* 用户程序链接脚本 */

OUTPUT_ARCH(riscv)
ENTRY(_start)

/* 用户程序基地址 */
BASE_ADDRESS = 0x10000;

SECTIONS
{
    . = BASE_ADDRESS;
    
    .text : {
        *(.text.entry)
        *(.text .text.*)
    }
    
    .rodata : {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }
    
    .data : {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }
    
    .bss : {
        __bss_start = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        __bss_end = .;
    }
    
    /DISCARD/ : {
        *(.eh_frame)
        *(.debug*)
    }
}
