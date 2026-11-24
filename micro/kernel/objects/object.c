#include "object.h"

#include <stdint.h>

_Static_assert(OBJECT_MAX_SLOTS <= 0xffffu, "slot field of object id is 16 bits");

static struct object *slots[OBJECT_MAX_SLOTS];
static uint16_t generations[OBJECT_MAX_SLOTS];
static uint32_t live_objects;

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
    uintptr_t flags = registry_lock();

    for (uint32_t i = 0; i < OBJECT_MAX_SLOTS; i++) {
        slots[i] = 0;
    }
    live_objects = 0;
    registry_unlock(flags);

    /*
     * 代际计数器故意不在重置时清零：拿旧 id 的调用者可能在 init 之后
     * 才来查找，继续递增才能保证这些陈旧 id 永远不会命中新对象。
     */
}

void object_set_lock(object_lock_fn lock, object_unlock_fn unlock)
{
    lock_hook = lock;
    unlock_hook = unlock;
}

uintptr_t object_lock(void)
{
    return registry_lock();
}

void object_unlock(uintptr_t flags)
{
    registry_unlock(flags);
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

    /* 同一个对象头不能同时占据两个槽位：那会让旧 id 与析构都指向同一块内存。 */
    if (obj->slot < OBJECT_MAX_SLOTS && slots[obj->slot] == obj) {
        registry_unlock(flags);
        return -1;
    }

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
    live_objects++;

    registry_unlock(flags);
    return 0;
}

bool object_ref(struct object *obj)
{
    uint32_t old;

    if (obj == 0) {
        return false;
    }

    /*
     * 如果 refcnt 已经为 0，说明对象正在/已经析构，禁止复活。
     * 单核模型用 CAS 循环即可；SMP 时也保持同样语义。
     */
    do {
        old = __atomic_load_n(&obj->refcnt, __ATOMIC_RELAXED);
        if (old == 0) {
            return false;
        }
    } while (!__atomic_compare_exchange_n(&obj->refcnt, &old, old + 1, false,
                                          __ATOMIC_ACQ_REL,
                                          __ATOMIC_RELAXED));
    return true;
}

static void object_release(struct object *obj)
{
    uintptr_t flags;

    /*
     * 末引用在 CAS 中降为 0 之后、真正摘除槽位之前，lookup 可能看到
     * 一个仍然挂在表里但已无引用的对象。object_lookup 在锁内调用
     * object_ref，refcnt 为 0 就直接失败，因此这里只需保证摘除动作
     * 与 lookup 的“查表 + 加引用”互斥。
     */
    flags = registry_lock();
    if (obj->slot < OBJECT_MAX_SLOTS && slots[obj->slot] == obj) {
        slots[obj->slot] = 0;
        if (live_objects > 0) {
            live_objects--;
        }
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
    if (obj == 0 || obj->id != id ||
        (type != OBJ_NONE && obj->type != type) || !object_ref(obj)) {
        registry_unlock(flags);
        return 0;
    }
    registry_unlock(flags);
    return obj;
}

enum object_type object_get_type(const struct object *obj)
{
    return obj ? obj->type : OBJ_NONE;
}

const char *object_type_name(enum object_type type)
{
    switch (type) {
    case OBJ_NONE:     return "none";
    case OBJ_THREAD:   return "thread";
    case OBJ_AS:       return "as";
    case OBJ_ENDPOINT: return "endpoint";
    case OBJ_REPLY:    return "reply";
    default:           return "unknown";
    }
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

uint32_t object_live_count(void)
{
    return __atomic_load_n(&live_objects, __ATOMIC_RELAXED);
}
