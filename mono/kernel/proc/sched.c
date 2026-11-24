#include "proc/proc.h"

#include "arch.h"
#include "lib/kprintf.h"
#include "mm/vm.h"

/* 5 个 10 ms tick = 50 ms；教材默认可用 10，这里缩短以便演示。 */
#define TIME_SLICE_TICKS 5

static uint32_t slice_ticks;
static int need_resched;
static int last_pick;

void swtch(struct context *old, struct context *new);
struct proc *proc_at(int index);

static struct proc *pick_next(void)
{
    for (int n = 1; n <= MAX_PROCS; n++) {
        int i = (last_pick + n) % MAX_PROCS;
        struct proc *p;

        /* slot 0 是启动线程 kmain：启动期被定时器抢占后它会变成 READY，
         * 必须允许恢复，否则启动会永久停在半途。进入 idle 后它是
         * SLEEPING，不会被选中。 */
        p = proc_at(i);
        if (p != 0 && p->state == PROC_READY) {
            last_pick = i;
            return p;
        }
    }
    return 0;
}

void sched(void)
{
    struct proc *prev = current;
    struct proc *next;

    for (int i = 0; i < MAX_PROCS; i++) {
        struct proc *p = proc_at(i);
        if (p != 0 && p != current && p->state == PROC_ZOMBIE) {
            /* 用户页已在 sys_exit 中释放；这里只回收 PCB 槽位。 */
            proc_free(p);
        }
    }

    next = pick_next();

    if (next == 0) {
        if (prev->state == PROC_RUNNING) {
            return;
        }
        /* 没有就绪任务时回退到 idle；必须先确认它确实是 READY，否则会把
         * UNUSED/ZOMBIE 槽位强行置为 RUNNING。 */
        next = proc_idle();
        if (next == 0 || next->state != PROC_READY) {
            return;
        }
        last_pick = (int)(next - proc_at(0));
    }

    if (prev == next) {
        next->state = PROC_RUNNING;
        return;
    }

    if (prev->state == PROC_RUNNING) {
        prev->state = PROC_READY;
    }
    next->state = PROC_RUNNING;
    current = next;
    vm_switch(next->pagetable);
    swtch(&prev->ctx, &next->ctx);
}

void yield(void)
{
    uint64_t flags = arch_intr_save();

    if (current->state == PROC_RUNNING) {
        current->state = PROC_READY;
    }
    sched();
    arch_intr_restore(flags);
}

void sched_tick(void)
{
    slice_ticks++;
    if (slice_ticks >= TIME_SLICE_TICKS) {
        slice_ticks = 0;
        need_resched = 1;
    }
}

int sched_need_resched(void)
{
    return need_resched;
}

void sched_clear_need_resched(void)
{
    need_resched = 0;
}

void sched_check(void)
{
    uint64_t flags = arch_intr_save();

    if (need_resched != 0) {
        need_resched = 0;
        yield();
    }
    arch_intr_restore(flags);
}
