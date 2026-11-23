#include "init.h"

static void fail(struct init_model *init, uint32_t service, int status)
{
    init->phase = INIT_FAILED;
    init->failed_service = service;
    init->error = status;
    while (init->started_count) {
        struct init_child *child = &init->started[--init->started_count];
        init->port->stop(init->port->ctx, child->handle);
        *child = (struct init_child){0};
    }
}

static int start_next(struct init_model *init)
{
    size_t index = init->started_count;
    if (index == init->count) {
        init->phase = INIT_READY;
        return 0;
    }

    int status = init->port->start(init->port->ctx, &init->plan[index], &init->started[index]);
    if (status) {
        fail(init, init->plan[index].id, status);
        return status;
    }
    init->started_count++;
    return 0;
}

int init_boot(struct init_model *init, const struct init_port *port, uint32_t boot_caps,
              const struct init_service *plan, size_t count)
{
    if (init->phase != INIT_BOOT)
        return INIT_ALREADY_STARTED;
    init->port = port;
    if (count > INIT_CAPACITY || (count && !plan)) {
        fail(init, 0, INIT_BAD_PLAN);
        return INIT_BAD_PLAN;
    }
    for (size_t i = 0; i < count; i++) {
        if (!plan[i].id || (plan[i].grants & ~boot_caps)) {
            fail(init, plan[i].id, INIT_BAD_PLAN);
            return INIT_BAD_PLAN;
        }
        for (size_t earlier = 0; earlier < i; earlier++) {
            if (plan[earlier].id == plan[i].id) {
                fail(init, plan[i].id, INIT_BAD_PLAN);
                return INIT_BAD_PLAN;
            }
        }
        init->plan[i] = plan[i];
    }
    init->count = count;
    init->phase = INIT_STARTING;
    return start_next(init);
}

bool init_ready(struct init_model *init, uint64_t receipt)
{
    if (init->phase != INIT_STARTING || !init->started_count ||
        init->started[init->started_count - 1].ready_receipt != receipt)
        return false;
    start_next(init);
    return true;
}

bool init_on_exit(struct init_model *init, uint32_t child, int status)
{
    if (init->phase != INIT_STARTING)
        return false;
    for (size_t i = 0; i < init->started_count; i++) {
        if (init->started[i].handle == child) {
            fail(init, init->plan[i].id, status);
            return true;
        }
    }
    return false;
}
