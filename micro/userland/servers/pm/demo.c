#include "pm.h"
#include "demo.h"

#include "capability/capability.h"
#include "memory.h"
#include "sched/sched.h"

enum {
    AS_BASE = 0x100,
    CAPS_BASE = 0x200,
    THREAD_BASE = 0x300,
    MAX_HANDLES = 8,
    MAX_CHILDREN = 8,
    MAX_EVENTS = 24,
};

/* 三个独立提供者模拟 memory/vm、capability 与 sched：句柄空间互不重叠。 */
struct provider {
    uint32_t live[MAX_HANDLES];
    unsigned count, issued;
};

struct child {
    uint64_t pid;
    uint32_t as, caps, thread;
};

struct task_fixture {
    struct pm *pm;
    struct provider as, caps, thread;
    struct child children[MAX_CHILDREN];
    unsigned child_count;
    uint64_t parent;
    uint32_t last_thread;
    unsigned starts, releases;
    enum task_resource order[MAX_EVENTS];
    uint32_t released[MAX_EVENTS];
    bool fail_caps, fail_thread;
};

static bool provider_has(const struct provider *provider, uint32_t handle)
{
    for (unsigned i = 0; i < provider->count; i++) {
        if (provider->live[i] == handle)
            return true;
    }
    return false;
}

static uint32_t provider_issue(struct provider *provider, uint32_t base)
{
    CHECK(provider->count < MAX_HANDLES);
    uint32_t handle = base + ++provider->issued;
    provider->live[provider->count++] = handle;
    return handle;
}

static void provider_drop(struct provider *provider, uint32_t handle)
{
    for (unsigned i = 0; i < provider->count; i++) {
        if (provider->live[i] != handle)
            continue;
        provider->live[i] = provider->live[--provider->count];
        return;
    }
    CHECK(false);
}

static uint32_t create_as(void *ctx, uint32_t image)
{
    struct task_fixture *task = ctx;
    CHECK(image == 7);
    return provider_issue(&task->as, AS_BASE);
}

static uint32_t create_caps(void *ctx, uint32_t grants)
{
    struct task_fixture *task = ctx;
    CHECK(grants == 3);
    if (task->fail_caps)
        return 0;
    return provider_issue(&task->caps, CAPS_BASE);
}

static uint32_t create_thread(void *ctx, uint32_t as, uint32_t caps)
{
    struct task_fixture *task = ctx;
    CHECK(as == AS_BASE + task->as.issued && caps == CAPS_BASE + task->caps.issued);
    if (task->fail_thread)
        return 0;
    task->last_thread = provider_issue(&task->thread, THREAD_BASE);
    return task->last_thread;
}

static void start(void *ctx, uint32_t thread)
{
    struct task_fixture *task = ctx;
    int status = 0;
    bool published = false;

    CHECK(thread == task->last_thread);
    for (unsigned i = 0; i < PM_CAPACITY; i++) {
        const struct pm_process *p = &task->pm->processes[i];
        if (p->state != PM_LIVE || p->thread != thread)
            continue;
        published = true;
        CHECK(p->pid == task->pm->last_pid && p->parent == task->parent);
        CHECK(p->as == AS_BASE + task->as.issued);
        CHECK(p->caps == CAPS_BASE + task->caps.issued);
    }
    CHECK(published);
    CHECK(task->child_count < MAX_CHILDREN);

    /* start 在记录发布之后才被调用：此时 wait 应看到 LIVE 而不是空槽。 */
    CHECK(pm_wait(task->pm, task->parent, task->pm->last_pid, &status) == PM_RUNNING);
    CHECK(status == 0);

    task->children[task->child_count++] = (struct child){
        .pid = task->pm->last_pid,
        .as = AS_BASE + task->as.issued,
        .caps = CAPS_BASE + task->caps.issued,
        .thread = thread,
    };
    task->starts++;
}

static void release(void *ctx, enum task_resource kind, uint32_t handle)
{
    struct task_fixture *task = ctx;
    CHECK(task->releases < MAX_EVENTS);
    switch (kind) {
    case TASK_AS:
        provider_drop(&task->as, handle);
        break;
    case TASK_CAPS:
        provider_drop(&task->caps, handle);
        break;
    case TASK_THREAD:
        provider_drop(&task->thread, handle);
        break;
    default:
        CHECK(false);
    }
    task->order[task->releases] = kind;
    task->released[task->releases] = handle;
    task->releases++;
}

static const struct child *child_of(const struct task_fixture *task, uint64_t pid)
{
    for (unsigned i = 0; i < task->child_count; i++) {
        if (task->children[i].pid == pid)
            return &task->children[i];
    }
    CHECK(false);
    return NULL;
}

static const struct pm_process *zombie_of(const struct pm *pm, uint64_t pid)
{
    for (unsigned i = 0; i < PM_CAPACITY; i++) {
        const struct pm_process *p = &pm->processes[i];
        if (p->state == PM_ZOMBIE && p->pid == pid)
            return p;
    }
    return NULL;
}

static void fixture_cases(void)
{
    struct pm pm;
    struct task_fixture task = {.pm = &pm, .parent = 100};
    const struct pm_port port = {&task, create_as, create_caps, create_thread, start, release};
    uint64_t pid1 = 0, pid2 = 0, pid3 = 0, pid4 = 0, pid5 = 0, spare = 0;
    int status = 99;

    pm_init(&pm, &port);
    CHECK(pm.last_pid == 0);

    /* 参数错误不触碰资源提供者。 */
    CHECK(pm_spawn(&pm, 0, 7, 3, &spare) == PM_BAD_ARG);
    CHECK(pm_spawn(&pm, task.parent, 0, 3, &spare) == PM_BAD_ARG);
    CHECK(pm_spawn(&pm, task.parent, 7, 3, NULL) == PM_BAD_ARG);
    CHECK(pm_wait(&pm, task.parent, 1, NULL) == PM_BAD_ARG);
    CHECK(task.starts == 0 && task.releases == 0 && pm.last_pid == 0);

    /* 能力空间创建失败：只回滚已取得的地址空间。 */
    task.fail_caps = true;
    CHECK(pm_spawn(&pm, task.parent, 7, 3, &pid1) == PM_NO_SPACE);
    CHECK(pid1 == 0 && pm.last_pid == 0 && task.starts == 0);
    CHECK(task.releases == 1 && task.order[0] == TASK_AS);
    CHECK(task.as.count == 0 && task.caps.count == 0);
    task.fail_caps = false;

    /* 线程创建失败：能力空间、地址空间按创建逆序回滚。 */
    task.fail_thread = true;
    CHECK(pm_spawn(&pm, task.parent, 7, 3, &pid1) == PM_NO_SPACE);
    CHECK(pid1 == 0 && pm.last_pid == 0 && task.starts == 0);
    CHECK(task.releases == 3 && task.order[1] == TASK_CAPS && task.order[2] == TASK_AS);
    CHECK(task.as.count == 0 && task.caps.count == 0 && task.thread.count == 0);
    task.fail_thread = false;

    /* 同一父进程启动两个单线程子进程：发布先于放行，PID 单调。 */
    CHECK(pm_spawn(&pm, task.parent, 7, 3, &pid1) == PM_OK && pid1 == 1);
    CHECK(pm_spawn(&pm, task.parent, 7, 3, &pid2) == PM_OK && pid2 == 2);
    CHECK(task.starts == 2 && task.child_count == 2);
    CHECK(task.as.count == 2 && task.caps.count == 2 && task.thread.count == 2);
    CHECK(pm_wait(&pm, task.parent, pid1, &status) == PM_RUNNING && status == 99);
    CHECK(pm_wait(&pm, task.parent, pid2, &status) == PM_RUNNING && status == 99);
    CHECK(pm_wait(&pm, task.parent, 99, &status) == PM_NOT_FOUND && status == 99);

    /* 子进程退出：三个资源立即交还，ZOMBIE 只保留退出码。 */
    const struct child *first = child_of(&task, pid1);
    const struct child *second = child_of(&task, pid2);
    CHECK(pm_on_exit(&pm, first->thread, 42));
    CHECK(!pm_on_exit(&pm, first->thread, 43));
    CHECK(task.releases == 6);
    CHECK(task.order[3] == TASK_THREAD && task.order[4] == TASK_CAPS &&
          task.order[5] == TASK_AS);
    CHECK(task.released[3] == first->thread && task.released[4] == first->caps &&
          task.released[5] == first->as);
    const struct pm_process *zombie = zombie_of(&pm, pid1);
    CHECK(zombie && zombie->exit_code == 42);
    CHECK(!zombie->as && !zombie->caps && !zombie->thread);
    CHECK(!provider_has(&task.as, first->as) && !provider_has(&task.caps, first->caps) &&
          !provider_has(&task.thread, first->thread));
    CHECK(provider_has(&task.as, second->as) && provider_has(&task.caps, second->caps) &&
          provider_has(&task.thread, second->thread));
    CHECK(task.as.count == 1 && task.caps.count == 1 && task.thread.count == 1);

    /* 退出码只交付一次：错误父进程不消耗，重复 wait 不再命中。 */
    CHECK(pm_wait(&pm, task.parent + 1, pid1, &status) == PM_DENIED && status == 99);
    CHECK(pm_wait(&pm, task.parent, pid1, &status) == PM_OK && status == 42);
    CHECK(pm_wait(&pm, task.parent, pid1, &status) == PM_NOT_FOUND && status == 42);
    CHECK(task.releases == 6);

    /* 空槽复用后 PID 继续递增，存活子进程不受影响。 */
    CHECK(pm_spawn(&pm, task.parent, 7, 3, &pid3) == PM_OK && pid3 == 3);
    CHECK(pm_wait(&pm, task.parent, pid2, &status) == PM_RUNNING);

    /* 填满 4 个槽位；额外 spawn 在触碰资源提供者之前失败。 */
    CHECK(pm_spawn(&pm, task.parent, 7, 3, &pid4) == PM_OK && pid4 == 4);
    CHECK(pm_spawn(&pm, task.parent, 7, 3, &pid5) == PM_OK && pid5 == 5);
    unsigned starts = task.starts;
    CHECK(pm_spawn(&pm, task.parent, 7, 3, &spare) == PM_NO_SPACE);
    CHECK(spare == 0 && task.starts == starts);
    CHECK(task.as.count == 4 && task.caps.count == 4 && task.thread.count == 4);

    /* 未知线程与已回收进程的退出确认都无效。 */
    CHECK(!pm_on_exit(&pm, 0x999, 7));
    CHECK(!pm_on_exit(&pm, first->thread, 7));

    /* 清理全部子进程：每个退出码各交付一次，资源计数归零。 */
    const struct child *children[] = {
        child_of(&task, pid2), child_of(&task, pid3),
        child_of(&task, pid4), child_of(&task, pid5),
    };
    const int codes[] = {-1, 0, 7, 255};
    for (unsigned i = 0; i < 4; i++) {
        CHECK(pm_on_exit(&pm, children[i]->thread, codes[i]));
        CHECK(pm_wait(&pm, task.parent, children[i]->pid, &status) == PM_OK &&
              status == codes[i]);
        CHECK(pm_wait(&pm, task.parent, children[i]->pid, &status) == PM_NOT_FOUND);
    }
    CHECK(task.releases == 18);
    CHECK(task.as.count == 0 && task.caps.count == 0 && task.thread.count == 0);
    for (unsigned i = 0; i < PM_CAPACITY; i++)
        CHECK(pm.processes[i].state == PM_EMPTY);

    /* PID 空间耗尽时同样不分配资源。 */
    pm.last_pid = UINT64_MAX;
    starts = task.starts;
    CHECK(pm_spawn(&pm, task.parent, 7, 3, &spare) == PM_NO_SPACE);
    CHECK(spare == 0 && task.starts == starts && task.as.count == 0);

    demo_log("process: rollback, multiple children, capacity and one-shot wait");
}

/* ---- kernel 端口：把 pm_port 绑到 memory/sched/capability ---------- */

/*
 * 第一段用替身注入失败，这一段用真实模块验证交付语义：
 *   as    -> kernel/memory 的 as_create/as_unref
 *   caps  -> kernel/capability 的 cap_root_install/cap_delete
 *   thread-> kernel/sched 的 thread_create/thread_exit
 *
 * 子线程只在内核地址空间运行（thread_create 传 as=NULL），否则需要
 * 先由 vm 模块建立完整的用户页表；端口本身不关心特权级。
 */

static _Alignas(16) uint8_t kernel_stacks[PM_CAPACITY][4096];
static _Alignas(PAGE_SIZE) uint8_t kernel_frames[8 * PAGE_SIZE];

struct kernel_fixture {
    struct as *spaces[PM_CAPACITY];
    struct thread *threads[PM_CAPACITY];
    uint32_t as_handles[PM_CAPACITY];
    uint32_t caps_handles[PM_CAPACITY];
    uint32_t thread_handles[PM_CAPACITY];
    cap_sid_t sids[PM_CAPACITY];
    unsigned spawns, pending;
    unsigned entered, starts;
    unsigned as_releases, caps_releases, thread_releases;
};

static int kernel_cap_invoke(struct cap_object *obj, uint32_t op, void *args)
{
    (void)obj;
    (void)op;
    (void)args;
    return CAP_OK;
}

static void kernel_cap_finalize(struct cap_object *obj)
{
    (void)obj;
}

static const struct cap_object_ops kernel_cap_ops = {
    .invoke = kernel_cap_invoke,
    .finalize = kernel_cap_finalize,
};

static const struct cap_type kernel_cap_type = {
    .type = 1,
    .rights_mask = CAP_RIGHT_ALL,
    .supported = 1u << CAP_OP_READ,
    .required = {CAP_RIGHT_READ, 0, 0, 0, 0, 0},
    .ops = &kernel_cap_ops,
};

static void kernel_child(void *argument)
{
    struct kernel_fixture *task = argument;
    task->entered++;
}

static uint32_t kernel_as_create(void *ctx, uint32_t image)
{
    struct kernel_fixture *task = ctx;
    CHECK(image != 0 && task->spawns < PM_CAPACITY);
    unsigned slot = task->spawns++;
    task->pending = slot;

    struct as *space = as_create();
    if (!space)
        return 0;
    task->spaces[slot] = space;
    task->as_handles[slot] = slot + 1;
    return task->as_handles[slot];
}

static uint32_t kernel_caps_create(void *ctx, uint32_t grants)
{
    struct kernel_fixture *task = ctx;
    unsigned slot = task->pending;
    cap_rights_t rights = (cap_rights_t)(grants & CAP_RIGHT_ALL);
    CHECK(rights != 0);

    cap_oid_t oid = 0;
    if (cap_object_create(&kernel_cap_type, NULL, &oid) != CAP_OK)
        return 0;

    cap_handle_t handle = 0;
    if (cap_root_install((cap_sid_t)slot, (cap_slot_t)(slot + 1), oid, rights, 0,
                         &handle) != CAP_OK) {
        CHECK(cap_object_discard(oid) == CAP_OK);
        return 0;
    }
    task->sids[slot] = (cap_sid_t)slot;
    task->caps_handles[slot] = handle;
    return handle;
}

static uint32_t kernel_thread_create(void *ctx, uint32_t as, uint32_t caps)
{
    struct kernel_fixture *task = ctx;
    unsigned slot = task->pending;
    CHECK(as == task->as_handles[slot] && caps == task->caps_handles[slot]);

    struct thread *thread = thread_create(NULL, kernel_child, task, kernel_stacks[slot],
                                          sizeof(kernel_stacks[slot]), "pm-child");
    if (!thread)
        return 0;
    task->threads[slot] = thread;
    task->thread_handles[slot] = slot + 1;
    return task->thread_handles[slot];
}

static void kernel_start(void *ctx, uint32_t thread)
{
    struct kernel_fixture *task = ctx;
    unsigned slot = task->pending;
    unsigned entered = task->entered;

    CHECK(thread == task->thread_handles[slot]);
    /* 放行后子线程运行到 thread_exit，再切回当前执行流。 */
    thread_yield();
    CHECK(task->entered == entered + 1);
    CHECK(task->threads[slot] != NULL && task->threads[slot]->state == THREAD_DEAD);
    task->starts++;
}

static void kernel_release(void *ctx, enum task_resource kind, uint32_t handle)
{
    struct kernel_fixture *task = ctx;

    for (unsigned slot = 0; slot < PM_CAPACITY; slot++) {
        if (kind == TASK_AS && task->as_handles[slot] == handle) {
            as_unref(task->spaces[slot]);
            task->spaces[slot] = NULL;
            task->as_handles[slot] = 0;
            task->as_releases++;
            return;
        }
        if (kind == TASK_CAPS && task->caps_handles[slot] == handle) {
            CHECK(cap_delete(task->sids[slot], handle) == CAP_OK);
            task->caps_handles[slot] = 0;
            task->caps_releases++;
            return;
        }
        if (kind == TASK_THREAD && task->thread_handles[slot] == handle) {
            /* 线程由 sched 在下一个安全点回收；PM 只撤销自己的记录。 */
            CHECK(task->threads[slot] != NULL &&
                  task->threads[slot]->state == THREAD_DEAD);
            task->thread_handles[slot] = 0;
            task->thread_releases++;
            return;
        }
    }
    CHECK(false);
}

static void kernel_cases(void)
{
    object_system_init();
    sched_init();
    cap_system_init();
    CHECK(frame_init((paddr_t)(uintptr_t)kernel_frames, sizeof(kernel_frames),
                     kernel_frames) == MEM_OK);
    uint64_t free_start = frame_free_count();

    static struct thread runner;
    runner = (struct thread){.name = "pm-runner"};
    sched_bootstrap(&runner);

    struct pm pm;
    struct kernel_fixture task = {0};
    const struct pm_port port = {
        &task, kernel_as_create, kernel_caps_create, kernel_thread_create,
        kernel_start, kernel_release,
    };
    pm_init(&pm, &port);

    uint64_t pid1 = 0, pid2 = 0;
    int status = 99;

    CHECK(pm_spawn(&pm, 200, 1, CAP_RIGHT_READ, &pid1) == PM_OK && pid1 == 1);
    CHECK(task.starts == 1 && task.entered == 1);
    CHECK(pm_wait(&pm, 200, pid1, &status) == PM_RUNNING && status == 99);

    /* 子线程已运行结束；退出确认交还真实 as/cap/thread。 */
    CHECK(pm_on_exit(&pm, task.thread_handles[0], 23));
    CHECK(task.thread_releases == 1 && task.caps_releases == 1 && task.as_releases == 1);
    CHECK(cap_live_caps() == 0 && cap_live_objects() == 0);
    CHECK(frame_free_count() == free_start);
    CHECK(!task.threads[0]->reaped);

    /* 下一次调度在安全点回收 DEAD 线程，PM 不再持有它。 */
    thread_yield();
    CHECK(task.threads[0]->reaped && !task.threads[0]->used);
    CHECK(pm_wait(&pm, 200, pid1, &status) == PM_OK && status == 23);
    CHECK(sched_ready_count() == 0);

    CHECK(pm_spawn(&pm, 200, 1, CAP_RIGHT_READ, &pid2) == PM_OK && pid2 == 2);
    CHECK(task.starts == 2 && task.entered == 2);
    CHECK(pm_on_exit(&pm, task.thread_handles[1], -7));
    thread_yield();
    CHECK(pm_wait(&pm, 200, pid2, &status) == PM_OK && status == -7);
    CHECK(task.as_releases == 2 && task.caps_releases == 2 && task.thread_releases == 2);
    CHECK(cap_live_caps() == 0 && cap_live_objects() == 0);
    CHECK(cap_check_invariants() == CAP_OK);
    CHECK(frame_free_count() == free_start);
    CHECK(sched_ready_count() == 0);

    sched_init();
    object_system_init();
    demo_log("process: pm_port bound to kernel address space, thread and caps");
}

static void run(void)
{
    fixture_cases();
    kernel_cases();
}

DEMO_MODULE(pm, false, run, NULL);
