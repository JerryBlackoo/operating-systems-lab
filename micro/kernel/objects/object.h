#ifndef MICRO_KERNEL_OBJECT_H
#define MICRO_KERNEL_OBJECT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/*
 * 内核对象基座：只负责身份、类型和生命周期，不承担调度/映射/IPC 策略。
 *
 * 所有内核对象都把 struct object 放在第一个字段，从而可以：
 *   - 用同一套 ref/unref 管理生命周期；
 *   - 用 (id, generation) 安全地查找对象，避免槽位复用造成的 ABA；
 *   - 在对象层统一实现析构回调，类型模块只关心自己的状态机。
 */

#define OBJECT_MAX_SLOTS 256

enum object_type {
    OBJ_NONE = 0,
    OBJ_THREAD,
    OBJ_AS,
    OBJ_ENDPOINT,
    OBJ_REPLY,
    OBJ_TYPE_MAX
};

struct object;
typedef void (*object_destroy_fn)(struct object *obj);

struct object {
    enum object_type type;
    uint32_t refcnt;
    uint32_t id; /* (generation << 16) | slot */
    uint16_t slot;
    uint16_t gen;
    object_destroy_fn destroy;
};

/*
 * 注册表锁钩子。单核教学模型在 sched 初始化时把它接到“关中断/恢复中断”；
 * 主机自测使用默认的空实现。引用计数本身使用原子操作，钩子保护槽位表
 * 以及类型模块“扫描自有静态池 + 注册对象”这段复合操作。
 */
typedef uintptr_t (*object_lock_fn)(void);
typedef void (*object_unlock_fn)(uintptr_t flags);

void object_system_init(void);
void object_set_lock(object_lock_fn lock, object_unlock_fn unlock);

/*
 * 类型模块分配自有对象槽时使用同一把内核锁。
 * 未安装钩子时为空操作；object_register/lookup 内部会嵌套获取同一把锁，
 * 单核关中断实现允许嵌套。
 */
uintptr_t object_lock(void);
void object_unlock(uintptr_t flags);

/* 创建一个对象槽位；成功后 refcnt 为 1，失败返回负值。 */
int object_register(struct object *obj, enum object_type type,
                    object_destroy_fn destroy);

/*
 * 引用计数操作。object_ref 返回是否成功获得引用；refcnt 已为 0 的对象
 * 正在析构，不能被复活。object_unref 对已死亡对象是空操作。
 */
bool object_ref(struct object *obj);
void object_unref(struct object *obj);

/*
 * 按 id 查找并返回一个额外引用，调用者必须 object_unref。
 * type 传 OBJ_NONE 表示不限制类型。查找与加引用在同一临界区内完成：
 * 并发到达的末引用释放不会让调用者拿到一个 refcnt == 0 的对象。
 */
struct object *object_lookup(uint32_t id, enum object_type type);

enum object_type object_get_type(const struct object *obj);
const char *object_type_name(enum object_type type);
uint32_t object_get_id(const struct object *obj);
uint32_t object_refcount(const struct object *obj);
bool object_alive(const struct object *obj);
uint32_t object_live_count(void);

#endif /* MICRO_KERNEL_OBJECT_H */
