#include "sm.h"
#include "demo.h"

static bool valid_name(const char *name, size_t length)
{
    return name && length > 0 && length <= SM_NAME_MAX;
}

static struct sm_binding *find(struct sm *sm, const char *name, size_t length)
{
    for (unsigned i = 0; i < SM_CAPACITY; i++) {
        if (sm->names[i].length == length && !memcmp(sm->names[i].name, name, length))
            return &sm->names[i];
    }
    return NULL;
}

enum sm_result sm_register(struct sm *sm, uint32_t owner, const char *name,
                           size_t length, uint32_t handle)
{
    if (!valid_name(name, length))
        return SM_BAD_NAME;
    if (find(sm, name, length))
        return SM_EXISTS;
    for (unsigned i = 0; i < SM_CAPACITY; i++) {
        struct sm_binding *entry = &sm->names[i];
        if (entry->length)
            continue;
        uint32_t endpoint;
        enum sm_result result = sm->port->retain(sm->port->ctx, owner, handle, &endpoint);
        if (result != SM_OK)
            return result;
        *entry = (struct sm_binding){.owner = owner, .endpoint = endpoint, .length = length};
        memcpy(entry->name, name, length);
        return SM_OK;
    }
    return SM_NO_SPACE;
}

enum sm_result sm_lookup(struct sm *sm, uint32_t client, const char *name,
                         size_t length, uint32_t *handle)
{
    if (!handle)
        return SM_BAD_ARG;
    if (!valid_name(name, length))
        return SM_BAD_NAME;
    struct sm_binding *entry = find(sm, name, length);
    if (!entry)
        return SM_NOT_FOUND;
    return sm->port->grant_send(sm->port->ctx, client, entry->endpoint, handle);
}

enum sm_result sm_unregister(struct sm *sm, uint32_t owner, const char *name, size_t length)
{
    if (!valid_name(name, length))
        return SM_BAD_NAME;
    struct sm_binding *entry = find(sm, name, length);
    if (!entry)
        return SM_NOT_FOUND;
    if (entry->owner != owner)
        return SM_DENIED;
    uint32_t endpoint = entry->endpoint;
    *entry = (struct sm_binding){0};
    sm->port->release(sm->port->ctx, endpoint);
    return SM_OK;
}
