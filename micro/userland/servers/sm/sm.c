#include "sm.h"
#include "demo.h"

/*
 * 名称是长度有界、区分大小写、非空的字节串。
 * 不引入 NUL 终止约定：长度就是名称的一部分，比较逐字节进行。
 */
static bool name_valid(const char *name, size_t length)
{
    return name && length > 0 && length <= SM_NAME_MAX;
}

static struct sm_binding *find(struct sm *sm, const char *name, size_t length)
{
    for (unsigned i = 0; i < SM_CAPACITY; i++) {
        struct sm_binding *binding = &sm->names[i];
        if (binding->state != SM_REGISTERED || binding->length != length)
            continue;
        if (!memcmp(binding->name, name, length))
            return binding;
    }
    return NULL;
}

static struct sm_binding *free_binding(struct sm *sm)
{
    for (unsigned i = 0; i < SM_CAPACITY; i++) {
        if (sm->names[i].state == SM_UNREGISTERED)
            return &sm->names[i];
    }
    return NULL;
}

enum sm_result sm_register(struct sm *sm, uint32_t owner, const char *name,
                           size_t length, uint32_t handle)
{
    if (!sm || !sm->port)
        return SM_BAD_ARG;
    if (!name_valid(name, length))
        return SM_BAD_NAME;
    if (find(sm, name, length))
        return SM_EXISTS;

    struct sm_binding *binding = free_binding(sm);
    if (!binding)
        return SM_NO_SPACE;

    /*
     * 先向能力层申请引用，成功后才写名称表。
     * retain 失败时表项保持未注册，不留下半个绑定。
     */
    uint32_t endpoint;
    enum sm_result result = sm->port->retain(sm->port->ctx, owner, handle, &endpoint);
    if (result != SM_OK)
        return result;

    *binding = (struct sm_binding){
        .state = SM_REGISTERED,
        .owner = owner,
        .endpoint = endpoint,
        .length = length,
    };
    memcpy(binding->name, name, length);
    return SM_OK;
}

enum sm_result sm_lookup(struct sm *sm, uint32_t client, const char *name,
                         size_t length, uint32_t *handle)
{
    if (!sm || !sm->port || !handle)
        return SM_BAD_ARG;
    if (!name_valid(name, length))
        return SM_BAD_NAME;

    struct sm_binding *binding = find(sm, name, length);
    if (!binding)
        return SM_NOT_FOUND;

    /*
     * 发放失败（越权或客户端能力槽位耗尽）时只返回错误：
     * 绑定保持不变，调用者的输出句柄也不被改写。
     */
    uint32_t granted;
    enum sm_result result = sm->port->grant_send(sm->port->ctx, client,
                                                 binding->endpoint, &granted);
    if (result == SM_OK)
        *handle = granted;
    return result;
}

enum sm_result sm_unregister(struct sm *sm, uint32_t owner, const char *name,
                             size_t length)
{
    if (!sm || !sm->port)
        return SM_BAD_ARG;
    if (!name_valid(name, length))
        return SM_BAD_NAME;

    struct sm_binding *binding = find(sm, name, length);
    if (!binding)
        return SM_NOT_FOUND;
    if (binding->owner != owner)
        return SM_DENIED;

    uint32_t endpoint = binding->endpoint;
    *binding = (struct sm_binding){0};
    sm->port->release(sm->port->ctx, endpoint);
    return SM_OK;
}

unsigned sm_owner_exit(struct sm *sm, uint32_t owner)
{
    if (!sm || !sm->port)
        return 0;

    unsigned released = 0;
    for (unsigned i = 0; i < SM_CAPACITY; i++) {
        struct sm_binding *binding = &sm->names[i];
        if (binding->state != SM_REGISTERED || binding->owner != owner)
            continue;
        uint32_t endpoint = binding->endpoint;
        *binding = (struct sm_binding){0};
        sm->port->release(sm->port->ctx, endpoint);
        released++;
    }
    return released;
}

unsigned sm_count(const struct sm *sm)
{
    if (!sm)
        return 0;

    unsigned count = 0;
    for (unsigned i = 0; i < SM_CAPACITY; i++) {
        if (sm->names[i].state == SM_REGISTERED)
            count++;
    }
    return count;
}
