#include "pager.h"

#include "cap_as.h"
#include "abi/abi.h"
#include "abi/service.h"
#include "clock/clock.h"
#include "csr.h"
#include "demo.h"
#include "ipc/ipc.h"
#include "memory.h"
#include "object.h"
#include "sched.h"
#include "trap.h"

static struct endpoint *vm_ep;
static struct waitq pager_waitq;
/* VM 调用 ABI_PAGER_READY 后置位；在此之前缺页直接拒绝而不是永久挂起。 */
static bool vm_ready;

#define PAGER_MAX_PENDING 8
/*
 * 单次缺页的等待上限。VM 是用户态服务，可能因为自身 bug 或被杀掉而永远
 * 不回复；没有这个上限，缺页线程会一直挂在 pager_waitq 上，既占着 AS
 * 也不响应任何信号。取值留足正常补页的时间（QEMU 下单次补页在毫秒级）。
 */
#define PAGER_TIMEOUT_NS (200ULL * 1000ULL * 1000ULL)

enum pager_pending_state {
    PAGER_PENDING = 0,
    PAGER_DONE,
};

struct pager_pending {
    uint64_t tid;
    uint64_t va;
    uint64_t as_id;
    int64_t status;
    uint8_t state; /* PAGER_PENDING / PAGER_DONE */
    bool used;
    /*
     * 超时看门狗：VM 若卡住不调 ABI_PAGER_DONE/FAIL，缺页线程会永远阻塞在
     * pager_waitq 上。到期时把该槽标成 DONE(ABI_TIMED_OUT) 并唤醒等待者，
     * 由 reject_and_exit 结束缺页线程。
     */
    struct clock_timer watchdog;
};

static struct pager_pending pending[PAGER_MAX_PENDING];

static void pager_watchdog_fire(struct clock_timer *timer);

static char *put_dec(char *p, uint64_t v)
{
    char tmp[24];
    unsigned n = 0;

    if (v == 0) {
        *p++ = '0';
        return p;
    }
    while (v != 0) {
        tmp[n++] = (char)('0' + v % 10);
        v /= 10;
    }
    while (n != 0) {
        *p++ = tmp[--n];
    }
    return p;
}

static uint64_t page_align(uint64_t va)
{
    return va & ~(uint64_t)(PAGE_SIZE - 1);
}

static char *put_str(char *p, const char *s)
{
    while (*s) {
        *p++ = *s++;
    }
    return p;
}

static char *put_hex(char *p, uint64_t v)
{
    static const char dg[] = "0123456789abcdef";

    for (int i = 60; i >= 0; i -= 4) {
        *p++ = dg[(v >> i) & 0xf];
    }
    return p;
}

static unsigned fault_access(uint64_t scause)
{
    if (scause == (uint64_t)EXC_INST_PAGEFAULT) {
        return ABI_EXEC;
    }
    if (scause == (uint64_t)EXC_STORE_PAGEFAULT) {
        return ABI_WRITE | ABI_READ;
    }
    return ABI_READ;
}

void pager_init(struct endpoint *vm_recv)
{
    vm_ep = vm_recv;
    vm_ready = false;
    waitq_init(&pager_waitq);
    for (unsigned i = 0; i < PAGER_MAX_PENDING; i++) {
        pending[i] = (struct pager_pending){0};
        /* INACTIVE 状态才能被 pending_add 重新 init。 */
        clock_timer_init(&pending[i].watchdog, pager_watchdog_fire, 0);
    }
}

void pager_vm_ready(void)
{
    vm_ready = true;
}

static struct pager_pending *pending_add(uint64_t tid, uint64_t va,
                                         uint64_t as_id)
{
    for (unsigned i = 0; i < PAGER_MAX_PENDING; i++) {
        if (!pending[i].used) {
            /*
             * 槽位上一次的生命周期可能留下了 RUNNING 的看门狗（超时回调
             * 正在执行时被 pending_remove_tid 清掉，cancel 对 RUNNING 无效）。
             * clock_timer_init 只允许 INACTIVE，因此这里必须等它自然结束；
             * 否则会破坏时钟队列的链接。
             */
            if (pending[i].watchdog.state == CLOCK_TIMER_RUNNING) {
                continue;
            }
            pending[i] = (struct pager_pending){
                .tid = tid,
                .va = page_align(va),
                .as_id = as_id,
                .status = ABI_BUSY,
                .state = PAGER_PENDING,
                .used = true,
            };
            clock_timer_init(&pending[i].watchdog, pager_watchdog_fire, 0);
            /*
             * arm 可能失败（例如同一 hart 上该定时器的上一次回调仍在
             * RUNNING）。失败意味着这条缺页没有超时保护，宁可现在就拒绝
             * 也不能让缺页线程无界阻塞。
             */
            if (!clock_timer_arm(&pending[i].watchdog,
                                 clock_deadline_after(clock_now(),
                                                      PAGER_TIMEOUT_NS))) {
                pending[i] = (struct pager_pending){0};
                return NULL;
            }
            return &pending[i];
        }
    }
    return NULL;
}

static struct pager_pending *pending_find(uint64_t tid, uint64_t va,
                                          uint64_t as_id)
{
    va = page_align(va);
    for (unsigned i = 0; i < PAGER_MAX_PENDING; i++) {
        if (pending[i].used && pending[i].tid == tid && pending[i].va == va &&
            pending[i].as_id == as_id) {
            return &pending[i];
        }
    }
    return NULL;
}

static struct pager_pending *pending_find_tid(uint64_t tid)
{
    for (unsigned i = 0; i < PAGER_MAX_PENDING; i++) {
        if (pending[i].used && pending[i].tid == tid) {
            return &pending[i];
        }
    }
    return NULL;
}

static void pending_remove_tid(uint64_t tid)
{
    for (unsigned i = 0; i < PAGER_MAX_PENDING; i++) {
        if (pending[i].used && pending[i].tid == tid) {
            (void)clock_timer_cancel(&pending[i].watchdog);
            pending[i] = (struct pager_pending){0};
        }
    }
}

/*
 * 超时回调：定时器中断上下文。
 *
 * 只把槽位标成 DONE 并唤醒挂在该线程上的等待者，不做任何分配、日志或
 * 解锁。真正的拒绝（打印 + thread_exit）由缺页线程在 pager_waitq 唤醒后
 * 自己完成，因此这里保持有界。
 */
static void pager_watchdog_fire(struct clock_timer *timer)
{
    struct thread *fault;
    uint64_t flags;

    (void)timer;
    /*
     * 找到对应的 pending 槽：定时器的 arg 就是槽地址。槽在 remove 时
     * 会先 cancel 定时器，因此这里看到的 used 一定仍为真（除非回调与
     * remove 竞争，那时 cancel 返回 false，我们据 used 再判一次）。
     */
    flags = sched_lock();
    for (unsigned i = 0; i < PAGER_MAX_PENDING; i++) {
        struct pager_pending *slot = &pending[i];

        if (&slot->watchdog != timer) {
            continue;
        }
        if (!slot->used || slot->state != PAGER_PENDING) {
            break;
        }
        slot->state = PAGER_DONE;
        slot->status = ABI_TIMED_OUT;
        fault = sched_thread_by_id_locked((uint32_t)slot->tid);
        if (fault != 0 && fault->state == THREAD_BLOCKED) {
            fault->fault_status = ABI_TIMED_OUT;
            thread_wake_locked(fault);
        }
        break;
    }
    sched_unlock(flags);
}

bool pager_authorize_map(uint64_t fault_tid, uint64_t va, uint64_t as_id)
{
    struct pager_pending *slot = pending_find(fault_tid, va, as_id);

    return slot != NULL && slot->state == PAGER_PENDING;
}

/*
 * 真正的拒绝路径：先写 pending 状态，再仅在缺页线程确实 BLOCKED 时唤醒。
 * 这样 fault 线程在“已投递、尚未 BLOCKED”的窗口里也能看到完成状态，
 * 不会丢唤醒后永久阻塞。
 */
static bool pending_complete_locked(struct thread *fault, int64_t status)
{
    struct pager_pending *slot = pending_find_tid(object_get_id(&fault->obj));

    if (slot == NULL) {
        return false;
    }
    /* 正常完成路径要撤掉看门狗，否则它稍后还会再“拒绝”一次。 */
    (void)clock_timer_cancel(&slot->watchdog);
    slot->state = PAGER_DONE;
    slot->status = status;
    if (fault->state == THREAD_BLOCKED) {
        fault->fault_status = status;
        thread_wake_locked(fault);
    }
    return true;
}

static _Noreturn void reject_and_exit(uint64_t tid, int64_t status)
{
    char line[128];
    char *p = line;

    p = put_str(p, "[fault] rejected tid=");
    p = put_dec(p, tid);
    p = put_str(p, " status=");
    if (status < 0) {
        *p++ = '-';
        p = put_dec(p, (uint64_t)(-status));
    } else {
        p = put_dec(p, (uint64_t)status);
    }
    *p = '\0';
    demo_log(line);
    thread_exit();
}

bool pager_handle_user_fault(struct trap_frame *frame)
{
    struct thread *fault = sched_current();
    struct pager_pending *slot;
    ipc_message msg = {0};
    char line[160];
    uint64_t tid;
    char *p;
    int rc;

    if (fault == 0 || fault->as == 0) {
        return false;
    }
    tid = object_get_id(&fault->obj);
    if (vm_ep == 0) {
        reject_and_exit(tid, ABI_CLOSED);
    }
    if (!vm_ready) {
        reject_and_exit(tid, ABI_BUSY);
    }

    if (cap_vm_ensure_as_map(fault->as) != CAP_OK ||
        !cap_vm_can_map_as(fault->as)) {
        reject_and_exit(tid, ABI_NO_MEMORY);
    }

    slot = pending_add(tid, frame->stval, object_get_id(&fault->as->obj));
    if (slot == NULL) {
        reject_and_exit(tid, ABI_NO_MEMORY);
    }

    msg.tag = TAG_VM_FAULT;
    msg.words[0] = tid;
    msg.words[1] = frame->stval;
    msg.words[2] = fault_access(frame->scause);

    p = line;
    p = put_str(p, "[fault] tid=");
    p = put_dec(p, tid);
    p = put_str(p, " va=");
    p = put_hex(p, frame->stval);
    p = put_str(p, " access=");
    p = put_hex(p, msg.words[2]);
    p = put_str(p, " -> pager request\n");
    *p = '\0';
    demo_log(line);

    /*
     * 排队投递：VM 在 recvq 上则直接移交；否则消息进入 endpoint 的
     * posts 队列。并发两次缺页不会因为“VM 恰好不在等待”而丢失。
     */
    rc = ep_post(vm_ep, &msg);
    if (rc == IPC_EDEAD) {
        pending_remove_tid(tid);
        reject_and_exit(tid, ABI_CLOSED);
    }
    if (rc == IPC_ENOMEM) {
        pending_remove_tid(tid);
        reject_and_exit(tid, ABI_NO_MEMORY);
    }
    if (rc != IPC_OK) {
        pending_remove_tid(tid);
        reject_and_exit(tid, ABI_BAD_ARG);
    }

    {
        uint64_t flags = sched_lock();

        slot = pending_find(tid, frame->stval, object_get_id(&fault->as->obj));
        if (slot != NULL && slot->state == PAGER_DONE) {
            int64_t status = slot->status;

            /*
             * 先撤看门狗再清槽：直接把含 clock_timer 的结构体清零会让
             * 时钟队列里指向它的链接失效（定时器仍 ENQUEUED 时尤其危险）。
             */
            (void)clock_timer_cancel(&slot->watchdog);
            *slot = (struct pager_pending){0};
            sched_unlock(flags);
            if (status != ABI_OK) {
                reject_and_exit(tid, status);
            }
            return true;
        }

        fault->fault_status = ABI_BUSY;
        thread_block_locked(&pager_waitq);
        sched();
        sched_unlock(flags);
    }

    if (fault->fault_status != ABI_OK) {
        int64_t status = fault->fault_status;

        pending_remove_tid(tid);
        reject_and_exit(tid, status);
    }

    /*
     * 成功补页也要回收 pending 槽。否则每成功一次就永久占一个槽，
     * 累计 PAGER_MAX_PENDING 次后所有后续缺页都会被当作 NO_MEMORY 拒绝。
     */
    pending_remove_tid(tid);
    return true;
}

int64_t pager_complete(uint64_t fault_tid)
{
    struct object *obj = object_lookup((uint32_t)fault_tid, OBJ_THREAD);
    struct thread *fault;
    uint64_t flags;

    if (obj == 0) {
        return ABI_BAD_ARG;
    }
    fault = (struct thread *)obj;
    if (fault->state == THREAD_DEAD) {
        pending_remove_tid(fault_tid);
        object_unref(obj);
        return ABI_CLOSED;
    }

    flags = sched_lock();
    bool completed = pending_complete_locked(fault, ABI_OK);
    sched_unlock(flags);
    object_unref(obj);
    if (completed) {
        demo_log("[fault] resumed");
    }
    return completed ? ABI_OK : ABI_BAD_ARG;
}

int64_t pager_fail(uint64_t fault_tid, int64_t status)
{
    struct object *obj = object_lookup((uint32_t)fault_tid, OBJ_THREAD);
    struct thread *fault;
    uint64_t flags;

    if (obj == 0) {
        return ABI_BAD_ARG;
    }
    fault = (struct thread *)obj;
    if (fault->state == THREAD_DEAD) {
        pending_remove_tid(fault_tid);
        object_unref(obj);
        return ABI_CLOSED;
    }

    flags = sched_lock();
    bool completed = pending_complete_locked(fault, status < 0 ? status : ABI_DENIED);
    sched_unlock(flags);
    object_unref(obj);
    return completed ? ABI_OK : ABI_BAD_ARG;
}