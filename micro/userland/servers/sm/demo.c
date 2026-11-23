#include "sm.h"
#include "demo.h"

struct caps { uint32_t client_slot[2]; unsigned refs; };

static enum sm_result retain(void *ctx, uint32_t owner, uint32_t handle, uint32_t *ep)
{
    struct caps *caps = ctx;
    if (owner != 10 || handle != 0x1234)
        return SM_DENIED;
    *ep = 1;
    caps->refs++;
    return SM_OK;
}

static enum sm_result grant_send(void *ctx, uint32_t client, uint32_t ep, uint32_t *handle)
{
    struct caps *caps = ctx;
    CHECK(ep == 1);
    if (client > 2 || client == 0)
        return SM_DENIED;
    uint32_t *slot = &caps->client_slot[client - 1];
    if (*slot)
        return SM_NO_SPACE;
    *handle = *slot = client * 256 + 1;
    caps->refs++;
    return SM_OK;
}

static void release(void *ctx, uint32_t ep)
{
    struct caps *caps = ctx;
    CHECK(ep == 1 && caps->refs > 0);
    caps->refs--;
}

static void run(void)
{
    struct caps caps = {0};
    const struct sm_port port = {&caps, retain, grant_send, release};
    struct sm sm = {.port = &port};
    uint32_t sender1, sender2;

    CHECK(sm_register(&sm, 11, "fs", 2, 0x1234) == SM_DENIED && !caps.refs);
    CHECK(sm_register(&sm, 10, "fs", 2, 0x1234) == SM_OK && caps.refs == 1);
    CHECK(sm_register(&sm, 10, "fs", 2, 0x1234) == SM_EXISTS);
    CHECK(sm_lookup(&sm, 1, "FS", 2, &sender1) == SM_NOT_FOUND);
    CHECK(sm_lookup(&sm, 1, "fs", 2, &sender1) == SM_OK);
    CHECK(sm_lookup(&sm, 1, "fs", 2, &sender2) == SM_NO_SPACE && caps.refs == 2);
    CHECK(sm_lookup(&sm, 2, "fs", 2, &sender2) == SM_OK && caps.refs == 3);
    CHECK(sender1 != sender2 && caps.client_slot[0] == sender1 && caps.client_slot[1] == sender2);
    CHECK(sm_unregister(&sm, 11, "fs", 2) == SM_DENIED && caps.refs == 3);
    CHECK(sm_unregister(&sm, 10, "fs", 2) == SM_OK && caps.refs == 2);
    CHECK(sm_lookup(&sm, 1, "fs", 2, &sender1) == SM_NOT_FOUND);
    CHECK(caps.client_slot[0] == sender1 && caps.client_slot[1] == sender2);
    for (unsigned i = 0; i < 2; i++) {
        caps.client_slot[i] = 0;
        release(&caps, 1);
    }
    CHECK(caps.refs == 0);
    demo_log("registry: scoped send grants, duplicate names and reference cleanup");
}

DEMO_MODULE(sm, true, run, NULL);
