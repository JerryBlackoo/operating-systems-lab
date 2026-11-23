#ifndef MICRO_KERNEL_SCHED_H
#define MICRO_KERNEL_SCHED_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "object.h"

struct as;

#define SCHED_MAX_THREADS 16
#define SCHED_QUANTUM_TICKS 5
#define THREAD_MSG_WORDS 4

enum thread_state {
    THREAD_READY = 0,
    THREAD_RUNNING,
    THREAD_BLOCKED,
    THREAD_DEAD
};

/*
 * RISC-V 上下文只保存 callee-saved 寄存器：
 *   ra, sp, s0-s11
 * caller-saved 已由 trap 入口保存在陷阱帧中。
 */
struct cpu_context {
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

/* TCB 中的消息寄存器；IPC 模块负责解释内容。 */
struct thread_msg {
    uint32_t tag;
    uint32_t reply_id;
    uint64_t words[THREAD_MSG_WORDS];
};

struct waitq {
    struct thread *head;
    struct thread *tail;
};

struct thread {
    struct object obj;
    struct cpu_context ctx;

    enum thread_state state;
    struct as *as;
    uint32_t time_left;

    struct thread_msg msg;
    int ipc_status;

    struct waitq *waitq;
    struct thread *wait_next;

    struct thread *ready_next;
    bool in_ready;
    bool used;
    bool reaped;

    void (*entry)(void *);
    void *arg;
    const char *name;
};

void sched_init(void);

struct thread *thread_create(struct as *as, void (*entry)(void *), void *arg,
                             void *stack_base, size_t stack_size,
                             const char *name);

void thread_exit(void) __attribute__((noreturn));
void thread_yield(void);

/* 阻塞会原子地改状态、入等待队列，然后调度；唤醒把线程放回就绪队列。 */
void thread_block(struct waitq *wq);
void thread_wake(struct thread *t);

/* 假定调用者已持有 sched_lock()，供 IPC 等模块做原子队列操作。 */
void thread_block_locked(struct waitq *wq);
void thread_wake_locked(struct thread *t);

void sched(void);
void sched_tick(void);
void sched_check(void);

/* 启动/自测用：强制某个线程成为 current，并把它从就绪队列摘下。 */
void sched_bootstrap(struct thread *t);
void sched_set_idle(struct thread *t);

struct thread *sched_current(void);
struct thread *sched_idle(void);
int sched_ready_count(void);

/*
 * 单核临界区：RISC-V 上清/恢复 sstatus.SIE；主机自测为空实现。
 * 这是 sched 向 IPC/objects 暴露的唯一并发缝。
 */
uint64_t sched_lock(void);
void sched_unlock(uint64_t flags);

void waitq_init(struct waitq *wq);
bool waitq_empty(const struct waitq *wq);
void waitq_push(struct waitq *wq, struct thread *t);
struct thread *waitq_pop(struct waitq *wq);
void waitq_remove(struct waitq *wq, struct thread *t);

/* 由 arch 提供：switch_rv64.S 或主机自测里的 fake。 */
void cpu_switch(struct cpu_context *old_ctx, struct cpu_context *new_ctx);
void cpu_switch_to(struct cpu_context *new_ctx);

#endif /* MICRO_KERNEL_SCHED_H */
