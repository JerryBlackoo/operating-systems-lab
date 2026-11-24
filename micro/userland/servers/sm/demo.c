#include "sm.h"
#include "demo.h"

/*
 * 能力替身：两个发布端点、三个客户端能力表。
 *
 * 客户端句柄编码为 (generation << 8) | (slot + 1)，只在发放它的那张表里
 * 有效；槽位释放后 generation 递增，旧句柄失效。端点引用计数把注册项的
 * 引用和客户端句柄的引用分开计算。
 */
enum { EP_FS = 1, EP_LOG = 2, EP_COUNT = 2, CLIENT_COUNT = 3, CLIENT_SLOTS = 2 };

struct endpoint {
    uint32_t owner;
    uint32_t handle;
    bool alive;
    unsigned refs;
};

struct client_slot {
    bool used;
    uint32_t gen;
    uint32_t endpoint;
};

struct caps {
    struct endpoint endpoints[EP_COUNT];
    struct client_slot slots[CLIENT_COUNT][CLIENT_SLOTS];
    unsigned grants;
    unsigned releases;
};

static struct endpoint *endpoint_get(struct caps *caps, uint32_t endpoint)
{
    CHECK(endpoint >= 1 && endpoint <= EP_COUNT);
    struct endpoint *ep = &caps->endpoints[endpoint - 1];
    CHECK(ep->alive);
    return ep;
}

static uint32_t encode_handle(unsigned slot, uint32_t gen)
{
    return (gen << 8) | (slot + 1);
}

static bool decode_handle(uint32_t handle, unsigned *slot, uint32_t *gen)
{
    uint32_t low = handle & 0xffu;
    if (!low || low > (uint32_t)CLIENT_SLOTS)
        return false;
    *slot = low - 1;
    *gen = handle >> 8;
    return *gen != 0;
}

static struct client_slot *lookup_slot(struct caps *caps, uint32_t client,
                                       uint32_t handle)
{
    unsigned slot;
    uint32_t gen;
    if (!client || client > (uint32_t)CLIENT_COUNT ||
        !decode_handle(handle, &slot, &gen))
        return NULL;

    struct client_slot *entry = &caps->slots[client - 1][slot];
    if (!entry->used || entry->gen != gen)
        return NULL;
    return entry;
}

static enum sm_result retain(void *ctx, uint32_t owner, uint32_t handle,
                             uint32_t *endpoint)
{
    struct caps *caps = ctx;
    for (unsigned i = 0; i < EP_COUNT; i++) {
        struct endpoint *ep = &caps->endpoints[i];
        if (!ep->alive || ep->owner != owner || ep->handle != handle)
            continue;
        ep->refs++;
        *endpoint = i + 1;
        return SM_OK;
    }
    return SM_DENIED;
}

static enum sm_result grant_send(void *ctx, uint32_t client, uint32_t endpoint,
                                 uint32_t *handle)
{
    struct caps *caps = ctx;
    if (!client || client > (uint32_t)CLIENT_COUNT)
        return SM_DENIED;

    struct endpoint *ep = endpoint_get(caps, endpoint);
    for (unsigned i = 0; i < CLIENT_SLOTS; i++) {
        struct client_slot *slot = &caps->slots[client - 1][i];
        if (slot->used)
            continue;
        slot->used = true;
        slot->gen++;
        slot->endpoint = endpoint;
        ep->refs++;
        caps->grants++;
        *handle = encode_handle(i, slot->gen);
        return SM_OK;
    }
    return SM_NO_SPACE;
}

static void release(void *ctx, uint32_t endpoint)
{
    struct caps *caps = ctx;
    struct endpoint *ep = endpoint_get(caps, endpoint);
    CHECK(ep->refs > 0);
    ep->refs--;
    caps->releases++;
}

static bool can_send(struct caps *caps, uint32_t client, uint32_t handle,
                     uint32_t endpoint)
{
    struct client_slot *slot = lookup_slot(caps, client, handle);
    return slot && slot->endpoint == endpoint;
}

static bool drop_handle(struct caps *caps, uint32_t client, uint32_t handle)
{
    struct client_slot *slot = lookup_slot(caps, client, handle);
    if (!slot)
        return false;

    struct endpoint *ep = endpoint_get(caps, slot->endpoint);
    CHECK(ep->refs > 0);
    ep->refs--;
    slot->used = false;
    return true;
}

static void run(void)
{
    struct caps caps = {
        .endpoints = {
            {.owner = 10, .handle = 0x1001, .alive = true},
            {.owner = 20, .handle = 0x2002, .alive = true},
        },
    };
    const struct sm_port port = {&caps, retain, grant_send, release};
    struct sm sm = {.port = &port};
    char long_name[SM_NAME_MAX + 1];
    char max_name[SM_NAME_MAX];
    uint32_t untouched = 0xdeadbeef;
    uint32_t h1 = 0, h1b = 0, h1c = 0, h2fs = 0, h3 = 0, h3max = 0, hlog = 0;

    memset(long_name, 'x', sizeof(long_name));
    memset(max_name, 'm', sizeof(max_name));

    /* 名称必须非空且有界；失败不占槽位、不持引用。 */
    CHECK(sm_register(&sm, 10, NULL, 0, 0x1001) == SM_BAD_NAME);
    CHECK(sm_register(&sm, 10, "", 0, 0x1001) == SM_BAD_NAME);
    CHECK(sm_register(&sm, 10, long_name, sizeof(long_name), 0x1001) == SM_BAD_NAME);
    CHECK(sm_count(&sm) == 0 && caps.endpoints[0].refs == 0);

    /* 发布者身份来自请求上下文，且必须真的持有该端点句柄。 */
    CHECK(sm_register(&sm, 11, "fs", 2, 0x1001) == SM_DENIED);
    CHECK(sm_register(&sm, 10, "fs", 2, 0x2002) == SM_DENIED);
    CHECK(sm_count(&sm) == 0 && caps.endpoints[0].refs == 0);

    CHECK(sm_register(&sm, 10, "fs", 2, 0x1001) == SM_OK);
    CHECK(sm_count(&sm) == 1 && caps.endpoints[0].refs == 1);
    CHECK(sm_register(&sm, 10, "fs", 2, 0x1001) == SM_EXISTS);
    CHECK(sm_register(&sm, 20, "fs", 2, 0x2002) == SM_EXISTS);
    CHECK(caps.endpoints[0].refs == 1);

    /* 名称逐字节比较：大小写、长度、前缀都不可混用。 */
    CHECK(sm_lookup(&sm, 1, "FS", 2, &untouched) == SM_NOT_FOUND &&
          untouched == 0xdeadbeef);
    CHECK(sm_lookup(&sm, 1, "f", 1, &untouched) == SM_NOT_FOUND &&
          untouched == 0xdeadbeef);
    CHECK(sm_lookup(&sm, 1, "fsx", 3, &untouched) == SM_NOT_FOUND &&
          untouched == 0xdeadbeef);
    CHECK(sm_lookup(&sm, 1, "fs", 2, NULL) == SM_BAD_ARG);

    /* lookup 在调用者自己的能力空间里发放发送句柄。 */
    CHECK(sm_lookup(&sm, 1, "fs", 2, &h1) == SM_OK && caps.endpoints[0].refs == 2);
    CHECK(sm_lookup(&sm, 1, "fs", 2, &h1b) == SM_OK && h1b != h1);
    CHECK(caps.endpoints[0].refs == 3);
    CHECK(can_send(&caps, 1, h1, EP_FS) && can_send(&caps, 1, h1b, EP_FS));
    CHECK(!can_send(&caps, 2, h1, EP_FS) && !can_send(&caps, 1, h1, EP_LOG));

    /* 客户端槽位耗尽时失败是原子的：绑定和已有句柄都不动。 */
    CHECK(sm_lookup(&sm, 1, "fs", 2, &untouched) == SM_NO_SPACE);
    CHECK(untouched == 0xdeadbeef && caps.endpoints[0].refs == 3);
    CHECK(sm_lookup(&sm, 0, "fs", 2, &untouched) == SM_DENIED);
    CHECK(sm_lookup(&sm, 3, "fs", 2, &h3) == SM_OK);
    CHECK(can_send(&caps, 3, h3, EP_FS));

    /* 名称绑定到哪个端点，发放的句柄就只对哪个端点有效。 */
    CHECK(sm_register(&sm, 20, "log", 3, 0x2002) == SM_OK);
    CHECK(sm_count(&sm) == 2 && caps.endpoints[1].refs == 1);
    CHECK(sm_lookup(&sm, 2, "log", 3, &hlog) == SM_OK);
    CHECK(can_send(&caps, 2, hlog, EP_LOG) && !can_send(&caps, 2, hlog, EP_FS));

    /* 只有 owner 能注销；被拒绝的注销不影响绑定。 */
    CHECK(sm_unregister(&sm, 11, "fs", 2) == SM_DENIED);
    CHECK(sm_lookup(&sm, 2, "fs", 2, &h2fs) == SM_OK && caps.endpoints[0].refs == 5);
    CHECK(sm_unregister(&sm, 10, "fs", 2) == SM_OK);
    CHECK(sm_count(&sm) == 1 && caps.endpoints[0].refs == 4);
    CHECK(sm_lookup(&sm, 1, "fs", 2, &untouched) == SM_NOT_FOUND &&
          untouched == 0xdeadbeef);
    CHECK(sm_unregister(&sm, 10, "fs", 2) == SM_NOT_FOUND);

    /* 已发放句柄的寿命由客户端能力表管理，可以长于名称。 */
    CHECK(can_send(&caps, 1, h1, EP_FS) && can_send(&caps, 1, h1b, EP_FS));
    CHECK(can_send(&caps, 2, h2fs, EP_FS) && can_send(&caps, 3, h3, EP_FS));

    /* 注销后名称可以重新注册。 */
    CHECK(sm_register(&sm, 10, "fs", 2, 0x1001) == SM_OK);
    CHECK(sm_count(&sm) == 2 && caps.endpoints[0].refs == 5);

    /* 槽位复用后 generation 变化，旧句柄失效。 */
    CHECK(drop_handle(&caps, 1, h1) && caps.endpoints[0].refs == 4);
    CHECK(sm_lookup(&sm, 1, "fs", 2, &h1c) == SM_OK && h1c != h1);
    CHECK(caps.endpoints[0].refs == 5);
    CHECK(!can_send(&caps, 1, h1, EP_FS) && can_send(&caps, 1, h1c, EP_FS));

    /* 发布者退出：它名下的名称和注册项引用被整体回收。 */
    CHECK(sm_register(&sm, 20, "db", 2, 0x2002) == SM_OK);
    CHECK(sm_count(&sm) == 3 && caps.endpoints[1].refs == 3);
    CHECK(sm_owner_exit(&sm, 20) == 2);
    CHECK(sm_count(&sm) == 1 && caps.endpoints[1].refs == 1);
    CHECK(sm_lookup(&sm, 2, "log", 3, &untouched) == SM_NOT_FOUND &&
          untouched == 0xdeadbeef);
    CHECK(can_send(&caps, 2, hlog, EP_LOG));
    CHECK(sm_owner_exit(&sm, 20) == 0);

    /* 名称表满后拒绝新名称，且不多持端点引用。 */
    CHECK(sm_register(&sm, 10, "net", 3, 0x1001) == SM_OK);
    CHECK(sm_register(&sm, 10, max_name, SM_NAME_MAX, 0x1001) == SM_OK);
    CHECK(sm_register(&sm, 10, "blk", 3, 0x1001) == SM_OK);
    CHECK(sm_count(&sm) == SM_CAPACITY && caps.endpoints[0].refs == 8);
    CHECK(sm_register(&sm, 10, "extra", 5, 0x1001) == SM_NO_SPACE);
    CHECK(sm_count(&sm) == SM_CAPACITY && caps.endpoints[0].refs == 8);
    CHECK(sm_lookup(&sm, 3, max_name, SM_NAME_MAX, &h3max) == SM_OK);
    CHECK(can_send(&caps, 3, h3max, EP_FS));

    /* owner 退出一次回收全部注册项。 */
    CHECK(sm_owner_exit(&sm, 10) == 4);
    CHECK(sm_count(&sm) == 0 && caps.endpoints[0].refs == 5);
    CHECK(sm_lookup(&sm, 3, max_name, SM_NAME_MAX, &untouched) == SM_NOT_FOUND);

    /* 客户端释放全部句柄后，端点引用归零。 */
    CHECK(drop_handle(&caps, 1, h1b));
    CHECK(drop_handle(&caps, 1, h1c));
    CHECK(drop_handle(&caps, 2, h2fs));
    CHECK(drop_handle(&caps, 2, hlog));
    CHECK(drop_handle(&caps, 3, h3));
    CHECK(drop_handle(&caps, 3, h3max));
    CHECK(caps.endpoints[0].refs == 0 && caps.endpoints[1].refs == 0);
    CHECK(caps.releases == 7 && caps.grants == 7);

    demo_log("registry: bounded names, scoped send grants, owner exit cleanup");
}

DEMO_MODULE(sm, true, run, NULL);
