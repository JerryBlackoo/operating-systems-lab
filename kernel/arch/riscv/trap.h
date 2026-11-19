#ifndef KERNEL_ARCH_RISCV_TRAP_H
#define KERNEL_ARCH_RISCV_TRAP_H

#include <stdint.h>

/* 与 trap.S 的保存布局一致：x[0] 占位，x[1]..x[31] 对应整数寄存器。 */
struct trap_frame {
    uint64_t x[32];
};

void trap_entry(void);

/* 安装 stvec，并打开 S-mode 时钟中断源（尚未全局开中断）。 */
void trap_init(void);

/* 打开 sstatus.SIE，此后时钟中断可以进入。 */
void trap_enable(void);

void trap_dispatch(struct trap_frame *tf);

#endif
