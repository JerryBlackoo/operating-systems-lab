#include "object.h"

#include <assert.h>
#include <stdio.h>
#include <string.h>

struct fake {
    struct object obj;
    int destroyed;
};

static struct fake fakes[OBJECT_MAX_SLOTS + 8];
static int destroyed_total;

static unsigned lock_calls;
static unsigned unlock_calls;

static uintptr_t counting_lock(void)
{
    lock_calls++;
    return 0xabcd;
}

static void counting_unlock(uintptr_t flags)
{
    assert(flags == 0xabcd);
    unlock_calls++;
}

static void fake_destroy(struct object *obj)
{
    struct fake *fake = (struct fake *)obj;

    fake->destroyed++;
    destroyed_total++;
}

static struct fake *fake_register(int index, enum object_type type)
{
    struct fake *fake = &fakes[index];

    fake->destroyed = 0;
    assert(object_register(&fake->obj, type, fake_destroy) == 0);
    assert(fake->obj.id != 0);
    assert(fake->obj.refcnt == 1);
    return fake;
}

static void test_lifecycle(void)
{
    struct fake *a;
    struct fake *b;
    struct object *found;
    uint32_t a_id;

    object_system_init();
    destroyed_total = 0;

    a = fake_register(0, OBJ_THREAD);
    b = fake_register(1, OBJ_AS);
    a_id = object_get_id(&a->obj);

    assert(object_get_type(&a->obj) == OBJ_THREAD);
    assert(object_get_type(&b->obj) == OBJ_AS);
    assert(object_get_id(&a->obj) != object_get_id(&b->obj));
    assert(object_alive(&a->obj));
    assert(object_live_count() == 2);
    assert(strcmp(object_type_name(OBJ_THREAD), "thread") == 0);
    assert(strcmp(object_type_name(OBJ_AS), "as") == 0);
    assert(strcmp(object_type_name(OBJ_ENDPOINT), "endpoint") == 0);
    assert(strcmp(object_type_name(OBJ_REPLY), "reply") == 0);
    assert(strcmp(object_type_name(OBJ_NONE), "none") == 0);
    assert(strcmp(object_type_name(OBJ_TYPE_MAX), "unknown") == 0);

    assert(object_ref(&a->obj));
    assert(object_refcount(&a->obj) == 2);
    object_unref(&a->obj);
    assert(object_refcount(&a->obj) == 1);

    found = object_lookup(a_id, OBJ_THREAD);
    assert(found == &a->obj);
    assert(object_refcount(&a->obj) == 2);
    object_unref(found);
    assert(object_refcount(&a->obj) == 1);

    assert(object_lookup(a_id, OBJ_AS) == 0);
    assert(object_lookup(a_id + 0x10000u, OBJ_NONE) == 0);

    object_unref(&a->obj);
    assert(destroyed_total == 1);
    assert(a->destroyed == 1);
    assert(!object_alive(&a->obj));
    assert(object_live_count() == 1);
    assert(object_lookup(a_id, OBJ_NONE) == 0);

    /* 对死亡对象 ref 不应复活。 */
    assert(!object_ref(&a->obj));
    assert(object_refcount(&a->obj) == 0);
    object_unref(&a->obj);
    assert(destroyed_total == 1);

    object_unref(&b->obj);
    assert(destroyed_total == 2);
    assert(object_live_count() == 0);

    printf("objects: lifecycle ok\n");
}

static void test_generation(void)
{
    struct fake *a;
    struct fake *b;
    struct object *found;
    uint32_t old_id;
    uint32_t new_id;

    object_system_init();
    destroyed_total = 0;

    a = fake_register(2, OBJ_ENDPOINT);
    old_id = object_get_id(&a->obj);
    object_unref(&a->obj);
    assert(destroyed_total == 1);

    b = fake_register(3, OBJ_ENDPOINT);
    new_id = object_get_id(&b->obj);
    assert(new_id != old_id);
    assert(object_lookup(old_id, OBJ_ENDPOINT) == 0);
    found = object_lookup(new_id, OBJ_ENDPOINT);
    assert(found == &b->obj);
    object_unref(found);

    object_unref(&b->obj);
    assert(destroyed_total == 2);

    printf("objects: generation ok\n");
}

static void test_lock_hook(void)
{
    struct fake *a;
    struct object *found;

    /* 先装钩子，再重置；计数从 init 之后开始。 */
    object_set_lock(counting_lock, counting_unlock);
    object_system_init();
    lock_calls = 0;
    unlock_calls = 0;
    destroyed_total = 0;

    a = fake_register(4, OBJ_THREAD);
    assert(lock_calls == 1 && unlock_calls == 1);

    found = object_lookup(object_get_id(&a->obj), OBJ_THREAD);
    assert(found == &a->obj);
    assert(lock_calls == 2 && unlock_calls == 2);

    /* 纯引用计数不碰注册表，不应额外取锁。 */
    assert(object_ref(&a->obj));
    assert(lock_calls == 2 && unlock_calls == 2);

    object_unref(found);   /* 归还 object_lookup 额外引用 */
    object_unref(&a->obj); /* 归还上面的 object_ref */
    object_unref(&a->obj); /* 归还 object_register 的初始引用 */
    assert(destroyed_total == 1);
    assert(lock_calls == 3 && unlock_calls == 3);

    object_set_lock(0, 0);
    printf("objects: lock hook ok\n");
}

static void test_duplicate_register(void)
{
    struct fake *a;
    uint32_t id;

    object_system_init();
    destroyed_total = 0;

    a = fake_register(5, OBJ_AS);
    id = object_get_id(&a->obj);
    assert(object_live_count() == 1);

    /* 同一个对象头不能重复注册，失败不得改写已有身份。 */
    assert(object_register(&a->obj, OBJ_ENDPOINT, fake_destroy) < 0);
    assert(object_get_type(&a->obj) == OBJ_AS);
    assert(object_get_id(&a->obj) == id);
    assert(object_refcount(&a->obj) == 1);
    assert(object_live_count() == 1);

    /* 非法类型或空析构在触碰对象前就被拒绝。 */
    assert(object_register(&fakes[6].obj, OBJ_NONE, fake_destroy) < 0);
    assert(object_register(&fakes[6].obj, OBJ_THREAD, 0) < 0);
    assert(object_live_count() == 1);

    object_unref(&a->obj);
    assert(destroyed_total == 1 && object_live_count() == 0);

    printf("objects: duplicate registration ok\n");
}

static void test_init_generation(void)
{
    struct fake *a = &fakes[7];
    struct object *found;
    uint32_t old_id;

    object_system_init();
    destroyed_total = 0;
    a->destroyed = 0;

    assert(object_register(&a->obj, OBJ_THREAD, fake_destroy) == 0);
    old_id = object_get_id(&a->obj);
    object_unref(&a->obj);
    assert(destroyed_total == 1);

    /* 重置注册表不得把代际归零，否则旧 id 会在同一槽位上复活。 */
    object_system_init();
    a->destroyed = 0;
    assert(object_register(&a->obj, OBJ_THREAD, fake_destroy) == 0);
    assert(object_get_id(&a->obj) != old_id);
    assert(object_lookup(old_id, OBJ_NONE) == 0);

    found = object_lookup(object_get_id(&a->obj), OBJ_THREAD);
    assert(found == &a->obj);
    object_unref(found);   /* object_lookup 额外引用 */
    object_unref(&a->obj); /* object_register 初始引用 */
    assert(destroyed_total == 2 && object_live_count() == 0);

    printf("objects: generation across reset ok\n");
}

static void test_capacity(void)
{
    struct fake *registered[OBJECT_MAX_SLOTS];
    int count = 0;
    struct fake overflow;

    object_system_init();
    destroyed_total = 0;

    for (int i = 0; i < OBJECT_MAX_SLOTS; i++) {
        struct fake *fake = &fakes[i];
        fake->destroyed = 0;
        assert(object_register(&fake->obj, OBJ_REPLY, fake_destroy) == 0);
        registered[count++] = fake;
    }

    assert(object_live_count() == OBJECT_MAX_SLOTS);

    overflow.destroyed = 0;
    assert(object_register(&overflow.obj, OBJ_REPLY, fake_destroy) < 0);
    assert(object_live_count() == OBJECT_MAX_SLOTS);

    for (int i = 0; i < count; i++) {
        object_unref(&registered[i]->obj);
    }
    assert(destroyed_total == OBJECT_MAX_SLOTS);
    assert(object_live_count() == 0);

    printf("objects: capacity ok\n");
}

int main(void)
{
    test_lifecycle();
    test_generation();
    test_lock_hook();
    test_duplicate_register();
    test_init_generation();
    test_capacity();
    printf("objects: all tests passed\n");
    return 0;
}
