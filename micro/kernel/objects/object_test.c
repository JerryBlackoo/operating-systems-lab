#include "object.h"

#include <assert.h>
#include <stdio.h>

struct fake {
    struct object obj;
    int destroyed;
};

static struct fake fakes[OBJECT_MAX_SLOTS + 8];
static int destroyed_total;

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

    object_ref(&a->obj);
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
    assert(object_lookup(a_id, OBJ_NONE) == 0);

    /* 对死亡对象 ref 不应复活。 */
    object_ref(&a->obj);
    assert(object_refcount(&a->obj) == 0);
    object_unref(&a->obj);
    assert(destroyed_total == 1);

    object_unref(&b->obj);
    assert(destroyed_total == 2);

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

    overflow.destroyed = 0;
    assert(object_register(&overflow.obj, OBJ_REPLY, fake_destroy) < 0);

    for (int i = 0; i < count; i++) {
        object_unref(&registered[i]->obj);
    }
    assert(destroyed_total == OBJECT_MAX_SLOTS);

    printf("objects: capacity ok\n");
}

int main(void)
{
    test_lifecycle();
    test_generation();
    test_capacity();
    printf("objects: all tests passed\n");
    return 0;
}
