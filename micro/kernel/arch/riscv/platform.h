/*
 * platform.h — QEMU virt 的平台事实。
 *
 * 这些数字不是我发明的，是 dump DTB 实测的：
 *   qemu-system-riscv64 -machine virt,dumpdtb=/tmp/virt.dtb -m 128M -display none
 * 然后解析 memory@/soc/clint/plic/serial 节点得到的。
 *
 * 最小模型用编译期常量，不解析 DTB。
 * 但 _start 必须把 a1（DTB 物理地址）原样保存——这是零成本的未来兼容。
 */
#ifndef MICRO_ARCH_RISCV_PLATFORM_H
#define MICRO_ARCH_RISCV_PLATFORM_H

#define RAM_BASE        0x80000000UL   /* memory@80000000 */
#define RAM_SIZE        (128UL << 20)

#define KERNEL_BASE     0x80200000UL   /* OpenSBI fw_dynamic 的 next-stage */

#define TIMEBASE_HZ     10000000UL     /* 10 MHz，DTB timebase-frequency */

#define CLINT_BASE      0x02000000UL
#define CLINT_MTIMECMP(h) (CLINT_BASE + 0x4000UL + 8UL * (h))
#define CLINT_MTIME     (CLINT_BASE + 0xBFF8UL)

#define PLIC_BASE       0x0c000000UL
#define PLIC_NDEV       95

#define UART0_BASE      0x10000000UL
#define UART0_IRQ       10
#define UART0_CLK       3686400UL

/* 写 0x00100000 地址的 0x5555 可以关机 —— 让 make run 自己退出。 */
#define QEMU_TEST_BASE  0x00100000UL
#define QEMU_POWEROFF   0x5555
#define QEMU_REBOOT     0x7777

/* SBI legacy EID */
#define SBI_EXT_LEGACY_PUTCHAR 0x01
#define SBI_EXT_LEGACY_SET_TIMER 0x00
#define SBI_EXT_SRST 0x53525354

#endif /* MICRO_ARCH_RISCV_PLATFORM_H */
