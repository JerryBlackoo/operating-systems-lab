#include <stddef.h>
#include "pm.h"

enum pm_result pm_spawn(struct pm *pm, uint64_t parent, uint32_t image, uint32_t grants,
                        uint64_t *pid)
{
    if (!parent || !image || !pid)
        return PM_BAD_ARG;
    struct pm_process *slot = NULL;
    for (unsigned i = 0; i < PM_CAPACITY; i++) {
        if (pm->processes[i].state == PM_EMPTY) {
            slot = &pm->processes[i];
            break;
        }
    }
    if (!slot || pm->last_pid == UINT64_MAX)
        return PM_NO_SPACE;

    const struct pm_port *port = pm->port;
    uint32_t as = port->create_as(port->ctx, image);
    if (!as)
        return PM_NO_SPACE;
    uint32_t caps = port->create_caps(port->ctx, grants);
    if (!caps)
        goto release_as;
    uint32_t thread = port->create_thread(port->ctx, as, caps);
    if (!thread)
        goto release_caps;

    *slot = (struct pm_process){
        .state = PM_LIVE, .pid = ++pm->last_pid, .parent = parent,
        .as = as, .caps = caps, .thread = thread,
    };
    *pid = slot->pid;
    port->start(port->ctx, thread);
    return PM_OK;

release_caps:
    port->release(port->ctx, TASK_CAPS, caps);
release_as:
    port->release(port->ctx, TASK_AS, as);
    return PM_NO_SPACE;
}

bool pm_on_exit(struct pm *pm, uint32_t thread, int exit_code)
{
    for (unsigned i = 0; i < PM_CAPACITY; i++) {
        struct pm_process *p = &pm->processes[i];
        if (p->state != PM_LIVE || p->thread != thread)
            continue;
        const struct pm_port *port = pm->port;
        port->release(port->ctx, TASK_THREAD, p->thread);
        port->release(port->ctx, TASK_CAPS, p->caps);
        port->release(port->ctx, TASK_AS, p->as);
        p->thread = p->caps = p->as = 0;
        p->exit_code = exit_code;
        p->state = PM_ZOMBIE;
        return true;
    }
    return false;
}

enum pm_result pm_wait(struct pm *pm, uint64_t parent, uint64_t pid, int *exit_code)
{
    if (!exit_code)
        return PM_BAD_ARG;
    for (unsigned i = 0; i < PM_CAPACITY; i++) {
        struct pm_process *p = &pm->processes[i];
        if (p->state == PM_EMPTY || p->pid != pid)
            continue;
        if (p->parent != parent)
            return PM_DENIED;
        if (p->state == PM_LIVE)
            return PM_RUNNING;
        *exit_code = p->exit_code;
        *p = (struct pm_process){0};
        return PM_OK;
    }
    return PM_NOT_FOUND;
}
