#include "demo.h"
#include "object.h"

static unsigned destroyed;

static void destroy(struct object *object)
{
    CHECK(object_refcount(object) == 0);
    destroyed++;
}

static void run(void)
{
    struct object object = {0};
    object_system_init();
    destroyed = 0;

    CHECK(object_register(&object, OBJ_ENDPOINT, destroy) == 0);
    uint32_t old = object_get_id(&object);
    CHECK(object_lookup(old, OBJ_AS) == NULL);
    struct object *ref = object_lookup(old, OBJ_ENDPOINT);
    CHECK(ref == &object && object_refcount(ref) == 2);
    object_unref(ref);
    object_unref(&object);
    CHECK(destroyed == 1 && !object_alive(&object));
    CHECK(object_register(&object, OBJ_ENDPOINT, destroy) == 0);
    CHECK(object_get_id(&object) != old && object_lookup(old, OBJ_ENDPOINT) == NULL);
    object_unref(&object);
    CHECK(destroyed == 2);
    demo_log("objects: typed lookup, last-reference destruction and stale ID");
}

DEMO_MODULE(objects, false, run, NULL);
