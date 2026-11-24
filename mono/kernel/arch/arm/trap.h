#ifndef KERNEL_ARCH_ARM_TRAP_H
#define KERNEL_ARCH_ARM_TRAP_H

#include <stdint.h>

/* 规范化的用户 trap frame：r0-r12, sp, lr(用户态 LR，不是 SVC 返回地址), pc, cpsr。
 * switch.S 的 arm_enter_user 直接按此布局恢复用户上下文。
 * 注意：vectors.S 的 SVC 入口帧布局不同（r0-r12、SVC 返回地址、用户 sp/lr、SPSR），
 * 由 arm_prepare_syscall() 转换后写入本结构。 */
struct trap_frame {
    uint32_t r[13];
    uint32_t sp;
    uint32_t lr;
    uint32_t pc;
    uint32_t cpsr;
};

#endif
