#include "pm.h"
#include "demo.h"

struct task_fixture {
    struct pm *pm;
    bool live[3], fail_thread;
    unsigned starts, releases;
    enum task_resource order[6];
};

static uint32_t create_as(void *ctx, uint32_t image)
{
    struct task_fixture *task = ctx;
    CHECK(image == 7 && !task->live[TASK_AS]);
    task->live[TASK_AS] = true;
    return 20;
}

static uint32_t create_caps(void *ctx, uint32_t grants)
{
    struct task_fixture *task = ctx;
    CHECK(grants == 3 && !task->live[TASK_CAPS]);
    task->live[TASK_CAPS] = true;
    return 21;
}

static uint32_t create_thread(void *ctx, uint32_t as, uint32_t caps)
{
    struct task_fixture *task = ctx;
    CHECK(as == 20 && caps == 21 && !task->live[TASK_THREAD]);
    if (task->fail_thread)
        return 0;
    task->live[TASK_THREAD] = true;
    return 22;
}

static void start(void *ctx, uint32_t thread)
{
    struct task_fixture *task = ctx;
    int status;
    CHECK(thread == 22 && pm_wait(task->pm, 100, task->pm->last_pid, &status) == PM_RUNNING);
    task->starts++;
}

static void release(void *ctx, enum task_resource kind, uint32_t handle)
{
    struct task_fixture *task = ctx;
    CHECK(task->live[kind] && handle == 20u + kind && task->releases < 6);
    task->live[kind] = false;
    task->order[task->releases++] = kind;
}

static void run(void)
{
    struct pm pm = {0};
    struct task_fixture task = {.pm = &pm, .fail_thread = true};
    const struct pm_port port = {&task, create_as, create_caps, create_thread, start, release};
    pm.port = &port;
    uint64_t pid = 0;
    int status = 99;

    CHECK(pm_spawn(&pm, 100, 7, 3, &pid) == PM_NO_SPACE && pid == 0 && pm.last_pid == 0);
    CHECK(task.releases == 2 && task.order[0] == TASK_CAPS && task.order[1] == TASK_AS);
    task.fail_thread = false;
    CHECK(pm_spawn(&pm, 100, 7, 3, &pid) == PM_OK && pid != 0 && task.starts == 1);
    CHECK(pm_wait(&pm, 100, pid, &status) == PM_RUNNING && status == 99);
    CHECK(pm_on_exit(&pm, 22, 42) && !pm_on_exit(&pm, 22, 43));
    CHECK(task.releases == 5 && task.order[2] == TASK_THREAD);
    CHECK(!task.live[TASK_THREAD] && !task.live[TASK_AS] && !task.live[TASK_CAPS]);
    CHECK(pm_wait(&pm, 101, pid, &status) == PM_DENIED && status == 99);
    CHECK(pm_wait(&pm, 100, pid, &status) == PM_OK && status == 42);
    CHECK(pm_wait(&pm, 100, pid, &status) == PM_NOT_FOUND);
    demo_log("process: transactional spawn, exit cleanup and one wait by the parent");
}

DEMO_MODULE(pm, true, run, NULL);
