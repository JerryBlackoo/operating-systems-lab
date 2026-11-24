#ifndef KERNEL_SYSCALL_SYSCALL_H
#define KERNEL_SYSCALL_SYSCALL_H

#include <stdint.h>

int64_t syscall_dispatch(uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2);

/* 进程退出（或被杀）时释放 fd、交换槽位和用户地址空间。 */
void proc_exit_cleanup(void);

/* 定时器 tick 调用：若 UART 有数据，唤醒控制台读等待者。 */
void console_input_poll(void);

#endif
