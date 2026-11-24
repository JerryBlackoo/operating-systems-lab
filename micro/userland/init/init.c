#include "init.h"

static bool child_handle_used(const struct init_model *init, uint32_t handle,
                              size_t count)
{
    for (size_t i = 0; i < count; i++) {
        if (init->started[i].handle == handle)
            return true;
    }
    return false;
}

static bool child_receipt_used(const struct init_model *init, uint64_t receipt,
                               size_t count)
{
    for (size_t i = 0; i < count; i++) {
        if (init->started[i].ready_receipt == receipt)
            return true;
    }
    return false;
}

/*
 * 失败即封存：记录失败项与状态，按启动的逆序交还本轮取得的子服务。
 * 先改 phase 再调 stop，避免 stop 回调重入本模型。
 */
static void fail(struct init_model *init, uint32_t service, int status)
{
    init->phase = INIT_FAILED;
    init->failed_service = service;
    init->error = status;
    while (init->started_count) {
        struct init_child *child = &init->started[--init->started_count];
        if (child->handle && init->port && init->port->stop)
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

    struct init_child child = {0};
    int status = init->port->start(init->port->ctx, &init->plan[index], &child);
    if (status) {
        fail(init, init->plan[index].id, status);
        return status;
    }

    bool duplicate_handle = child_handle_used(init, child.handle, index);
    bool duplicate_receipt = child_receipt_used(init, child.ready_receipt, index);
    if (!child.handle || !child.ready_receipt || duplicate_handle || duplicate_receipt) {
        /*
         * 句柄重复时无法安全判断 stop 指向哪一项，交给端口自行回收；
         * 句柄唯一时它只可能属于本项，先停止它再逆序清理已确认的项。
         */
        if (child.handle && !duplicate_handle)
            init->port->stop(init->port->ctx, child.handle);
        fail(init, init->plan[index].id, INIT_BAD_START);
        return INIT_BAD_START;
    }

    init->started[index] = child;
    init->started_count++;
    return 0;
}

int init_boot(struct init_model *init, const struct init_port *port, uint32_t boot_caps,
              const struct init_service *plan, size_t count)
{
    if (init->phase != INIT_BOOT)
        return INIT_ALREADY_STARTED;
    if (!port || !port->start || (count && !port->stop)) {
        init->phase = INIT_FAILED;
        init->failed_service = 0;
        init->error = INIT_BAD_PORT;
        return INIT_BAD_PORT;
    }
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
    }
    for (size_t i = 0; i < count; i++)
        init->plan[i] = plan[i];
    init->count = count;
    init->phase = INIT_STARTING;
    return start_next(init);
}

bool init_ready(struct init_model *init, uint64_t receipt)
{
    if (init->phase != INIT_STARTING || !init->started_count || !receipt ||
        init->started[init->started_count - 1].ready_receipt != receipt)
        return false;
    start_next(init);
    return true;
}

bool init_on_exit(struct init_model *init, uint32_t child, int status)
{
    if (init->phase != INIT_STARTING || !child)
        return false;
    for (size_t i = 0; i < init->started_count; i++) {
        if (init->started[i].handle == child) {
            fail(init, init->plan[i].id, status);
            return true;
        }
    }
    return false;
}
