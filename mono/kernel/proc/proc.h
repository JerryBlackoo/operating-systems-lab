#ifndef KERNEL_PROC_PROC_H
#define KERNEL_PROC_PROC_H

#include <stdint.h>

#ifdef PLATFORM_ZYNQ
#include "arch/arm/trap.h"
#else
#include "arch/riscv/trap.h"
#endif

#define MAX_PROCS 16
#define KSTACK_SIZE 16384

enum proc_state {
    PROC_UNUSED = 0,
    PROC_NEW,
    PROC_READY,
    PROC_RUNNING,
    PROC_SLEEPING,
    PROC_ZOMBIE,
};

#ifdef PLATFORM_ZYNQ
struct context {
    uint32_t r4;
    uint32_t r5;
    uint32_t r6;
    uint32_t r7;
    uint32_t r8;
    uint32_t r9;
    uint32_t r10;
    uint32_t r11;
    uint32_t sp;
    uint32_t lr;
};
#else
struct context {
    uint64_t ra;
    uint64_t sp;
    uint64_t s0;
    uint64_t s1;
    uint64_t s2;
    uint64_t s3;
    uint64_t s4;
    uint64_t s5;
    uint64_t s6;
    uint64_t s7;
    uint64_t s8;
    uint64_t s9;
    uint64_t s10;
    uint64_t s11;
};
#endif

struct proc {
    int pid;
    enum proc_state state;
    char name[20];
    struct context ctx;
    struct trap_frame *tf;
    uint64_t pagetable;
    uint64_t heap_top;
    int is_user;
    int exit_code;
    int wait_pid;
    struct proc *parent;
    int fd[8];
    int heap_pages;     /* 当前进程已驻留的堆页数（RISC-V 请求分页用） */
    int vm_fault_count; /* 本进程生命周期内的缺页次数 */
    int vm_evict_count; /* 本进程页面被换出的次数 */
    void (*kthread_fn)(void);
    uint64_t sleep_until;
};

extern struct proc *current;

void proc_init(void);
struct proc *proc_alloc(void);
void proc_free(struct proc *p);
struct proc *proc_find(int pid);
/* 启动时创建的 idle 线程；调度器在没有就绪任务时回退到它。 */
struct proc *proc_idle(void);
void proc_dump(void);

struct proc *thread_create(const char *name, void (*fn)(void));
void thread_trampoline(void);
void thread_exit(void) __attribute__((noreturn));
void idle_loop(void);

void sched(void);
void yield(void);
void sched_tick(void);
int sched_need_resched(void);
void sched_clear_need_resched(void);

/* 在安全点（长循环/系统调用返回）消费时间片标志。 */
void sched_check(void);

#endif
