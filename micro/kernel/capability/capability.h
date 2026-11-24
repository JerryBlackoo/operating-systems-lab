/*
 * capability.h — 能力模块的公共契约。
 *
 * 本模块只回答一个问题：
 *   某个主体，能不能通过某个 cap，对某个对象执行某个操作？
 *
 * 它不定义对象语义、IPC、内存映射和调度。对象层通过 struct cap_type /
 * struct cap_object_ops 注入类型、权限上限、操作要求和 invoke/finalize。
 *
 * 最小模型：
 *   每个主体一张私有表；表项 = (oid, rights, badge, node)；
 *   新 cap 只能由旧 cap 按权限子集派生，或由 move 转移；
 *   对象寿命 = 表项引用计数；revoke = 删除派生子树。
 */
#ifndef MICRO_KERNEL_CAPABILITY_H
#define MICRO_KERNEL_CAPABILITY_H

#include <stdint.h>

#define CAP_MAX_TASKS       8
#define CAP_MAX_SLOTS       64
#define CAP_MAX_OBJECTS     64
#define CAP_MAX_NODES       (CAP_MAX_TASKS * CAP_MAX_SLOTS)

/* 让 mint/copy/move 自动选择目标槽位。 */
#define CAP_SLOT_AUTO       0xffu

typedef uint32_t cap_oid_t;
typedef uint32_t cap_handle_t;
typedef uint16_t cap_rights_t;
typedef uint16_t cap_badge_t;
typedef uint8_t  cap_slot_t;
typedef uint8_t  cap_sid_t;
typedef uint8_t  cap_type_t;

/*
 * 用户态看到的是 (slot, generation) 编码后的句柄，而不是内核指针。
 * generation 在槽位释放时递增，用来拒绝陈旧句柄。
 */
#define CAP_HANDLE(slot, gen) (((uint32_t)(gen) << 16) | \
                               (uint32_t)((slot) & 0xffffu))
#define CAP_HANDLE_SLOT(h)    ((uint32_t)(h) & 0xffffu)
#define CAP_HANDLE_GEN(h)     ((uint32_t)(h) >> 16)

#define CAP_RIGHT_READ   (1u << 0)
#define CAP_RIGHT_WRITE  (1u << 1)
#define CAP_RIGHT_GRANT  (1u << 2)
#define CAP_RIGHT_MAP    (1u << 3)
#define CAP_RIGHT_EXEC   (1u << 4)
#define CAP_RIGHT_ALL    (CAP_RIGHT_READ | CAP_RIGHT_WRITE | \
                          CAP_RIGHT_GRANT | CAP_RIGHT_MAP | CAP_RIGHT_EXEC)

enum cap_op {
    CAP_OP_READ = 0,
    CAP_OP_WRITE,
    CAP_OP_MAP,
    CAP_OP_SEND,
    CAP_OP_RECV,
    CAP_OP_PING,
    CAP_OP_MAX
};

enum cap_error {
    CAP_OK = 0,
    CAP_E_BAD_SLOT = -1,
    CAP_E_BAD_HANDLE = -2,
    CAP_E_OCCUPIED = -3,
    CAP_E_RIGHTS = -4,
    CAP_E_TYPE = -5,
    CAP_E_NOMEM = -6,
    CAP_E_BAD_OBJECT = -7,
    CAP_E_BAD_ARG = -8,
    CAP_E_BUSY = -9,
    CAP_E_INVARIANTS = -10,
};

struct cap_object;

struct cap_object_ops {
    int  (*invoke)(struct cap_object *obj, uint32_t op, void *args);
    void (*finalize)(struct cap_object *obj);
};

/*
 * 对象层提供的类型描述：
 *   rights_mask  该类型允许的最大权限；
 *   supported    支持的操作位图；
 *   required[op] 该操作要求的最小权限。
 * 三个检查都在 cap_invoke 的统一入口完成。
 */
struct cap_type {
    cap_type_t type;
    cap_rights_t rights_mask;
    uint16_t supported;
    uint16_t required[CAP_OP_MAX];
    const struct cap_object_ops *ops;
};

struct cap_object {
    const struct cap_type *type;
    void *payload;
    uint32_t cap_refs;  /* 指向本对象的 cap 数 */
    uint32_t pins;      /* 在途 invoke 数；防止 check-use 之间被 finalize */
    uint8_t  alive;
};

struct cap_info {
    cap_oid_t oid;
    cap_type_t type;
    cap_rights_t rights;
    cap_badge_t badge;
};

void cap_system_init(void);

/* 对象创建/丢弃属于 objects/untyped 层；这里只提供最小 bootstrap 接口。 */
int cap_object_create(const struct cap_type *type, void *payload,
                      cap_oid_t *out);
int cap_object_discard(cap_oid_t oid);

/* 引导：把第一个 cap 装进指定主体的槽位。 */
int cap_root_install(cap_sid_t sid, cap_slot_t slot, cap_oid_t oid,
                     cap_rights_t rights, cap_badge_t badge,
                     cap_handle_t *out);

/* 派生：mint 允许减权/改 badge；copy 是同权复制。 */
int cap_mint(cap_sid_t sid, cap_handle_t src, cap_slot_t dst,
             cap_rights_t rights, cap_badge_t badge, cap_handle_t *out);
int cap_copy(cap_sid_t sid, cap_handle_t src, cap_slot_t dst,
             cap_handle_t *out);

/* 转移：跨主体时要求源 cap 带 GRANT；节点与父边保持不动。 */
int cap_move(cap_sid_t from_sid, cap_handle_t src, cap_sid_t to_sid,
             cap_slot_t dst, cap_handle_t *out);

/* 撤销：delete = revoke(后代) + 删除自己。 */
int cap_delete(cap_sid_t sid, cap_handle_t h);
int cap_revoke(cap_sid_t sid, cap_handle_t h);

/* 唯一调用入口：查表 → 查类型 → required ⊆ rights → 对象 ops。 */
int cap_invoke(cap_sid_t sid, cap_handle_t h, uint32_t op, void *args);

/*
 * 内核内的「借用」缝：按 rights 解析并 pin 住对象，成功返回对象指针。
 * 用于需要同时持有两个 cap 的复合操作（例如 AS_MAP 要同时检查 AS cap 与
 * frame cap），以及 syscall 后端的 acquire/release 契约。
 *
 * 这不是用户 ABI：调用者必须持有 pin 直到 cap_release()，期间对象不会被
 * 最后一个 cap 的删除终结。cap_release() 与 cap_acquire() 必须严格配对。
 */
int cap_acquire(cap_sid_t sid, cap_handle_t h, cap_rights_t rights,
                struct cap_object **out);
void cap_release(struct cap_object *obj);

int cap_info(cap_sid_t sid, cap_handle_t h, struct cap_info *out);

/* 自检/观测接口；不是用户 ABI。 */
uint32_t cap_live_caps(void);
uint32_t cap_live_objects(void);
uint32_t cap_object_refs(cap_oid_t oid);
uint32_t cap_object_pins(cap_oid_t oid);
int cap_check_invariants(void);

/* 仅用于内核内自检；正式镜像可以不编译 cap_selftest.c。 */
int cap_selftest(void);

#endif /* MICRO_KERNEL_CAPABILITY_H */
