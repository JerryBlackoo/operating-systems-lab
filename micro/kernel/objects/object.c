#include "object.h"

#include <stdint.h>

static struct object *slots[OBJECT_MAX_SLOTS];
static uint16_t generations[OBJECT_MAX_SLOTS];

static object_lock_fn lock_hook;
static object_unlock_fn unlock_hook;

static uintptr_t registry_lock(void)
{
    return lock_hook ? lock_hook() : 0;
}

static void registry_unlock(uintptr_t flags)
{
    if (unlock_hook) {
        unlock_hook(flags);
    }
}

static uint32_t make_id(uint16_t slot, uint16_t gen)
{
    return ((uint32_t)gen << 16) | (uint32_t)slot;
}

void object_system_init(void)
{
    for (uint32_t i = 0; i < OBJECT_MAX_SLOTS; i++) {
        slots[i] = 0;
        generations[i] = 0;
    }
}

void object_set_lock(object_lock_fn lock, object_unlock_fn unlock)
{
    lock_hook = lock;
    unlock_hook = unlock;
}

int object_register(struct object *obj, enum object_type type,
                    object_destroy_fn destroy)
{
    uintptr_t flags;
    int slot = -1;

    if (obj == 0 || destroy == 0) {
        return -1;
    }
    if (type <= OBJ_NONE || type >= OBJ_TYPE_MAX) {
        return -1;
    }

    flags = registry_lock();
    for (int i = 0; i < OBJECT_MAX_SLOTS; i++) {
        if (slots[i] == 0) {
            slot = i;
            break;
        }
    }
    if (slot < 0) {
        registry_unlock(flags);
        return -1;
    }

    /* 代际从 1 开始递增；回绕时跳过 0，保证 id 不等于 0。 */
    generations[slot]++;
    if (generations[slot] == 0) {
        generations[slot] = 1;
    }

    obj->type = type;
    obj->refcnt = 1;
    obj->slot = (uint16_t)slot;
    obj->gen = generations[slot];
    obj->id = make_id(obj->slot, obj->gen);
    obj->destroy = destroy;
    slots[slot] = obj;

    registry_unlock(flags);
    return 0;
}

void object_ref(struct object *obj)
{
    uint32_t old;

    if (obj == 0) {
        return;
    }

    /*
     * 如果 refcnt 已经为 0，说明对象正在/已经析构，禁止复活。
     * 单核模型用 CAS 循环即可；SMP 时也保持同样语义。
     */
    do {
        old = __atomic_load_n(&obj->refcnt, __ATOMIC_RELAXED);
        if (old == 0) {
            return;
        }
    } while (!__atomic_compare_exchange_n(&obj->refcnt, &old, old + 1, false,
                                          __ATOMIC_ACQ_REL,
                                          __ATOMIC_RELAXED));
}

static void object_release(struct object *obj)
{
    uintptr_t flags;

    flags = registry_lock();
    if (obj->slot < OBJECT_MAX_SLOTS && slots[obj->slot] == obj) {
        slots[obj->slot] = 0;
    }
    registry_unlock(flags);

    /* 析构在锁外执行，允许回调释放类型自己的池。 */
    if (obj->destroy) {
        obj->destroy(obj);
    }
}

void object_unref(struct object *obj)
{
    uint32_t old;

    if (obj == 0) {
        return;
    }

    do {
        old = __atomic_load_n(&obj->refcnt, __ATOMIC_RELAXED);
        if (old == 0) {
            return;
        }
    } while (!__atomic_compare_exchange_n(&obj->refcnt, &old, old - 1, false,
                                          __ATOMIC_ACQ_REL,
                                          __ATOMIC_RELAXED));

    if (old == 1) {
        object_release(obj);
    }
}

struct object *object_lookup(uint32_t id, enum object_type type)
{
    uintptr_t flags;
    uint32_t slot = id & 0xffffu;
    struct object *obj;

    if (slot >= OBJECT_MAX_SLOTS) {
        return 0;
    }

    flags = registry_lock();
    obj = slots[slot];
    if (obj == 0 || obj->id != id) {
        registry_unlock(flags);
        return 0;
    }
    if (type != OBJ_NONE && obj->type != type) {
        registry_unlock(flags);
        return 0;
    }
    object_ref(obj);
    registry_unlock(flags);
    return obj;
}

enum object_type object_get_type(const struct object *obj)
{
    return obj ? obj->type : OBJ_NONE;
}

uint32_t object_get_id(const struct object *obj)
{
    return obj ? obj->id : 0;
}

uint32_t object_refcount(const struct object *obj)
{
    return obj ? __atomic_load_n(&obj->refcnt, __ATOMIC_RELAXED) : 0;
}

bool object_alive(const struct object *obj)
{
    return object_refcount(obj) != 0;
}
