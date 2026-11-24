#include "proc/proc.h"

#include "arch.h"
#include "lib/kprintf.h"
#include "lib/string.h"
#include "mm/vm.h"

static struct proc procs[MAX_PROCS];
static uint8_t kstacks[MAX_PROCS][KSTACK_SIZE] __attribute__((aligned(16)));
static int next_pid = 1;
static struct proc *idle_proc;

struct proc *current;

static const char *state_name(enum proc_state state)
{
    switch (state) {
    case PROC_UNUSED:
        return "UNUSED";
    case PROC_NEW:
        return "NEW";
    case PROC_READY:
        return "READY";
    case PROC_RUNNING:
        return "RUN";
    case PROC_SLEEPING:
        return "SLEEP";
    case PROC_ZOMBIE:
        return "ZOMBIE";
    default:
        return "?";
    }
}

void proc_init(void)
{
    memset(procs, 0, sizeof(procs));
    next_pid = 1;

    struct proc *p = &procs[0];
    p->pid = 0;
    p->state = PROC_RUNNING;
    strncpy(p->name, "kmain", sizeof(p->name) - 1);
    p->pagetable = vm_kernel_pagetable();
    p->kthread_fn = 0;
    current = p;

    /* 内核主线程之后，idle 线程负责在没有可运行任务时进入 wfi。 */
    idle_proc = thread_create("idle", idle_loop);
    kprintf("[proc] init: kmain pid=0, %d slots\n", MAX_PROCS);
}

/* 启动期 kmain 在开中断后仍会创建用户进程；定时器抢占可能让另一个
 * 进程在 proc_alloc 的 memset 与 state=PROC_NEW 之间看到同一个
 * UNUSED 槽位。PCB 的分配/回收必须与调度器一样关中断。 */
struct proc *proc_alloc(void)
{
    uint64_t flags = arch_intr_save();
    struct proc *result = 0;

    for (int i = 0; i < MAX_PROCS; i++) {
        if (procs[i].state == PROC_UNUSED) {
            struct proc *p = &procs[i];
            memset(p, 0, sizeof(*p));
            p->pid = next_pid++;
            p->state = PROC_NEW;
            p->pagetable = vm_kernel_pagetable();
            p->tf = 0;
            p->wait_pid = -1;
            p->parent = 0;
            for (int fd = 0; fd < 8; fd++) {
                p->fd[fd] = -1;
            }
            result = p;
            break;
        }
    }
    arch_intr_restore(flags);
    return result;
}

void proc_free(struct proc *p)
{
    uint64_t flags;

    if (p == 0) {
        return;
    }
    flags = arch_intr_save();
    p->state = PROC_UNUSED;
    p->tf = 0;
    p->kthread_fn = 0;
    arch_intr_restore(flags);
}

struct proc *proc_at(int index)
{
    if (index < 0 || index >= MAX_PROCS) {
        return 0;
    }
    return &procs[index];
}

struct proc *proc_find(int pid)
{
    for (int i = 0; i < MAX_PROCS; i++) {
        if (procs[i].state != PROC_UNUSED && procs[i].pid == pid) {
            return &procs[i];
        }
    }
    return 0;
}

struct proc *proc_idle(void)
{
    return idle_proc;
}

void proc_dump(void)
{
    kprintf("[proc] pid state name\n");
    for (int i = 0; i < MAX_PROCS; i++) {
        if (procs[i].state != PROC_UNUSED) {
            kprintf("[proc] %d %s %s\n", procs[i].pid,
                    state_name(procs[i].state), procs[i].name);
        }
    }
}

void *proc_kstack_top(struct proc *p)
{
    int index = (int)(p - procs);
    return kstacks[index] + KSTACK_SIZE;
}

void *proc_kstack_base(struct proc *p)
{
    int index = (int)(p - procs);
    return kstacks[index];
}
