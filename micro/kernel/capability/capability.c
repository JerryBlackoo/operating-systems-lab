/*
 * capability.c — 最小能力系统。
 *
 * 设计取舍：
 *   - 平表而不是 seL4 的 CNode 树：最小模型只需要 slot → cap。
 *   - 显式 slot（或 CAP_SLOT_AUTO）：把分配策略留给调用者/上层。
 *   - generation：槽位复用后旧句柄必须失效。
 *   - 单 hart：irq_save/restore 就是全局锁；SMP 时替换成每 CSpace 锁。
 *   - CDT 用平数组 + parent/child/sibling；move 只搬表项，不搬节点。
 */
#include "capability.h"
#include "cap_platform.h"
#include <stddef.h>

struct cap_entry {
    cap_oid_t oid;
    cap_rights_t rights;
    cap_badge_t badge;
    int node;
};

struct cap_cspace {
    struct cap_entry slot[CAP_MAX_SLOTS];
    uint16_t gen[CAP_MAX_SLOTS];
    uint64_t used;
};

struct cap_node {
    int parent;
    int first_child;
    int next_sibling;
    uint8_t used;
};

static struct cap_cspace g_cspaces[CAP_MAX_TASKS];
static struct cap_object g_objects[CAP_MAX_OBJECTS];
static struct cap_node g_nodes[CAP_MAX_NODES];
static uint32_t g_live_caps;
static uint32_t g_live_objects;

static void cap_memzero(void *p, size_t n)
{
    uint8_t *b = (uint8_t *)p;
    for (size_t i = 0; i < n; i++)
        b[i] = 0;
}

static int cap_sid_ok(cap_sid_t sid)
{
    return (uint32_t)sid < CAP_MAX_TASKS;
}

static int cap_oid_ok(cap_oid_t oid)
{
    return oid != 0 && oid <= CAP_MAX_OBJECTS;
}

static struct cap_object *cap_object_get(cap_oid_t oid)
{
    if (!cap_oid_ok(oid))
        return NULL;
    struct cap_object *obj = &g_objects[oid - 1];
    return obj->alive ? obj : NULL;
}

/* ---- 对象生命周期 ---- */

static void cap_object_finalize_locked(struct cap_object *obj)
{
    if (!obj->alive)
        return;

    /*
     * 先置 dead，正常查找路径不再返回它。
     * finalize 运行在能力锁内，对象层不得回调能力模块；否则可能重入/死锁。
     */
    obj->alive = 0;
    g_live_objects--;
    if (obj->type && obj->type->ops && obj->type->ops->finalize)
        obj->type->ops->finalize(obj);
    obj->type = NULL;
    obj->payload = NULL;
    obj->cap_refs = 0;
    obj->pins = 0;
}

static void cap_object_maybe_finalize_locked(struct cap_object *obj)
{
    if (obj->alive && obj->cap_refs == 0 && obj->pins == 0)
        cap_object_finalize_locked(obj);
}

static int cap_object_create_locked(const struct cap_type *type,
                                    void *payload, cap_oid_t *out)
{
    if (!type || !type->ops || !type->ops->invoke || !out)
        return CAP_E_BAD_ARG;

    for (uint32_t i = 0; i < CAP_MAX_OBJECTS; i++) {
        struct cap_object *obj = &g_objects[i];
        if (obj->alive)
            continue;
        obj->type = type;
        obj->payload = payload;
        obj->cap_refs = 0;
        obj->pins = 0;
        obj->alive = 1;
        g_live_objects++;
        *out = i + 1;
        return CAP_OK;
    }
    return CAP_E_NOMEM;
}

static int cap_object_discard_locked(cap_oid_t oid)
{
    struct cap_object *obj = cap_object_get(oid);
    if (!obj)
        return CAP_E_BAD_OBJECT;
    if (obj->cap_refs != 0 || obj->pins != 0)
        return CAP_E_BUSY;
    cap_object_finalize_locked(obj);
    return CAP_OK;
}

/* ---- CDT ---- */

static int cap_node_alloc_locked(int parent)
{
    if (parent >= 0 &&
        (parent >= CAP_MAX_NODES || !g_nodes[parent].used))
        return -1;

    for (int i = 0; i < CAP_MAX_NODES; i++) {
        struct cap_node *n = &g_nodes[i];
        if (n->used)
            continue;
        n->used = 1;
        n->parent = parent;
        n->first_child = -1;
        n->next_sibling = -1;
        if (parent >= 0) {
            n->next_sibling = g_nodes[parent].first_child;
            g_nodes[parent].first_child = i;
        }
        return i;
    }
    return -1;
}

static void cap_node_unlink_locked(int node)
{
    if (node < 0 || node >= CAP_MAX_NODES || !g_nodes[node].used)
        return;

    int parent = g_nodes[node].parent;
    if (parent >= 0 && parent < CAP_MAX_NODES && g_nodes[parent].used) {
        int *link = &g_nodes[parent].first_child;
        while (*link >= 0) {
            if (*link == node) {
                *link = g_nodes[node].next_sibling;
                break;
            }
            link = &g_nodes[*link].next_sibling;
        }
    }
    g_nodes[node].parent = -1;
    g_nodes[node].next_sibling = -1;
}

static void cap_node_free_subtree_locked(int node)
{
    if (node < 0 || node >= CAP_MAX_NODES || !g_nodes[node].used)
        return;

    int child = g_nodes[node].first_child;
    while (child >= 0) {
        int next = g_nodes[child].next_sibling;
        cap_node_free_subtree_locked(child);
        child = next;
    }
    cap_node_unlink_locked(node);
    g_nodes[node].first_child = -1;
    g_nodes[node].used = 0;
}

static int cap_node_is_descendant_locked(int node, int ancestor)
{
    if (node < 0 || ancestor < 0)
        return 0;

    int cur = node;
    int guard = 0;
    while (cur >= 0 && cur < CAP_MAX_NODES && guard++ <= CAP_MAX_NODES) {
        if (cur == ancestor)
            return 1;
        if (!g_nodes[cur].used)
            return 0;
        cur = g_nodes[cur].parent;
    }
    return 0;
}

/* ---- CSpace ---- */

static int cap_slot_used_locked(cap_sid_t sid, cap_slot_t slot)
{
    return (int)((g_cspaces[sid].used >> slot) & 1u);
}

static int cap_slot_auto_locked(cap_sid_t sid)
{
    for (uint32_t i = 0; i < CAP_MAX_SLOTS; i++) {
        if (!cap_slot_used_locked(sid, (cap_slot_t)i))
            return (int)i;
    }
    return -1;
}

static cap_handle_t cap_make_handle_locked(cap_sid_t sid, cap_slot_t slot)
{
    return CAP_HANDLE(slot, g_cspaces[sid].gen[slot]);
}

static void cap_slot_clear_locked(cap_sid_t sid, cap_slot_t slot)
{
    struct cap_entry *e = &g_cspaces[sid].slot[slot];
    e->oid = 0;
    e->rights = 0;
    e->badge = 0;
    e->node = -1;
    g_cspaces[sid].used &= ~((uint64_t)1 << slot);
    g_cspaces[sid].gen[slot] =
        (uint16_t)((g_cspaces[sid].gen[slot] + 1u) & 0xffffu);
    g_live_caps--;
}

static void cap_slot_install_locked(cap_sid_t sid, cap_slot_t slot,
                                    const struct cap_entry *e)
{
    g_cspaces[sid].slot[slot] = *e;
    g_cspaces[sid].used |= (uint64_t)1 << slot;
    g_live_caps++;
}

static int cap_lookup_locked(cap_sid_t sid, cap_handle_t h,
                             struct cap_entry *out)
{
    if (!cap_sid_ok(sid) || !out)
        return CAP_E_BAD_HANDLE;

    uint32_t slot = CAP_HANDLE_SLOT(h);
    if (slot >= CAP_MAX_SLOTS ||
        !cap_slot_used_locked(sid, (cap_slot_t)slot))
        return CAP_E_BAD_HANDLE;
    if ((uint16_t)CAP_HANDLE_GEN(h) != g_cspaces[sid].gen[slot])
        return CAP_E_BAD_HANDLE;

    *out = g_cspaces[sid].slot[slot];
    return CAP_OK;
}

/* 只摘表项，不动物件引用计数和 CDT 节点；delete/move 各自决定下一步。 */
static void cap_drop_cap_locked(cap_sid_t sid, cap_slot_t slot)
{
    struct cap_entry e = g_cspaces[sid].slot[slot];
    cap_slot_clear_locked(sid, slot);

    struct cap_object *obj = cap_object_get(e.oid);
    if (obj) {
        if (obj->cap_refs > 0)
            obj->cap_refs--;
        cap_object_maybe_finalize_locked(obj);
    }
}

/* ---- 派生/转移 ---- */

static int cap_target_slot_locked(cap_sid_t sid, cap_slot_t *slot)
{
    if (*slot == CAP_SLOT_AUTO) {
        int free_slot = cap_slot_auto_locked(sid);
        if (free_slot < 0)
            return CAP_E_NOMEM;
        *slot = (cap_slot_t)free_slot;
        return CAP_OK;
    }
    return (uint32_t)*slot < CAP_MAX_SLOTS ? CAP_OK : CAP_E_BAD_SLOT;
}

static int cap_derive_locked(cap_sid_t sid, cap_handle_t src, cap_slot_t dst,
                             cap_rights_t rights, cap_badge_t badge,
                             cap_handle_t *out)
{
    struct cap_entry se;
    int rc = cap_lookup_locked(sid, src, &se);
    if (rc != CAP_OK)
        return rc;
    if (!out)
        return CAP_E_BAD_ARG;

    rc = cap_target_slot_locked(sid, &dst);
    if (rc != CAP_OK)
        return rc;
    if (cap_slot_used_locked(sid, dst))
        return CAP_E_OCCUPIED;

    struct cap_object *obj = cap_object_get(se.oid);
    if (!obj)
        return CAP_E_BAD_OBJECT;
    if (rights & ~se.rights)
        return CAP_E_RIGHTS;
    if (rights & ~obj->type->rights_mask)
        return CAP_E_RIGHTS;
    /* 改 badge 是一种委托；源 cap 必须带 GRANT。 */
    if (badge != se.badge && !(se.rights & CAP_RIGHT_GRANT))
        return CAP_E_RIGHTS;

    int node = cap_node_alloc_locked(se.node);
    if (node < 0)
        return CAP_E_NOMEM;

    struct cap_entry ne;
    ne.oid = se.oid;
    ne.rights = rights;
    ne.badge = badge;
    ne.node = node;
    cap_slot_install_locked(sid, dst, &ne);
    obj->cap_refs++;
    *out = cap_make_handle_locked(sid, dst);
    return CAP_OK;
}

static int cap_copy_locked(cap_sid_t sid, cap_handle_t src, cap_slot_t dst,
                           cap_handle_t *out)
{
    struct cap_entry se;
    int rc = cap_lookup_locked(sid, src, &se);
    if (rc != CAP_OK)
        return rc;
    return cap_derive_locked(sid, src, dst, se.rights, se.badge, out);
}

static int cap_root_install_locked(cap_sid_t sid, cap_slot_t slot,
                                   cap_oid_t oid, cap_rights_t rights,
                                   cap_badge_t badge, cap_handle_t *out)
{
    if (!cap_sid_ok(sid) || !out)
        return CAP_E_BAD_ARG;

    int rc = cap_target_slot_locked(sid, &slot);
    if (rc != CAP_OK)
        return rc;
    if (cap_slot_used_locked(sid, slot))
        return CAP_E_OCCUPIED;

    struct cap_object *obj = cap_object_get(oid);
    if (!obj)
        return CAP_E_BAD_OBJECT;
    if (rights & ~obj->type->rights_mask)
        return CAP_E_RIGHTS;

    int node = cap_node_alloc_locked(-1);
    if (node < 0)
        return CAP_E_NOMEM;

    struct cap_entry e;
    e.oid = oid;
    e.rights = rights;
    e.badge = badge;
    e.node = node;
    cap_slot_install_locked(sid, slot, &e);
    obj->cap_refs++;
    *out = cap_make_handle_locked(sid, slot);
    return CAP_OK;
}

static int cap_move_locked(cap_sid_t from_sid, cap_handle_t src,
                           cap_sid_t to_sid, cap_slot_t dst,
                           cap_handle_t *out)
{
    if (!cap_sid_ok(from_sid) || !cap_sid_ok(to_sid) || !out)
        return CAP_E_BAD_ARG;

    struct cap_entry e;
    int rc = cap_lookup_locked(from_sid, src, &e);
    if (rc != CAP_OK)
        return rc;

    rc = cap_target_slot_locked(to_sid, &dst);
    if (rc != CAP_OK)
        return rc;
    if (from_sid == to_sid && CAP_HANDLE_SLOT(src) == dst)
        return CAP_E_OCCUPIED;
    if (cap_slot_used_locked(to_sid, dst))
        return CAP_E_OCCUPIED;
    if (from_sid != to_sid && !(e.rights & CAP_RIGHT_GRANT))
        return CAP_E_RIGHTS;

    /*
     * 关键：只搬表项与节点号，不新建 CDT 节点、不改 cap_refs。
     * 这样即使 cap 被移到别的主体，revoke 仍能沿父边找到它。
     */
    cap_slot_clear_locked(from_sid, CAP_HANDLE_SLOT(src));
    cap_slot_install_locked(to_sid, dst, &e);
    *out = cap_make_handle_locked(to_sid, dst);
    return CAP_OK;
}

/* ---- 撤销 ---- */

static int cap_revoke_locked(cap_sid_t sid, cap_handle_t h)
{
    struct cap_entry target;
    int rc = cap_lookup_locked(sid, h, &target);
    if (rc != CAP_OK)
        return rc;

    /* 先摘掉所有后代 cap（可能已被 move 到别的主体）。 */
    for (uint32_t s = 0; s < CAP_MAX_TASKS; s++) {
        for (uint32_t i = 0; i < CAP_MAX_SLOTS; i++) {
            if (!cap_slot_used_locked((cap_sid_t)s, (cap_slot_t)i))
                continue;
            int node = g_cspaces[s].slot[i].node;
            if (node == target.node)
                continue;
            if (cap_node_is_descendant_locked(node, target.node))
                cap_drop_cap_locked((cap_sid_t)s, (cap_slot_t)i);
        }
    }

    /* 再释放后代节点。后代 cap 已全部摘除，节点树可以安全递归回收。 */
    int child = g_nodes[target.node].first_child;
    while (child >= 0) {
        int next = g_nodes[child].next_sibling;
        cap_node_free_subtree_locked(child);
        child = next;
    }
    return CAP_OK;
}

static int cap_delete_locked(cap_sid_t sid, cap_handle_t h)
{
    struct cap_entry e;
    int rc = cap_lookup_locked(sid, h, &e);
    if (rc != CAP_OK)
        return rc;

    rc = cap_revoke_locked(sid, h);
    if (rc != CAP_OK)
        return rc;

    cap_drop_cap_locked(sid, (cap_slot_t)CAP_HANDLE_SLOT(h));
    cap_node_free_subtree_locked(e.node);
    return CAP_OK;
}

/* ---- 公共入口 ---- */

void cap_system_init(void)
{
    uint64_t flags = cap_arch_intr_save();

    for (uint32_t i = 0; i < CAP_MAX_TASKS; i++) {
        cap_memzero(g_cspaces[i].slot, sizeof(g_cspaces[i].slot));
        cap_memzero(g_cspaces[i].gen, sizeof(g_cspaces[i].gen));
        g_cspaces[i].used = 0;
    }
    for (uint32_t i = 0; i < CAP_MAX_OBJECTS; i++)
        cap_memzero(&g_objects[i], sizeof(g_objects[i]));
    for (uint32_t i = 0; i < CAP_MAX_NODES; i++) {
        cap_memzero(&g_nodes[i], sizeof(g_nodes[i]));
        g_nodes[i].parent = -1;
        g_nodes[i].first_child = -1;
        g_nodes[i].next_sibling = -1;
    }
    g_live_caps = 0;
    g_live_objects = 0;

    cap_arch_intr_restore(flags);
}

int cap_object_create(const struct cap_type *type, void *payload,
                      cap_oid_t *out)
{
    uint64_t flags = cap_arch_intr_save();
    int rc = cap_object_create_locked(type, payload, out);
    cap_arch_intr_restore(flags);
    return rc;
}

int cap_object_discard(cap_oid_t oid)
{
    uint64_t flags = cap_arch_intr_save();
    int rc = cap_object_discard_locked(oid);
    cap_arch_intr_restore(flags);
    return rc;
}

int cap_root_install(cap_sid_t sid, cap_slot_t slot, cap_oid_t oid,
                     cap_rights_t rights, cap_badge_t badge,
                     cap_handle_t *out)
{
    uint64_t flags = cap_arch_intr_save();
    int rc = cap_root_install_locked(sid, slot, oid, rights, badge, out);
    cap_arch_intr_restore(flags);
    return rc;
}

int cap_mint(cap_sid_t sid, cap_handle_t src, cap_slot_t dst,
             cap_rights_t rights, cap_badge_t badge, cap_handle_t *out)
{
    uint64_t flags = cap_arch_intr_save();
    int rc = cap_derive_locked(sid, src, dst, rights, badge, out);
    cap_arch_intr_restore(flags);
    return rc;
}

int cap_mint_to(cap_sid_t from_sid, cap_handle_t src, cap_sid_t to_sid,
                cap_slot_t dst, cap_rights_t rights, cap_badge_t badge,
                cap_handle_t *out)
{
    struct cap_entry se;
    struct cap_entry ne;
    struct cap_object *obj;
    uint64_t flags;
    int node;
    int rc;

    flags = cap_arch_intr_save();
    rc = cap_lookup_locked(from_sid, src, &se);
    if (rc != CAP_OK) {
        goto out;
    }
    if (!out || !cap_sid_ok(to_sid)) {
        rc = CAP_E_BAD_ARG;
        goto out;
    }
    if (from_sid != to_sid && !(se.rights & CAP_RIGHT_GRANT)) {
        rc = CAP_E_RIGHTS;
        goto out;
    }
    rc = cap_target_slot_locked(to_sid, &dst);
    if (rc != CAP_OK) {
        goto out;
    }
    if (cap_slot_used_locked(to_sid, dst)) {
        rc = CAP_E_OCCUPIED;
        goto out;
    }
    obj = cap_object_get(se.oid);
    if (!obj) {
        rc = CAP_E_BAD_OBJECT;
        goto out;
    }
    if ((rights & ~se.rights) || (rights & ~obj->type->rights_mask) ||
        (badge != se.badge && !(se.rights & CAP_RIGHT_GRANT))) {
        rc = CAP_E_RIGHTS;
        goto out;
    }
    node = cap_node_alloc_locked(se.node);
    if (node < 0) {
        rc = CAP_E_NOMEM;
        goto out;
    }
    ne = (struct cap_entry){.oid = se.oid, .rights = rights,
                            .badge = badge, .node = node};
    cap_slot_install_locked(to_sid, dst, &ne);
    obj->cap_refs++;
    *out = cap_make_handle_locked(to_sid, dst);
    rc = CAP_OK;
out:
    cap_arch_intr_restore(flags);
    return rc;
}

int cap_copy(cap_sid_t sid, cap_handle_t src, cap_slot_t dst,
             cap_handle_t *out)
{
    uint64_t flags = cap_arch_intr_save();
    int rc = cap_copy_locked(sid, src, dst, out);
    cap_arch_intr_restore(flags);
    return rc;
}

int cap_move(cap_sid_t from_sid, cap_handle_t src, cap_sid_t to_sid,
             cap_slot_t dst, cap_handle_t *out)
{
    uint64_t flags = cap_arch_intr_save();
    int rc = cap_move_locked(from_sid, src, to_sid, dst, out);
    cap_arch_intr_restore(flags);
    return rc;
}

int cap_delete(cap_sid_t sid, cap_handle_t h)
{
    uint64_t flags = cap_arch_intr_save();
    int rc = cap_delete_locked(sid, h);
    cap_arch_intr_restore(flags);
    return rc;
}

int cap_delete_slot_if_present(cap_sid_t sid, cap_slot_t slot)
{
    cap_handle_t h;
    uint64_t flags;
    int rc;

    if (!cap_sid_ok(sid) || slot >= CAP_MAX_SLOTS) {
        return CAP_E_BAD_ARG;
    }
    flags = cap_arch_intr_save();
    if (!cap_slot_used_locked(sid, slot)) {
        cap_arch_intr_restore(flags);
        return CAP_OK;
    }
    h = cap_make_handle_locked(sid, slot);
    rc = cap_delete_locked(sid, h);
    cap_arch_intr_restore(flags);
    return rc;
}

int cap_revoke(cap_sid_t sid, cap_handle_t h)
{
    uint64_t flags = cap_arch_intr_save();
    int rc = cap_revoke_locked(sid, h);
    cap_arch_intr_restore(flags);
    return rc;
}

int cap_invoke(cap_sid_t sid, cap_handle_t h, uint32_t op, void *args)
{
    if (op >= CAP_OP_MAX)
        return CAP_E_BAD_ARG;

    uint64_t flags = cap_arch_intr_save();
    struct cap_entry e;
    int rc = cap_lookup_locked(sid, h, &e);
    if (rc != CAP_OK) {
        cap_arch_intr_restore(flags);
        return rc;
    }

    struct cap_object *obj = cap_object_get(e.oid);
    if (!obj) {
        cap_arch_intr_restore(flags);
        return CAP_E_BAD_OBJECT;
    }
    const struct cap_type *type = obj->type;
    if (!(type->supported & (1u << op))) {
        cap_arch_intr_restore(flags);
        return CAP_E_TYPE;
    }
    if (type->required[op] & ~e.rights) {
        cap_arch_intr_restore(flags);
        return CAP_E_RIGHTS;
    }

    /*
     * pin 住对象再放锁调用。删掉最后一个 cap 不会 finalize 一个正在
     * invoke 的对象；unpin 之后再做一次 finalize 检查。
     */
    obj->pins++;
    cap_arch_intr_restore(flags);

    rc = type->ops->invoke(obj, op, args);

    flags = cap_arch_intr_save();
    if (obj->pins > 0)
        obj->pins--;
    cap_object_maybe_finalize_locked(obj);
    cap_arch_intr_restore(flags);
    return rc;
}

/*
 * 借用缝：只做「解析 + 权限包含 + pin」，不执行任何对象语义。
 * 复合操作（例如同时需要 AS cap 与 frame cap 的 map）由对象层在 pin 住
 * 两个对象后完成；这样既保持 cap_invoke 的单一入口语义，又避免在
 * 能力模块里硬编码对象之间的关系。
 */
int cap_acquire(cap_sid_t sid, cap_handle_t h, cap_rights_t rights,
                struct cap_object **out)
{
    if (!out)
        return CAP_E_BAD_ARG;

    uint64_t flags = cap_arch_intr_save();
    struct cap_entry e;
    int rc = cap_lookup_locked(sid, h, &e);
    if (rc != CAP_OK) {
        cap_arch_intr_restore(flags);
        return rc;
    }

    struct cap_object *obj = cap_object_get(e.oid);
    if (!obj) {
        cap_arch_intr_restore(flags);
        return CAP_E_BAD_OBJECT;
    }
    if ((e.rights & rights) != rights) {
        cap_arch_intr_restore(flags);
        return CAP_E_RIGHTS;
    }

    obj->pins++;
    *out = obj;
    cap_arch_intr_restore(flags);
    return CAP_OK;
}

void cap_release(struct cap_object *obj)
{
    if (!obj)
        return;

    uint64_t flags = cap_arch_intr_save();
    if (obj->pins > 0)
        obj->pins--;
    cap_object_maybe_finalize_locked(obj);
    cap_arch_intr_restore(flags);
}

bool cap_holder_has_oid(cap_sid_t sid, cap_oid_t oid, cap_rights_t need)
{
    uint64_t flags;
    unsigned i;
    bool found = false;

    if (!cap_sid_ok(sid) || !cap_oid_ok(oid) || need == 0) {
        return false;
    }

    flags = cap_arch_intr_save();
    for (i = 0; i < CAP_MAX_SLOTS; i++) {
        if (!cap_slot_used_locked(sid, (cap_slot_t)i)) {
            continue;
        }
        if (g_cspaces[sid].slot[i].oid == oid &&
            (g_cspaces[sid].slot[i].rights & need) == need) {
            found = true;
            break;
        }
    }
    cap_arch_intr_restore(flags);
    return found;
}

int cap_info(cap_sid_t sid, cap_handle_t h, struct cap_info *out)
{
    if (!out)
        return CAP_E_BAD_ARG;

    uint64_t flags = cap_arch_intr_save();
    struct cap_entry e;
    int rc = cap_lookup_locked(sid, h, &e);
    if (rc != CAP_OK) {
        cap_arch_intr_restore(flags);
        return rc;
    }

    struct cap_object *obj = cap_object_get(e.oid);
    if (!obj) {
        cap_arch_intr_restore(flags);
        return CAP_E_BAD_OBJECT;
    }
    out->oid = e.oid;
    out->type = obj->type->type;
    out->rights = e.rights;
    out->badge = e.badge;
    cap_arch_intr_restore(flags);
    return CAP_OK;
}

uint32_t cap_live_caps(void)
{
    uint64_t flags = cap_arch_intr_save();
    uint32_t v = g_live_caps;
    cap_arch_intr_restore(flags);
    return v;
}

uint32_t cap_live_objects(void)
{
    uint64_t flags = cap_arch_intr_save();
    uint32_t v = g_live_objects;
    cap_arch_intr_restore(flags);
    return v;
}

uint32_t cap_object_refs(cap_oid_t oid)
{
    uint64_t flags = cap_arch_intr_save();
    struct cap_object *obj = cap_object_get(oid);
    uint32_t v = obj ? obj->cap_refs : 0;
    cap_arch_intr_restore(flags);
    return v;
}

uint32_t cap_object_pins(cap_oid_t oid)
{
    uint64_t flags = cap_arch_intr_save();
    struct cap_object *obj = cap_object_get(oid);
    uint32_t v = obj ? obj->pins : 0;
    cap_arch_intr_restore(flags);
    return v;
}

int cap_check_invariants(void)
{
    uint32_t refs[CAP_MAX_OBJECTS];
    uint8_t node_seen[CAP_MAX_NODES];
    uint8_t node_owner[CAP_MAX_NODES];
    /* W3 需要沿父边比较 (oid, rights)；放在静态区，避免吃掉内核栈。 */
    static cap_oid_t node_oid[CAP_MAX_NODES];
    static cap_rights_t node_rights[CAP_MAX_NODES];
    uint32_t cap_count = 0;
    uint32_t obj_count = 0;
    int bad = 0;

    uint64_t flags = cap_arch_intr_save();
    cap_memzero(refs, sizeof(refs));
    cap_memzero(node_seen, sizeof(node_seen));
    cap_memzero(node_owner, sizeof(node_owner));
    cap_memzero(node_oid, sizeof(node_oid));
    cap_memzero(node_rights, sizeof(node_rights));

    for (uint32_t s = 0; s < CAP_MAX_TASKS && !bad; s++) {
        for (uint32_t i = 0; i < CAP_MAX_SLOTS; i++) {
            if (!cap_slot_used_locked((cap_sid_t)s, (cap_slot_t)i))
                continue;
            struct cap_entry *e = &g_cspaces[s].slot[i];
            if (e->oid == 0 || e->oid > CAP_MAX_OBJECTS) {
                bad = 1;
                break;
            }
            struct cap_object *obj = &g_objects[e->oid - 1];
            if (!obj->alive || !obj->type || !obj->type->ops) {
                bad = 1;
                break;
            }
            if (e->rights & ~obj->type->rights_mask) {
                bad = 1;
                break;
            }
            if (e->node < 0 || e->node >= CAP_MAX_NODES ||
                !g_nodes[e->node].used) {
                bad = 1;
                break;
            }
            if (node_seen[e->node]) {
                bad = 1;
                break;
            }
            node_seen[e->node] = 1;
            node_owner[e->node]++;
            node_oid[e->node] = e->oid;
            node_rights[e->node] = e->rights;
            refs[e->oid - 1]++;
            cap_count++;
        }
    }

    for (uint32_t i = 0; i < CAP_MAX_NODES && !bad; i++) {
        if (g_nodes[i].used && node_owner[i] != 1)
            bad = 1;
        if (!g_nodes[i].used && node_owner[i] != 0)
            bad = 1;
    }

    for (uint32_t i = 0; i < CAP_MAX_OBJECTS && !bad; i++) {
        struct cap_object *obj = &g_objects[i];
        if (obj->alive) {
            obj_count++;
            if (obj->cap_refs != refs[i])
                bad = 1;
            if (!obj->type || !obj->type->ops || !obj->type->ops->invoke)
                bad = 1;
        } else if (refs[i] != 0) {
            bad = 1;
        }
    }

    for (uint32_t i = 0; i < CAP_MAX_NODES && !bad; i++) {
        if (!g_nodes[i].used)
            continue;
        int cur = (int)i;
        int steps = 0;
        while (cur >= 0) {
            if (cur >= CAP_MAX_NODES || !g_nodes[cur].used) {
                bad = 1;
                break;
            }
            cur = g_nodes[cur].parent;
            if (++steps > CAP_MAX_NODES) {
                bad = 1;
                break;
            }
        }
    }

    /* W3：父边两侧必须指向同一对象，且子 cap 的权限是父权限的子集。 */
    for (uint32_t i = 0; i < CAP_MAX_NODES && !bad; i++) {
        if (!g_nodes[i].used)
            continue;
        int parent = g_nodes[i].parent;
        if (parent < 0)
            continue;
        if (parent >= CAP_MAX_NODES || !g_nodes[parent].used) {
            bad = 1;
            break;
        }
        if (node_oid[i] != node_oid[parent] ||
            (node_rights[i] & ~node_rights[parent]) != 0)
            bad = 1;
    }

    if (cap_count != g_live_caps)
        bad = 1;
    if (obj_count != g_live_objects)
        bad = 1;

    cap_arch_intr_restore(flags);
    return bad ? CAP_E_INVARIANTS : CAP_OK;
}
