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

/* switch_rv64.S 里的偏移是硬编码的；结构一改必须在这里编译失败。 */
#define CPU_CTX_ASSERT(field, offset)                                        \
    _Static_assert(offsetof(struct cpu_context, field) == (offset),          \
                   "cpu_context." #field " offset mismatch")

CPU_CTX_ASSERT(ra, 0);
CPU_CTX_ASSERT(sp, 8);
CPU_CTX_ASSERT(s0, 16);
CPU_CTX_ASSERT(s1, 24);
CPU_CTX_ASSERT(s2, 32);
CPU_CTX_ASSERT(s3, 40);
CPU_CTX_ASSERT(s4, 48);
CPU_CTX_ASSERT(s5, 56);
CPU_CTX_ASSERT(s6, 64);
CPU_CTX_ASSERT(s7, 72);
CPU_CTX_ASSERT(s8, 80);
CPU_CTX_ASSERT(s9, 88);
CPU_CTX_ASSERT(s10, 96);
CPU_CTX_ASSERT(s11, 104);
_Static_assert(sizeof(struct cpu_context) == 112,
               "cpu_context must be 14 words");

#undef CPU_CTX_ASSERT

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
    /* M6 pager：缺页阻塞期间由 VM 服务写入完成状态；ABI_OK 表示补页成功。 */
    int64_t fault_status;

    struct waitq *waitq;
    struct thread *wait_next;

    struct thread *ready_next;
    bool in_ready;
    bool used;
    bool reaped;
    /*
     * 延迟终止请求：线程已进入 RUNNING 或阻塞在异步 syscall 上、无法当场
     * 摘除时置位。它在 syscall_wait_park / trap 返回点自行退出，期间不再
     * 执行新的用户代码。见 sched_thread_terminate()。
     */
    bool kill_pending;
    /*
     * 该线程当前停在 syscall_wait_park 里：trap 帧被在途 reply 引用，
     * 不能直接 discard。异步调用被放弃（user_syscall_abandon_async）后清零。
     */
    bool async_parked;

    void (*entry)(void *);
    void *arg;
    const char *name;
    uint8_t cap_sid;
    struct waitq syscall_wq;
    /* 对齐 struct syscall_call；由 resident_trap 做 static_assert。 */
    uint64_t ucall_words[24];
};

void sched_thread_set_cap_sid(struct thread *t, uint8_t sid);
uint8_t sched_thread_cap_sid(const struct thread *t);

void sched_init(void);

struct thread *thread_create(struct as *as, void (*entry)(void *), void *arg,
                             void *stack_base, size_t stack_size,
                             const char *name);

void thread_exit(void) __attribute__((noreturn));
void thread_yield(void);

/*
 * 撤回尚未运行用户入口的 spawn（从就绪队列摘下并释放 AS/对象表项）。
 * 不可用于 current/idle/正在 RUNNING 的线程，也不可用于阻塞在
 * 异步 syscall 上的线程（其 trap 帧仍被在途 reply 引用）。
 */
int sched_thread_discard(struct thread *t);

/*
 * 请求终止一个已经跑起来的子线程。与 discard 的区别：
 *   - READY/BLOCKED 且不在 syscall_wq：立即走 discard 路径；
 *   - 阻塞在异步 syscall：置 kill_pending 并唤醒，线程在 trap 返回点退出
 *     （调用者必须先取消它的在途 reply，见 user_syscall_cancel_thread_async）；
 *   - RUNNING：置 kill_pending，待其下次陷入内核时退出。
 * 返回 0 表示已丢弃或已登记延迟退出；-1 表示目标不可终止（current/idle）。
 */
int sched_thread_terminate(struct thread *t);

/* 该线程是否已被请求终止。 */
bool sched_thread_kill_pending(const struct thread *t);

/* 撤销延迟终止请求（spawn 回滚后重启同一线程时用）。 */
void sched_thread_clear_kill(struct thread *t);

/*
 * 按 object id 在 TCB 池里查线程，不增加对象引用。
 * 调用者必须持 sched_lock()；返回指针只能在同一个临界区内使用。
 */
struct thread *sched_thread_by_id_locked(uint32_t id);

/*
 * 单调时间睡眠：deadline 是 clock 模块的绝对 ns 时间轴。
 * 由 clock 的 one-shot deadline 驱动；仅内核构建提供实现（主机自测不含
 * clock）。sleep 期间线程进入 BLOCKED 并挂在一个内部等待队列上。
 *
 * 没有其它就绪线程也没有 idle 时，函数会在内核里忙等到 deadline；
 * 正常启动流程应提供 idle 线程。
 */
void thread_sleep_ns(uint64_t ns);
void thread_sleep_until(uint64_t deadline_ns);

/* 阻塞会原子地改状态、入等待队列，然后调度；唤醒把线程放回就绪队列。 */
void thread_block(struct waitq *wq);
void thread_wake(struct thread *t);

/* 假定调用者已持有 sched_lock()，供 IPC 等模块做原子队列操作。 */
void thread_block_locked(struct waitq *wq);
void thread_wake_locked(struct thread *t);

void sched(void);
void sched_tick(void);
void sched_check(void);

/*
 * 周期性调度 tick 源。
 *
 * 调用前必须先 clock_init()。本函数武装一个 one-shot clock_timer，
 * 回调里调用 sched_tick() 并锚定旧 deadline 重装；同时置 sie.STIE。
 * sstatus.SIE 仍由调用者打开，以便先布置好状态再允许中断。
 *
 * 注意：仅武装 tick 不会自动产生抢占；trap 侧仍需在 S 态 timer 中断里
 * 调用 clock_handle_interrupt()，并在返回前调用 sched_check()。
 * sched_stop_tick() 取消 timer；只有启动前 STIE 原本为 0 时才清 STIE。
 *
 * 返回值：false 表示 period 为 0、已经在运行，或 timer 武装失败。
 */
bool sched_start_tick(uint64_t period_ns);
void sched_stop_tick(void);

/* 观测：真实 tick 回调次数与成功完成的上下文切换次数。 */
uint64_t sched_tick_count(void);
uint64_t sched_switch_count(void);
uint64_t sched_quantum_expiration_count(void);

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
