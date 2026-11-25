/*
 * cap_selftest.c — 能力模块的内核内自检。
 *
 * 这里定义的是「测试对象」，不是正式对象层：
 *   - 一个全权限测试类型，用于验证派生、撤销、调用守卫；
 *   - 一个只读类型，用于验证 E_TYPE。
 *
 * 正式镜像可以不编译本文件；它只依赖 sbi_putchar 做输出。
 */
#include "capability.h"
#include "sbi.h"

static uint32_t g_invoke_count;
static uint32_t g_finalize_count;
static uint32_t g_created_count;
static uint32_t g_fail;

static void cap_puts(const char *s)
{
    while (*s)
        sbi_putchar(*s++);
}

static void cap_put_dec(uint32_t v)
{
    char buf[12];
    int n = 0;

    if (v == 0) {
        sbi_putchar('0');
        return;
    }
    while (v) {
        buf[n++] = (char)('0' + (v % 10));
        v /= 10;
    }
    while (n)
        sbi_putchar(buf[--n]);
}

static void check(const char *name, int cond)
{
    cap_puts("[cap] ");
    cap_puts(name);
    cap_puts(cond ? "  OK\n" : "  FAIL\n");
    if (!cond)
        g_fail++;
}

static int test_invoke(struct cap_object *obj, uint32_t op, void *args)
{
    (void)args;
    g_invoke_count++;
    if (obj->payload)
        (*(uint32_t *)obj->payload)++;
    return op == CAP_OP_PING ? 7 : 0;
}

static void test_finalize(struct cap_object *obj)
{
    if (obj->payload)
        (*(uint32_t *)obj->payload) = 0xdeadbeefu;
    g_finalize_count++;
}

static const struct cap_object_ops g_test_ops = {
    .invoke = test_invoke,
    .finalize = test_finalize,
};

static const struct cap_type g_test_type = {
    .type = 1,
    .rights_mask = CAP_RIGHT_ALL,
    .supported = (1u << CAP_OP_READ) | (1u << CAP_OP_WRITE) |
                 (1u << CAP_OP_MAP) | (1u << CAP_OP_SEND) |
                 (1u << CAP_OP_RECV) | (1u << CAP_OP_PING),
    .required = {
        [CAP_OP_READ] = CAP_RIGHT_READ,
        [CAP_OP_WRITE] = CAP_RIGHT_WRITE,
        [CAP_OP_MAP] = CAP_RIGHT_MAP,
        [CAP_OP_SEND] = CAP_RIGHT_WRITE,
        [CAP_OP_RECV] = CAP_RIGHT_READ,
        [CAP_OP_PING] = 0,
    },
    .ops = &g_test_ops,
};

static const struct cap_type g_limited_type = {
    .type = 2,
    .rights_mask = CAP_RIGHT_READ,
    .supported = (1u << CAP_OP_READ) | (1u << CAP_OP_PING),
    .required = {
        [CAP_OP_READ] = CAP_RIGHT_READ,
        [CAP_OP_PING] = 0,
    },
    .ops = &g_test_ops,
};

int cap_selftest(void)
{
    cap_puts("\n[cap] capability minimal model\n");
    cap_system_init();
    g_invoke_count = 0;
    g_finalize_count = 0;
    g_created_count = 0;
    g_fail = 0;

    uint32_t p1 = 0, p2 = 0, p3 = 0, p4 = 0;
    cap_oid_t o1 = 0, o2 = 0, o3 = 0, o4 = 0;
    cap_handle_t root = 0, c1 = 0, c2 = 0, c3 = 0, c4 = 0, c5 = 0;
    cap_handle_t lim = 0, tmp = 0, delegated = 0, chain = 0;
    struct cap_info info;
    int rc;

    rc = cap_object_create(&g_test_type, &p1, &o1);
    if (rc == CAP_OK)
        g_created_count++;
    check("object create", rc == CAP_OK);

    rc = cap_root_install(0, 0, o1,
                          CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_GRANT,
                          0, &root);
    check("root install", rc == CAP_OK);

    rc = cap_mint(0, root, 1, CAP_RIGHT_READ, 0, &c1);
    check("mint subset", rc == CAP_OK && cap_object_refs(o1) == 2);

    rc = cap_mint(0, c1, 2, CAP_RIGHT_READ | CAP_RIGHT_WRITE, 0, &c2);
    check("mint superset rejected", rc == CAP_E_RIGHTS);

    rc = cap_mint(0, c1, 2, CAP_RIGHT_READ, 7, &c2);
    check("badge mint needs grant", rc == CAP_E_RIGHTS);

    rc = cap_mint(0, root, 2, CAP_RIGHT_READ | CAP_RIGHT_GRANT, 7, &c2);
    check("badge mint with grant", rc == CAP_OK);

    rc = cap_copy(0, c1, 3, &c3);
    check("copy", rc == CAP_OK && cap_object_refs(o1) == 4);

    rc = cap_move(0, c1, 1, 0, &tmp);
    check("cross-task move needs grant", rc == CAP_E_RIGHTS);

    rc = cap_move(0, c2, 1, 0, &tmp);
    check("cross-task move with grant", rc == CAP_OK);

    rc = cap_info(0, c2, &info);
    check("moved source handle stale", rc == CAP_E_BAD_HANDLE);

    rc = cap_mint_to(0, c1, 2, 0, CAP_RIGHT_READ, 0, &delegated);
    check("cross-task mint needs grant", rc == CAP_E_RIGHTS);
    rc = cap_mint_to(0, root, 2, 0, CAP_RIGHT_READ, 0, &delegated);
    check("cross-task mint with grant", rc == CAP_OK);
    rc = cap_mint_to(0, root, CAP_MAX_TASKS, 0, CAP_RIGHT_READ, 0, &delegated);
    check("mint_to rejects bad target sid", rc == CAP_E_BAD_ARG);
    rc = cap_mint_to(0, root, 2, 0, CAP_RIGHT_READ, 0, 0);
    check("mint_to rejects null out", rc == CAP_E_BAD_ARG);

    /* 深链：验证 revoke 沿 CDT 递归回收，而不是只看直接子节点。 */
    chain = root;
    for (unsigned i = 0; i < 4; i++) {
        cap_handle_t next = 0;

        rc = cap_mint(0, chain, (cap_slot_t)(20 + i),
                      CAP_RIGHT_READ | CAP_RIGHT_GRANT, 0, &next);
        check("deep mint", rc == CAP_OK);
        chain = next;
    }

    rc = cap_revoke(0, root);
    check("revoke root", rc == CAP_OK);
    check("root survives revoke", cap_info(0, root, &info) == CAP_OK);
    check("mint child gone", cap_info(0, c1, &info) == CAP_E_BAD_HANDLE);
    check("copy child gone", cap_info(0, c3, &info) == CAP_E_BAD_HANDLE);
    check("cross-task child gone", cap_info(1, tmp, &info) == CAP_E_BAD_HANDLE);
    check("cross-task mint child gone", cap_info(2, delegated, &info) == CAP_E_BAD_HANDLE);
    check("deep chain gone", cap_info(0, chain, &info) == CAP_E_BAD_HANDLE);
    check("refs back to root", cap_object_refs(o1) == 1);

    rc = cap_delete(0, root);
    check("delete root", rc == CAP_OK);
    check("finalized once", g_finalize_count == 1 && cap_live_objects() == 0);
    check("deleted handle stale", cap_info(0, root, &info) == CAP_E_BAD_HANDLE);

    rc = cap_object_create(&g_test_type, &p2, &o2);
    if (rc == CAP_OK)
        g_created_count++;
    check("object create 2", rc == CAP_OK);

    cap_handle_t root2 = 0;
    rc = cap_root_install(0, 0, o2,
                          CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_GRANT,
                          0, &root2);
    check("slot reuse new gen", rc == CAP_OK && root2 != root);

    rc = cap_mint(0, root2, 5, CAP_RIGHT_READ, 0, &c4);
    check("mint task0 slot5", rc == CAP_OK);
    check("task isolation", cap_info(1, c4, &info) == CAP_E_BAD_HANDLE);

    rc = cap_mint(0, root2, 1, CAP_RIGHT_READ, 0, &c5);
    check("mint read-only", rc == CAP_OK);

    uint32_t before = g_invoke_count;
    rc = cap_invoke(0, c5, CAP_OP_WRITE, 0);
    check("invoke without write right",
          rc == CAP_E_RIGHTS && g_invoke_count == before);
    rc = cap_invoke(0, c5, CAP_OP_READ, 0);
    check("invoke with read right",
          rc == CAP_OK && g_invoke_count == before + 1);

    rc = cap_object_create(&g_limited_type, &p3, &o3);
    if (rc == CAP_OK)
        g_created_count++;
    check("limited object create", rc == CAP_OK);
    rc = cap_root_install(0, 6, o3, CAP_RIGHT_READ, 0, &lim);
    check("limited object install", rc == CAP_OK);
    rc = cap_invoke(0, lim, CAP_OP_WRITE, 0);
    check("unsupported op rejected", rc == CAP_E_TYPE);

    rc = cap_mint(0, root2, 5, CAP_RIGHT_READ, 0, &tmp);
    check("occupied slot rejected", rc == CAP_E_OCCUPIED);
    check("occupied slot unchanged", cap_info(0, c4, &info) == CAP_OK);

    rc = cap_object_create(&g_test_type, &p4, &o4);
    if (rc == CAP_OK)
        g_created_count++;
    check("object create 4", rc == CAP_OK);
    rc = cap_object_discard(o4);
    check("discard unreferenced", rc == CAP_OK && g_finalize_count == 2);

    int loop_ok = 1;
    for (uint32_t i = 0; i < 100; i++) {
        rc = cap_mint(0, root2, CAP_SLOT_AUTO, CAP_RIGHT_READ, 0, &tmp);
        if (rc != CAP_OK) {
            loop_ok = 0;
            break;
        }
        rc = cap_delete(0, tmp);
        if (rc != CAP_OK) {
            loop_ok = 0;
            break;
        }
    }
    check("100 mint/delete cycles", loop_ok);

    rc = cap_delete(0, lim);
    check("delete limited object", rc == CAP_OK);
    rc = cap_delete(0, c5);
    check("delete c5", rc == CAP_OK);
    rc = cap_delete(0, c4);
    check("delete c4", rc == CAP_OK);
    rc = cap_delete(0, root2);
    check("delete root2", rc == CAP_OK);

    /*
     * cap_acquire/cap_release：复合操作在 pin 期间持有对象。
     * 删除最后一个 cap 时若仍有 pin，finalize 必须推迟到 release。
     */
    uint32_t p5 = 0;
    cap_oid_t o5 = 0;
    cap_handle_t pinned = 0;
    struct cap_object *borrowed = NULL;
    struct cap_object *stale = NULL;

    rc = cap_object_create(&g_test_type, &p5, &o5);
    if (rc == CAP_OK)
        g_created_count++;
    check("borrow object create", rc == CAP_OK);
    check("borrow root install",
          cap_root_install(0, 7, o5, CAP_RIGHT_READ, 0, &pinned) == CAP_OK);
    check("acquire rejects missing right",
          cap_acquire(0, pinned, CAP_RIGHT_WRITE, &borrowed) == CAP_E_RIGHTS);
    check("acquire pins object",
          cap_acquire(0, pinned, CAP_RIGHT_READ, &borrowed) == CAP_OK &&
          borrowed != NULL && borrowed->type == &g_test_type &&
          cap_object_pins(o5) == 1);
    check("delete defers finalize",
          cap_delete(0, pinned) == CAP_OK && cap_live_objects() == 1 &&
          cap_object_refs(o5) == 0 && g_finalize_count == g_created_count - 1);
    check("deleted borrow handle stale",
          cap_info(0, pinned, &info) == CAP_E_BAD_HANDLE &&
          cap_acquire(0, pinned, CAP_RIGHT_READ, &stale) == CAP_E_BAD_HANDLE);
    cap_release(borrowed);
    check("release finalizes",
          cap_object_pins(o5) == 0 && cap_live_objects() == 0 &&
          g_finalize_count == g_created_count);

    check("all objects finalized", g_finalize_count == g_created_count);
    check("no live caps", cap_live_caps() == 0);
    check("no live objects", cap_live_objects() == 0);
    check("invariants", cap_check_invariants() == CAP_OK);

    cap_puts("[cap] live caps=");
    cap_put_dec(cap_live_caps());
    cap_puts(" objects=");
    cap_put_dec(cap_live_objects());
    cap_puts(" finalized=");
    cap_put_dec(g_finalize_count);
    cap_puts("/");
    cap_put_dec(g_created_count);
    cap_puts("\n");
    cap_puts(g_fail == 0 ? "[cap] selftest ok\n" : "[cap] selftest FAILED\n");
    return g_fail != 0;
}
