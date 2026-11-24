#ifndef KERNEL_ARCH_H
#define KERNEL_ARCH_H

#include <stdint.h>

/* 当前架构/平台的简短名称，用于启动日志。 */
const char *arch_name(void);

/* 控制台使用的硬件描述，用于启动日志。 */
const char *arch_console_info(void);

/* 安装异常向量与中断控制器；此时全局中断仍然关闭。 */
void arch_init(void);

/* 打开 CPU 全局中断。 */
void arch_irq_enable(void);

/* 空闲等待一次中断。 */
void arch_idle(void);

/* 关闭/恢复 CPU 全局中断；save 返回恢复所需的状态。 */
uint64_t arch_intr_save(void);
void arch_intr_restore(uint64_t flags);
void arch_intr_disable(void);

/* 停机；不返回。 */
__attribute__((noreturn)) void arch_halt(void);

#endif
