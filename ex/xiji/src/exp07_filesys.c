/*
 * 希冀实验七：文件管理 —— 树形文件系统 mkdir/create/read/write/del/rm
 *
 * 课内要求：以"当前目录 recent"为基准维护目录树，节点用 child 指向子链表、
 * next/prev 维护兄弟链；isdir 区分目录与文件；删除前要做名字重复与类型检查。
 *
 * 原实现的两个坑，这次直接消灭：
 *   1. 名字拷贝无条件读到 name[15]，短字面量会越界读；这里按 strlen 上限拷贝并
 *      拒绝过长名字，同时对 16 字节满名强制 NUL；
 *   2. del/rm 只做摘链，没有把节点真正归还池子，长时间使用会耗尽。
 *      这里改为显式 free 标记 + 池子复用，并提供 format() 把树序列化出来，
 *      供 QEMU 写入真实 ATA 扇区后回读比对。
 */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

#define NAME_MAX 16
#define DATA_MAX 64
#define NODE_MAX 24

struct fs_node {
    char name[NAME_MAX];
    int isdir;
    int used;
    char data[DATA_MAX];
    int data_len;
    struct fs_node *parent;
    struct fs_node *child;
    struct fs_node *next;
    struct fs_node *prev;
};

static struct fs_node pool[NODE_MAX];
static struct fs_node *root;
static struct fs_node *recent;
static struct fs_node *open_file;

static void run_reset(void);

/* ---------- 名字与节点 ---------- */

static int name_copy(char *dst, const char *src)
{
    int i = 0;

    if (!src || !*src)
        return -1;
    while (src[i]) {
        if (i >= NAME_MAX - 1)
            return -1; /* 过长：报错而不是静默截断 */
        dst[i] = src[i];
        i++;
    }
    dst[i] = '\0';
    return 0;
}

static int name_eq(const char *a, const char *b)
{
    while (*a && *b) {
        if (*a != *b)
            return 0;
        a++;
        b++;
    }
    return *a == *b;
}

static struct fs_node *node_alloc(const char *name, int isdir, struct fs_node *parent)
{
    struct fs_node *n = 0;

    /* 优先复用已释放的槽位，避免反复创建/删除后池子耗尽 */
    for (int i = 0; i < NODE_MAX; i++) {
        if (!pool[i].used) {
            n = &pool[i];
            break;
        }
    }
    if (!n)
        return 0;
    *n = (struct fs_node){ 0 };
    if (name_copy(n->name, name) != 0)
        return 0;
    n->isdir = isdir;
    n->used = 1;
    n->parent = parent;
    return n;
}

/* 归还节点：在池子里标记空闲即可（池子不移动，指针持续有效） */
static void node_free(struct fs_node *n)
{
    if (!n)
        return;
    n->used = 0;
    n->isdir = 0;
    n->name[0] = '\0';
    n->data_len = 0;
    n->parent = 0;
    n->child = 0;
    n->next = 0;
    n->prev = 0;
}

static struct fs_node *lookup_child(struct fs_node *dir, const char *name)
{
    if (!dir || !dir->isdir)
        return 0;
    for (struct fs_node *c = dir->child; c; c = c->next) {
        if (c->used && name_eq(c->name, name))
            return c;
    }
    return 0;
}

static void attach_child(struct fs_node *parent, struct fs_node *child)
{
    child->parent = parent;
    child->next = 0;
    child->prev = 0;
    if (!parent->child) {
        parent->child = child;
        return;
    }
    struct fs_node *tail = parent->child;
    while (tail->next)
        tail = tail->next;
    tail->next = child;
    child->prev = tail;
}

static void detach_child(struct fs_node *parent, struct fs_node *n)
{
    if (n->prev)
        n->prev->next = n->next;
    else if (parent->child == n)
        parent->child = n->next;
    if (n->next)
        n->next->prev = n->prev;
    node_free(n);
}

/* ---------- 目录与文件操作 ---------- */

static int mkdir(const char *name)
{
    struct fs_node *d;

    if (!recent || !recent->isdir)
        return -1;
    if (lookup_child(recent, name))
        return -2;
    d = node_alloc(name, 1, recent);
    if (!d)
        return -3;
    attach_child(recent, d);
    return 0;
}

static int create(const char *name)
{
    struct fs_node *f;

    if (!recent || !recent->isdir)
        return -1;
    if (lookup_child(recent, name))
        return -2;
    f = node_alloc(name, 0, recent);
    if (!f)
        return -3;
    attach_child(recent, f);
    return 0;
}

static int open(const char *name)
{
    struct fs_node *n = lookup_child(recent, name);

    if (!n || n->isdir)
        return -1;
    open_file = n;
    return 0;
}

static void close_fs(void)
{
    open_file = 0;
}

static int write_file(const char *payload)
{
    int len = 0;

    if (!open_file)
        return -1;
    while (payload[len])
        len++;
    if (len >= DATA_MAX)
        return -2; /* 超长必须报错，不能截断后假装成功 */
    for (int i = 0; i < len; i++)
        open_file->data[i] = payload[i];
    open_file->data[len] = '\0';
    open_file->data_len = len;
    return len;
}

static int read_file(char *out, int cap)
{
    int n;

    if (!open_file || !out || cap <= 0)
        return -1;
    n = open_file->data_len;
    if (n >= cap)
        n = cap - 1; /* 缓冲区不足时按容量截断，但返回值如实反映写入量 */
    for (int i = 0; i < n; i++)
        out[i] = open_file->data[i];
    out[n] = '\0';
    return n;
}

static int del_file(const char *name)
{
    struct fs_node *n = lookup_child(recent, name);

    if (!n || n->isdir)
        return -1;
    if (open_file == n)
        open_file = 0;
    detach_child(recent, n);
    return 0;
}

static int rm_dir(const char *name)
{
    struct fs_node *n = lookup_child(recent, name);

    if (!n || !n->isdir)
        return -1;
    if (n->child)
        return -2;
    detach_child(recent, n);
    return 0;
}

/* ---------- 跨层：把树序列化，供 ATA 回读比对 ---------- */

static int fmt_append(char *out, int cap, int *pos, const char *s)
{
    while (*s) {
        if (*pos >= cap - 1)
            return -1;
        out[(*pos)++] = *s++;
    }
    out[*pos] = '\0';
    return 0;
}

static int fmt_walk(char *out, int cap, int *pos, struct fs_node *dir, int depth)
{
    for (struct fs_node *c = dir->child; c; c = c->next) {
        if (!c->used)
            continue;
        for (int i = 0; i < depth; i++) {
            if (fmt_append(out, cap, pos, "  ") != 0)
                return -1;
        }
        if (fmt_append(out, cap, pos, c->isdir ? "d " : "f ") != 0)
            return -1;
        if (fmt_append(out, cap, pos, c->name) != 0)
            return -1;
        if (!c->isdir) {
            if (fmt_append(out, cap, pos, "=") != 0)
                return -1;
            for (int i = 0; i < c->data_len; i++) {
                if (*pos >= cap - 1)
                    return -1;
                out[(*pos)++] = c->data[i];
            }
            out[*pos] = '\0';
        }
        if (fmt_append(out, cap, pos, "\n") != 0)
            return -1;
        if (c->isdir && fmt_walk(out, cap, pos, c, depth + 1) != 0)
            return -1;
    }
    return 0;
}

int xiji_exp07_format(char *out, int cap)
{
    int pos = 0;

    if (!out || cap <= 0)
        return -1;
    out[0] = '\0';
    if (fmt_append(out, cap, &pos, "/\n") != 0)
        return -1;
    if (fmt_walk(out, cap, &pos, root, 1) != 0)
        return -1;
    return pos;
}

/*
 * 构造一棵演示用目录树，供 QEMU 硬件轨写到真实扇区后回读比对。
 * 与 xiji_exp07_run() 分开，这样硬件轨不必依赖算法断言是否已执行过。
 */
void xiji_exp07_seed_demo(void)
{
    run_reset();
    mkdir("docs");
    mkdir("bin");
    recent = lookup_child(root, "docs");
    create("readme.txt");
    open("readme.txt");
    write_file("xiji-fs");
    close_fs();
    mkdir("sub");
    recent = lookup_child(recent, "sub");
    create("deep.txt");
    open("deep.txt");
    write_file("nested");
    close_fs();
    recent = root;
}

/* ---------- 自检 ---------- */

static void run_reset(void)
{
    open_file = 0;
    for (int i = 0; i < NODE_MAX; i++)
        pool[i] = (struct fs_node){ 0 };
    root = node_alloc("/", 1, 0);
    recent = root;
}

static int count_children(const struct fs_node *dir)
{
    int n = 0;

    for (const struct fs_node *c = dir->child; c; c = c->next) {
        if (c->used)
            n++;
    }
    return n;
}

/* 兄弟链双向一致性 + 父指针正确 */
static int tree_ok(const struct fs_node *dir)
{
    const struct fs_node *prev = 0;

    for (const struct fs_node *c = dir->child; c; c = c->next) {
        if (!c->used)
            return 0;
        if (c->prev != prev)
            return 0;
        if (c->parent != dir)
            return 0;
        if (c->isdir && !tree_ok(c))
            return 0;
        prev = c;
    }
    return 1;
}

static int format_contains(const char *hay, const char *needle)
{
    if (!*needle)
        return 1;
    for (const char *p = hay; *p; p++) {
        const char *a = p;
        const char *b = needle;
        while (*a && *b && *a == *b) {
            a++;
            b++;
        }
        if (!*b)
            return 1;
    }
    return 0;
}

int xiji_exp07_run(void)
{
    char buf[512];
    char tree[512];
    struct fs_node *docs;

    run_reset();
    XIJI_CHECK("exp07-root", root && root->isdir && count_children(root) == 0);

    /* --- mkdir / create --- */
    XIJI_CHECK("exp07-mkdir", mkdir("docs") == 0);
    docs = lookup_child(root, "docs");
    XIJI_CHECK("exp07-mkdir-node", docs && docs->isdir && docs->parent == root);
    XIJI_CHECK("exp07-mkdir-dup", mkdir("docs") == -2);
    XIJI_CHECK("exp07-tree-ok", tree_ok(root));

    recent = docs;
    XIJI_CHECK("exp07-create", create("readme.txt") == 0);
    XIJI_CHECK("exp07-dup", create("readme.txt") == -2);
    XIJI_CHECK("exp07-create-nested-dir", mkdir("sub") == 0);

    /* --- 同名不同类：文件与目录不能重名 --- */
    XIJI_CHECK("exp07-name-collision", create("sub") == -2 && mkdir("readme.txt") == -2);

    /* --- open / write / read --- */
    XIJI_CHECK("exp07-open", open("readme.txt") == 0);
    XIJI_CHECK("exp07-open-missing", open("nope.txt") == -1);
    XIJI_CHECK("exp07-open-dir", open("sub") == -1);
    XIJI_CHECK("exp07-write", write_file("hello-xiji") == 10);
    XIJI_CHECK("exp07-read", read_file(buf, (int)sizeof buf) == 10);
    XIJI_CHECK("exp07-read-data", name_eq(buf, "hello-xiji"));

    /* 小缓冲区截断 + 超长写入拒绝 */
    XIJI_CHECK("exp07-read-short", read_file(buf, 6) == 5 && name_eq(buf, "hello"));
    XIJI_CHECK("exp07-write-too-long",
               write_file("0123456789012345678901234567890123456789012345678901234567890123"
                          "456789") == -2);

    /* 覆写：新内容短于旧内容时长度要更新 */
    XIJI_CHECK("exp07-overwrite", write_file("hi") == 2);
    XIJI_CHECK("exp07-overwrite-len", read_file(buf, (int)sizeof buf) == 2 && name_eq(buf, "hi"));

    /* --- 名字边界 --- */
    close_fs();
    XIJI_CHECK("exp07-name-max", create("abcdefghijklmno") == 0); /* 15 字符 */
    XIJI_CHECK("exp07-name-too-long", create("abcdefghijklmnop") == -3); /* 16 字符被拒 */
    XIJI_CHECK("exp07-name-empty", create("") == -3);

    /* --- 删除 --- */
    XIJI_CHECK("exp07-del", del_file("readme.txt") == 0);
    XIJI_CHECK("exp07-del-gone", !lookup_child(docs, "readme.txt"));
    XIJI_CHECK("exp07-del-twice", del_file("readme.txt") == -1);
    XIJI_CHECK("exp07-del-dir-as-file", del_file("sub") == -1);
    XIJI_CHECK("exp07-rm-file-as-dir", rm_dir("abcdefghijklmno") == -1);

    /* --- rm 非空目录必须失败，空目录可删 --- */
    recent = lookup_child(docs, "sub");
    XIJI_CHECK("exp07-rm-setup", create("inner.txt") == 0);
    recent = docs;
    XIJI_CHECK("exp07-rm-busy", rm_dir("sub") == -2);
    XIJI_CHECK("exp07-rm-busy-intact", lookup_child(docs, "sub") != 0);
    recent = lookup_child(docs, "sub");
    XIJI_CHECK("exp07-rm-inner-del", del_file("inner.txt") == 0);
    recent = docs;
    XIJI_CHECK("exp07-rm", rm_dir("sub") == 0 && !lookup_child(docs, "sub"));
    XIJI_CHECK("exp07-tree-final", tree_ok(root));

    /* --- 删除根下的中间节点：兄弟链 prev/next 都要修 --- */
    recent = root;
    XIJI_CHECK("exp07-sibling-setup",
               create("f1") == 0 && create("f2") == 0 && create("f3") == 0);
    /* root: docs, f1, f2, f3 */
    XIJI_CHECK("exp07-sibling-count", count_children(root) == 4);
    XIJI_CHECK("exp07-sibling-del", del_file("f2") == 0);
    XIJI_CHECK("exp07-sibling-middle", count_children(root) == 3);
    XIJI_CHECK("exp07-sibling-prev", lookup_child(root, "f3") &&
                                         lookup_child(root, "f3")->prev ==
                                             lookup_child(root, "f1"));
    XIJI_CHECK("exp07-sibling-tree", tree_ok(root));

    /* --- 节点回收：反复创建/删除不应耗尽池子 --- */
    recent = root;
    for (int round = 0; round < 40; round++) {
        if (create("tmp") != 0)
            return xiji_fail("exp07-reclaim", "create failed: pool leak");
        if (del_file("tmp") != 0)
            return xiji_fail("exp07-reclaim", "delete failed");
    }
    XIJI_CHECK("exp07-reclaim-tree", tree_ok(root));

    /* --- 序列化：目录树可以完整打印出来，供 ATA 回读比对 --- */
    recent = root;
    XIJI_CHECK("exp07-format", xiji_exp07_format(tree, (int)sizeof tree) > 0);
    XIJI_CHECK("exp07-format-root", format_contains(tree, "/\n"));
    XIJI_CHECK("exp07-format-dir", format_contains(tree, "  d docs"));
    XIJI_CHECK("exp07-format-file", format_contains(tree, "f f1"));
    XIJI_CHECK("exp07-format-short-buf", xiji_exp07_format(buf, 4) == -1);

    xiji_puts("PASS exp07-file-management\n");
    return 0;
}
